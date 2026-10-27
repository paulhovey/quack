#!/bin/sh
# Install quack on Linux or macOS.
#
#   curl -fsSL https://github.com/paulhovey/quack/releases/latest/download/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --plucked
#
# By default this installs the binary and the defaults bundle (provider, model profile,
# language server and MCP example files). --plucked installs the same bundle without the
# provider files for outside services, for organisations that add their own provider.
# --binary-only updates the binary and leaves the installed defaults alone.
set -eu

repo=paulhovey/quack
prefix=${QUACK_INSTALL_PREFIX:-$HOME/.local}
version=latest
plucked=0
binary_only=0
archive=
source_dir=
target=
uninstall=0

usage() {
  cat <<'EOF'
Install quack.

Usage: install.sh [options]

  --plucked            Leave out the provider files for hosted services (OpenRouter,
                       Claude, OpenAI, ...). Profiles, sub-agents, MCP, language servers
                       and the local server files are all installed.
  --binary-only        Install or update only the binary. Existing defaults are left as
                       they are; none are installed if there were none.
  --prefix DIR         Install into DIR/bin and DIR/share/quack (default: ~/.local,
                       or $QUACK_INSTALL_PREFIX).
  --version VER        A release tag such as v0.2.0 (default: the latest release).
  --archive FILE       Install from a downloaded quack-*.tar.gz instead of downloading.
  --from-source [DIR]  Build with cargo from a checkout (default: the one holding this
                       script) and install that.
  --target TRIPLE      Target triple to build for with --from-source (e.g.
                       x86_64-unknown-linux-musl).
  --uninstall          Remove the binary and the defaults from the prefix. Your config,
                       sessions and memory are kept.
  -h, --help           Show this help.

A full install replaces the defaults folder, so files you deleted from it come back.
To turn a default off for good, override it in your own config instead (for example
`enabled = false` in ~/.config/quack/providers.d/<id>.toml), or update with --binary-only.
EOF
}

say() { printf 'quack: %s\n' "$*"; }
die() { printf 'quack: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case $1 in
    --plucked) plucked=1 ;;
    --binary-only) binary_only=1 ;;
    --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; prefix=$2; shift ;;
    --prefix=*) prefix=${1#*=} ;;
    --version) [ $# -ge 2 ] || die "--version needs a tag"; version=$2; shift ;;
    --version=*) version=${1#*=} ;;
    --archive) [ $# -ge 2 ] || die "--archive needs a file"; archive=$2; shift ;;
    --archive=*) archive=${1#*=} ;;
    --from-source)
      if [ $# -ge 2 ] && [ "${2#-}" = "$2" ]; then source_dir=$2; shift
      else source_dir=$(cd "$(dirname "$0")/.." 2>/dev/null && pwd) || die "can't find the checkout"
      fi ;;
    --target) [ $# -ge 2 ] || die "--target needs a triple"; target=$2; shift ;;
    --target=*) target=${1#*=} ;;
    --uninstall) uninstall=1 ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
  shift
done

bindir=$prefix/bin
defaults=$prefix/share/quack/defaults

if [ $uninstall = 1 ]; then
  rm -f "$bindir/quack"
  rm -rf "$prefix/share/quack/defaults"
  rmdir "$prefix/share/quack" 2>/dev/null || true
  say "removed $bindir/quack and $defaults"
  exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

# Where the new files come from: $new_bin and $new_defaults.
new_defaults=
if [ -n "$source_dir" ]; then
  [ -f "$source_dir/Cargo.toml" ] || die "$source_dir is not a quack checkout"
  command -v cargo >/dev/null || die "cargo is needed for --from-source (https://rustup.rs)"
  say "building from $source_dir"
  if [ -n "$target" ]; then
    (cd "$source_dir" && cargo build --release --locked -p quack-cli --target "$target") || die "build failed"
    new_bin=$source_dir/target/$target/release/quack
  else
    (cd "$source_dir" && cargo build --release --locked -p quack-cli) || die "build failed"
    new_bin=$source_dir/target/release/quack
  fi
  new_defaults=$source_dir/defaults
  if [ $plucked = 1 ]; then
    "$source_dir/scripts/copy-defaults.sh" "$tmp/defaults" --plucked || die "copying the defaults failed"
    new_defaults=$tmp/defaults
  fi
