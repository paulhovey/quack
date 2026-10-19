#!/usr/bin/env bash
# Build the release archives for one target from an already built binary.
#
#   scripts/package.sh <target-triple> [path/to/quack[.exe]] [out-dir]
#
# Writes two archives to out-dir (default dist/):
#   quack-<target>.tar.gz          the binary and the defaults bundle (what most people want)
#   quack-plucked-<target>.tar.gz  the binary only, for updating it without touching defaults
# Windows targets get .zip instead.
#
# Unix layout is a prefix (bin/quack, share/quack/defaults/) so it can be unpacked
# straight into ~/.local or /usr/local. Windows is flat (quack.exe, defaults\), matching
# where the binary looks for its bundle.
set -euo pipefail

target=${1:?usage: package.sh <target-triple> [binary] [out-dir]}
root=$(cd "$(dirname "$0")/.." && pwd)
case $target in
  *windows*) exe=quack.exe ;;
  *) exe=quack ;;
esac
bin=${2:-$root/target/$target/release/$exe}
out=${3:-$root/dist}
[ -f "$bin" ] || { echo "package.sh: no binary at $bin" >&2; exit 1; }

mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# Copies the defaults bundle without editor or OS leftovers.
copy_defaults() {
  mkdir -p "$1"
  (cd "$root/defaults" && tar cf - --exclude='*/.*' --exclude='*~' .) | (cd "$1" && tar xf -)
}

for flavour in quack quack-plucked; do
  dir=$stage/$flavour-$target
  mkdir -p "$dir"
  cp "$root/LICENSE" "$root/NOTICE" "$dir/"
  case $target in
    *windows*)
      cp "$bin" "$dir/$exe"
      [ $flavour = quack ] && copy_defaults "$dir/defaults"
      rm -f "$out/$flavour-$target.zip"
      if command -v 7z >/dev/null; then
        (cd "$stage" && 7z a -tzip -bso0 "$out/$flavour-$target.zip" "$flavour-$target")
      else
        (cd "$stage" && zip -qr "$out/$flavour-$target.zip" "$flavour-$target")
      fi
      ;;
    *)
      mkdir -p "$dir/bin"
      cp "$bin" "$dir/bin/$exe"
      chmod 755 "$dir/bin/$exe"
      [ $flavour = quack ] && copy_defaults "$dir/share/quack/defaults"
      tar -C "$stage" -czf "$out/$flavour-$target.tar.gz" "$flavour-$target"
      ;;
  esac
done

ls -1 "$out" | grep -- "-$target\." | sed "s|^|$out/|"
