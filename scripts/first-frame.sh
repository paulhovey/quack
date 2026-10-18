#!/bin/sh
# First-frame budget (docs/plan/13-testing-and-evals.md#performance-budgets-ci-enforced).
#
# Usage: scripts/first-frame.sh [budget_ms] [runs]   (defaults: 50 ms, 10 runs)
# Runs the release binary in a pseudo-terminal with the defaults bundle and an isolated
# config, using the hidden `--trace-startup` flag, and fails if the median is over budget.
# Needs `script` from util-linux (Linux).
set -eu

budget=${1:-50}
runs=${2:-10}
cargo build --release --locked -p cutoffharness-cli
bin="$PWD/target/release/cutoffharness"
tmp=$(mktemp -d)
mkdir -p "$tmp/ws"

i=0
while [ "$i" -lt "$runs" ]; do
    i=$((i + 1))
    CUTOFFHARNESS_CONFIG_DIR="$tmp/user" CUTOFFHARNESS_DATA_DIR="$tmp/data" \
    CUTOFFHARNESS_CACHE_DIR="$tmp/cache" CUTOFFHARNESS_SYSTEM_DIR="$tmp/system" \
    CUTOFFHARNESS_DEFAULTS_DIR="$PWD/defaults" \
        script -qec "stty cols 120 rows 40; '$bin' -C '$tmp/ws' --trace-startup '$tmp/trace-$i.json'" /dev/null >/dev/null
done

cat "$tmp"/trace-*.json | awk -v budget="$budget" -F'"first_frame_ms":' '
    { split($2, a, "}"); v[NR] = a[1] + 0 }
    END {
        n = NR
        for (i = 1; i <= n; i++) for (j = i + 1; j <= n; j++) if (v[j] < v[i]) { t = v[i]; v[i] = v[j]; v[j] = t }
        median = (n % 2) ? v[(n + 1) / 2] : (v[n / 2] + v[n / 2 + 1]) / 2
        printf "first frame: median %.1f ms over %d runs (min %.1f, max %.1f), budget %d ms\n", median, n, v[1], v[n], budget
        if (n == 0 || median > budget) exit 1
    }'
