#!/usr/bin/env bash
# Build quack and register it as an agent in Harbor and Pier benchmark harnesses.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

echo "=== Building / Installing quack ==="
cd "$repo_root"

# The adapter uploads this binary into each task's container, whose libc can be older
# than the host's, so the static musl build comes first.
if [ -f target/x86_64-unknown-linux-musl/release/quack ]; then
  echo "Using existing pre-built musl quack binary..."
  mkdir -p target/release
  cp target/x86_64-unknown-linux-musl/release/quack target/release/quack
elif [ -f target/release/quack ]; then
  echo "Using existing pre-built quack binary (glibc; it may not run in older task images)..."
else
  if ! command -v cc >/dev/null; then
    echo "Installing build-essential..."
    apt-get update -qq && apt-get install -y -qq build-essential
  fi

  if ! command -v cargo >/dev/null; then
    echo "Installing minimal Rust toolchain..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.99.1 --profile minimal
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
    export PATH="$HOME/.cargo/bin:/root/.cargo/bin:$PATH"
  fi

  cargo build --release --locked -p quack-cli
fi

mkdir -p /usr/local/bin /usr/local/share/quack
cp target/release/quack /usr/local/bin/quack
chmod +x /usr/local/bin/quack
cp -r defaults /usr/local/share/quack/defaults

/usr/local/bin/quack --version

echo "=== Registering quack in Harbor and Pier ==="
cp "$script_dir/quack_adapter.py" /usr/local/share/quack/quack_adapter.py

python3 - << 'PYEOF'
import glob
import os
import shutil

adapter_src = "/usr/local/share/quack/quack_adapter.py"
dirs = set()

for p in glob.glob("/root/.local/share/uv/tools/*/lib/python*/site-packages"):
    dirs.add(p)
for p in glob.glob(os.path.expanduser("~/.local/share/uv/tools/*/lib/python*/site-packages")):
    dirs.add(p)

try:
    import site
    for s in site.getsitepackages():
        dirs.add(s)
except Exception:
    pass

for d in dirs:
    if os.path.isdir(d):
        dest = os.path.join(d, "quack_adapter.py")
        shutil.copyfile(adapter_src, dest)
        pth = os.path.join(d, "register_quack.pth")
        with open(pth, "w") as f:
            f.write("import quack_adapter; quack_adapter.register()\n")
        print(f"Registered quack in {d}")
PYEOF

echo "=== Registration complete ==="
