#!/usr/bin/env bash
# Build .deb (and, where rpmbuild is installed, .rpm) packages for one Linux target from
# an already built binary.
#
#   scripts/package-linux.sh <target-triple> <version> [path/to/quack] [out-dir]
#
# Two packages, which conflict with each other, as with the archives:
#   quack          /usr/bin/quack and the defaults bundle in /usr/share/quack/defaults
#   quack-plucked  the same, without the provider files for outside services
# The binary finds the bundle at <exe>/../share/quack/defaults, so no configuration is
# needed. Release builds are static (musl), so the packages depend on nothing.
set -euo pipefail

target=${1:?usage: package-linux.sh <target-triple> <version> [binary] [out-dir]}
version=${2:?usage: package-linux.sh <target-triple> <version> [binary] [out-dir]}
version=${version#v}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=${3:-$root/target/$target/release/quack}
out=${4:-$root/dist}
[ -f "$bin" ] || { echo "package-linux.sh: no binary at $bin" >&2; exit 1; }
case $target in
  x86_64-*) deb_arch=amd64 rpm_arch=x86_64 ;;
  aarch64-*) deb_arch=arm64 rpm_arch=aarch64 ;;
  *) echo "package-linux.sh: no package architecture for $target" >&2; exit 1 ;;
esac

mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

repo=https://github.com/paulhovey/quack
summary="Terminal coding agent for open-weight models"
describe() {
  case $1 in
    quack) echo "The quack binary with its defaults bundle (providers, profiles, MCP and language server files)." ;;
    quack-plucked) echo "The quack binary and its defaults bundle without the hosted model services; for setups that add their own provider." ;;
  esac
}

# The installed files of one flavour under $1.
lay_out() {
  local dir=$1 flavour=$2
  install -Dm755 "$bin" "$dir/usr/bin/quack"
  install -Dm644 "$root/LICENSE" "$dir/usr/share/doc/$flavour/copyright"
  install -Dm644 "$root/NOTICE" "$dir/usr/share/doc/$flavour/NOTICE"
  if [ "$flavour" = quack ]; then
    "$root/scripts/copy-defaults.sh" "$dir/usr/share/quack/defaults"
  else
    "$root/scripts/copy-defaults.sh" "$dir/usr/share/quack/defaults" --plucked
  fi
}

for flavour in quack quack-plucked; do
  other=$([ "$flavour" = quack ] && echo quack-plucked || echo quack)
  dir=$stage/deb-$flavour
  lay_out "$dir" "$flavour"
  mkdir -p "$dir/DEBIAN"
  size=$(du -sk "$dir/usr" | cut -f1)
  cat > "$dir/DEBIAN/control" <<EOF
Package: $flavour
Version: $version
Architecture: $deb_arch
Maintainer: Paul Hovey <paul@paulhovey.org>
Installed-Size: $size
Conflicts: $other
Replaces: $other
Section: devel
Priority: optional
Homepage: $repo
Description: $summary
 $(describe "$flavour")
EOF
  dpkg-deb --root-owner-group --build "$dir" "$out/${flavour}_${version}_${deb_arch}.deb" >/dev/null
  echo "wrote $out/${flavour}_${version}_${deb_arch}.deb"

  if command -v rpmbuild >/dev/null; then
    rpmroot=$stage/rpm-$flavour
    mkdir -p "$rpmroot"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
    lay_out "$rpmroot/files" "$flavour"
    files=$(cd "$rpmroot/files" && find . -type f -o -type l | sed 's|^\.||' | sort)
    {
      echo "Name: $flavour"
      echo "Version: ${version//-/_}"
      echo "Release: 1"
      echo "Summary: $summary"
      echo "License: Apache-2.0"
      echo "URL: $repo"
      echo "Conflicts: $other"
      echo "AutoReqProv: no"
      echo
      echo "%description"
      describe "$flavour"
      echo
      echo "%install"
      echo "cp -a $rpmroot/files/. %{buildroot}/"
      echo
      echo "%files"
      echo "$files"
    } > "$rpmroot/SPECS/$flavour.spec"
    rpmbuild --quiet -bb --target "$rpm_arch" --define "_topdir $rpmroot" \
      --define "_build_id_links none" "$rpmroot/SPECS/$flavour.spec"
    cp "$rpmroot"/RPMS/*/*.rpm "$out/"
    echo "wrote $out/$(basename "$rpmroot"/RPMS/*/*.rpm)"
  fi
done
