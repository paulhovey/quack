#!/bin/sh
# Run `quack eval` for several models, each with several profiles, models in parallel.
#
# Usage: scripts/eval-matrix.sh <provider/model>:<profile>[,<profile>...] ...
#   e.g. scripts/eval-matrix.sh charm-hyper/glm-5.3:generic,cc-style charm-hyper/kimi-k3:generic,kimi-code
# Extra `quack eval` arguments go in EVAL_ARGS, e.g. EVAL_ARGS="--repeat 3 --tasks fix-typo,needle".
# Every task calls the model: this costs money on a paid key. Export the provider's key first
# (`set -a; . ./.env; set +a`). Logs go to a temp folder; results to evals/results/<date>/.
# Prints the comparison table (scripts/eval-table.py) for today's results at the end.
set -eu

[ "$#" -gt 0 ] || { sed -n '2,8p' "$0"; exit 2; }
cargo build --locked -p quack-cli
tmp=$(mktemp -d)
# A copy, so a rebuild while the runs go on doesn't swap the binary under them.
cp target/debug/quack "$tmp/quack"
export QUACK_DEFAULTS_DIR="${QUACK_DEFAULTS_DIR:-defaults}"

for spec in "$@"; do
    model=${spec%%:*}
    profiles=$(echo "${spec#*:}" | tr ',' ' ')
    (
        for p in $profiles; do
            log="$tmp/$(echo "$model" | tr '/' '_')-$p.txt"
            # shellcheck disable=SC2086 # EVAL_ARGS is a list of arguments
            "$tmp/quack" eval -m "$model" --profile "$p" ${EVAL_ARGS:-} >"$log" 2>&1 ||
                echo "$model $p: exit $? (see $log)" >&2
            tail -n 3 "$log" | head -n 1
        done
    ) &
done
wait
echo "logs: $tmp"
python3 scripts/eval-table.py "evals/results/$(date -u +%Y-%m-%d)"