else
  if [ -z "$archive" ]; then
    case $(uname -s) in
      Linux) os=unknown-linux-musl ;;
      Darwin) os=apple-darwin ;;
      *) die "unsupported OS $(uname -s); on Windows use install.ps1" ;;
    esac
    case $(uname -m) in
      x86_64 | amd64) arch=x86_64 ;;
      aarch64 | arm64) arch=aarch64 ;;
      *) die "unsupported CPU $(uname -m)" ;;
    esac
    # --plucked fetches the archive whose bundle has no hosted services.
    name=quack-$arch-$os.tar.gz
    [ $plucked = 1 ] && name=quack-plucked-$arch-$os.tar.gz
    if [ "$version" = latest ]; then
      base=https://github.com/$repo/releases/latest/download
    else
      base=https://github.com/$repo/releases/download/$version
    fi
    if command -v curl >/dev/null; then
      fetch() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }
    elif command -v wget >/dev/null; then
      fetch() { wget -q --https-only -O "$2" "$1"; }
    else
      die "curl or wget is needed to download"
    fi
    say "downloading $name ($version)"
    fetch "$base/$name" "$tmp/$name" || die "download failed: $base/$name"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"
    want=$(grep " \*\{0,1\}$name\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
    [ -n "$want" ] || die "$name is not listed in SHA256SUMS"
    if command -v sha256sum >/dev/null; then
      got=$(sha256sum "$tmp/$name" | cut -d' ' -f1)
    else
      got=$(shasum -a 256 "$tmp/$name" | cut -d' ' -f1)
    fi
    [ "$want" = "$got" ] || die "checksum mismatch for $name"
    archive=$tmp/$name
  fi
  [ -f "$archive" ] || die "no such archive: $archive"
  mkdir "$tmp/x"
  tar -xzf "$archive" -C "$tmp/x" 2>/dev/null || die "$archive is not a quack .tar.gz archive"
  top=$(find "$tmp/x" -mindepth 1 -maxdepth 1 -type d | head -n 1)
  [ -n "$top" ] && [ -f "$top/bin/quack" ] || die "$archive does not look like a quack archive"
  new_bin=$top/bin/quack
  [ -d "$top/share/quack/defaults" ] && new_defaults=$top/share/quack/defaults
fi

# The binary: copy beside the old one, then rename over it, so a running quack is
# never left with a half-written file.
mkdir -p "$bindir"
cp "$new_bin" "$bindir/.quack.new"
chmod 755 "$bindir/.quack.new"
mv -f "$bindir/.quack.new" "$bindir/quack"
say "installed $bindir/quack"

# The defaults: replaced as a whole, so files removed upstream disappear too.
if [ $binary_only = 1 ]; then
  if [ -d "$defaults" ]; then
    say "left the defaults in $defaults as they are (--binary-only)"
  else
    say "no defaults installed (--binary-only): add providers with \`quack setup\` or your own files"
  fi
elif [ -z "$new_defaults" ]; then
  say "this archive has no defaults bundle; left $defaults as it is"
else
  mkdir -p "$prefix/share/quack"
  rm -rf "$defaults.new"
  cp -R "$new_defaults" "$defaults.new"
  rm -rf "$defaults"
  mv "$defaults.new" "$defaults"
  say "installed the defaults in $defaults"
fi

case :$PATH: in
  *:"$bindir":*) ;;
  *) say "$bindir is not on your PATH; add it, e.g. export PATH=\"$bindir:\$PATH\"" ;;
esac
if [ $plucked = 1 ]; then
  say "no hosted services are set up (--plucked): add your server with \`quack setup\` or a providers.d file"
else
  say "run \`quack setup\` to pick a model, then \`quack\`"
fi
