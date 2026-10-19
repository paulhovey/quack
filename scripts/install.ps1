# Install quack on Windows.
#
#   irm https://github.com/paulhovey/quack/releases/latest/download/install.ps1 | iex
#   & ([scriptblock]::Create((irm .../install.ps1))) -Plucked
#
# By default this installs the binary and the defaults bundle (provider, model profile,
# language server and MCP example files). -Plucked installs or updates only the binary and
# leaves any defaults alone: for organisations that ship their own files.
#
# A full install replaces the defaults folder, so files you deleted from it come back.
# To turn a default off for good, override it in your own config instead (for example
# `enabled = false` in %APPDATA%\quack\providers.d\<id>.toml), or update with -Plucked.
[CmdletBinding()]
param(
    # Install or update only the binary; existing defaults are left as they are.
    [switch]$Plucked,
    # Install folder; quack.exe and defaults\ go directly in it.
    [string]$Prefix = $(if ($env:QUACK_INSTALL_PREFIX) { $env:QUACK_INSTALL_PREFIX } else { Join-Path $env:LOCALAPPDATA 'Programs\quack' }),
    # A release tag such as v0.2.0; the latest release by default.
    [string]$Version = 'latest',
    # Install from a downloaded quack-*.zip instead of downloading.
    [string]$Archive,
    # Build with cargo from this checkout and install that.
    [string]$FromSource,
    # Remove the binary and the defaults; config, sessions and memory are kept.
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$repo = 'paulhovey/quack'
function Say($m) { Write-Host "quack: $m" }

$exe = Join-Path $Prefix 'quack.exe'
$defaults = Join-Path $Prefix 'defaults'

function Set-UserPath([string]$dir, [bool]$add) {
    $cur = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($cur -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ne $dir.TrimEnd('\')) })
    if ($add) { $parts += $dir }
    [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
}

if ($Uninstall) {
    Remove-Item -Force -ErrorAction SilentlyContinue $exe, "$exe.old"
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $defaults
    if ((Test-Path $Prefix) -and -not (Get-ChildItem $Prefix)) { Remove-Item $Prefix }
    Set-UserPath $Prefix $false
    Say "removed $exe and $defaults"
    return
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("quack-install-" + [guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $newDefaults = $null
    if ($FromSource) {
        if (-not (Test-Path (Join-Path $FromSource 'Cargo.toml'))) { throw "$FromSource is not a quack checkout" }
        Say "building from $FromSource"
        Push-Location $FromSource
        try { cargo build --release --locked -p quack-cli; if ($LASTEXITCODE) { throw 'build failed' } }
        finally { Pop-Location }
        $newBin = Join-Path $FromSource 'target\release\quack.exe'
        $newDefaults = Join-Path $FromSource 'defaults'
    } else {
        if (-not $Archive) {
            $arch = switch ($env:PROCESSOR_ARCHITECTURE) {
                'AMD64' { 'x86_64' } 'ARM64' { 'aarch64' }
                default { throw "unsupported CPU $env:PROCESSOR_ARCHITECTURE" }
            }
            $flavour = if ($Plucked) { 'quack-plucked' } else { 'quack' }
            $name = "$flavour-$arch-pc-windows-msvc.zip"
            $base = if ($Version -eq 'latest') { "https://github.com/$repo/releases/latest/download" }
                    else { "https://github.com/$repo/releases/download/$Version" }
            Say "downloading $name ($Version)"
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -UseBasicParsing "$base/$name" -OutFile (Join-Path $tmp $name)
            Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')
            $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match "^([0-9a-f]{64}) \*?$([regex]::Escape($name))$" } | Select-Object -First 1
            if (-not $line) { throw "$name is not listed in SHA256SUMS" }
            $want = ($line -split ' ')[0]
            $got = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $name)).Hash.ToLower()
            if ($want -ne $got) { throw "checksum mismatch for $name" }
            $Archive = Join-Path $tmp $name
        }
        $x = Join-Path $tmp 'x'
        Expand-Archive -Path $Archive -DestinationPath $x
        $top = Get-ChildItem -Directory $x | Select-Object -First 1
        if (-not $top -or -not (Test-Path (Join-Path $top.FullName 'quack.exe'))) { throw "$Archive does not look like a quack archive" }
        $newBin = Join-Path $top.FullName 'quack.exe'
        $d = Join-Path $top.FullName 'defaults'
        if (Test-Path $d) { $newDefaults = $d }
    }

    # A running quack.exe can't be overwritten, but it can be renamed out of the way.
    New-Item -ItemType Directory -Force $Prefix | Out-Null
    Remove-Item -Force -ErrorAction SilentlyContinue "$exe.old"
    if (Test-Path $exe) { Move-Item -Force $exe "$exe.old" }
    Copy-Item $newBin $exe
    Remove-Item -Force -ErrorAction SilentlyContinue "$exe.old"
    Say "installed $exe"

    # The defaults: replaced as a whole, so files removed upstream disappear too.
    if ($Plucked) {
        if (Test-Path $defaults) { Say "left the defaults in $defaults as they are (-Plucked)" }
        else { Say 'no defaults installed (-Plucked): add providers with `quack setup` or your own files' }
    } elseif (-not $newDefaults) {
        Say "this archive has no defaults bundle; left $defaults as it is"
    } else {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue "$defaults.new"
        Copy-Item -Recurse $newDefaults "$defaults.new"
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $defaults
        Move-Item "$defaults.new" $defaults
        Say "installed the defaults in $defaults"
    }

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') | Where-Object { $_.TrimEnd('\') -eq $Prefix.TrimEnd('\') })) {
        Set-UserPath $Prefix $true
        Say "added $Prefix to your PATH; open a new terminal to use it"
    }
    Say 'run `quack setup` to pick a model, then `quack`'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmp
}
