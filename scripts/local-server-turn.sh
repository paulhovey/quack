#!/bin/sh
# Scripted tool-call turn against a local model server (docs/plan/13-testing-and-evals.md#ci).
#
# Usage: scripts/local-server-turn.sh <llama-cpp|ollama> <base_url> [model] [attempts]
#
# Without a model the provider file lists none, so the loaded model is discovered (the
# shipped llama-cpp.toml works this way).
#
# Puts a code number in a file and asks the model to read it and reply with it. Passes
# when the model called `read` and the answer contains the number. Tiny CPU models are not
# reliable, so a failed attempt is retried (default 3 attempts); every attempt's events are
# kept for debugging. The provider file is written the way a user would write it.
set -eu

kind=$1
base_url=$2
model=${3:-}
attempts=${4:-3}
bin=${QUACK_BIN:-target/debug/quack}
word=73914

tmp=$(mktemp -d)
mkdir -p "$tmp/user/providers.d" "$tmp/ws/notes"
printf 'The code is %s.\n' "$word" > "$tmp/ws/notes/code.txt"
{
    case $kind in
        llama-cpp) printf 'base_url = "%s/v1"\n' "$base_url" ;;
        # The qwen3.5 Modelfile sets presence_penalty = 1.5, which pushes a tiny model away
        # from repeating the number it just read, so it answers "1" or the file name.
        ollama) printf 'base_url = "%s"\napi = "ollama"\nextra_body = { options = { presence_penalty = 0.0 } }\n' "$base_url" ;;
        *) echo "unknown server kind: $kind" >&2; exit 2 ;;
    esac
    if [ -n "$model" ]; then printf 'models = ["%s"]\n' "$model"; fi
} > "$tmp/user/providers.d/local.toml"

export QUACK_CONFIG_DIR="$tmp/user" QUACK_DATA_DIR="$tmp/data"
export QUACK_CACHE_DIR="$tmp/cache" QUACK_SYSTEM_DIR="$tmp/system"
export QUACK_DEFAULTS_DIR="$PWD/defaults"

prompt="Use the read tool to open notes/code.txt, then reply with only the code number."
n=0
while [ "$n" -lt "$attempts" ]; do
    n=$((n + 1))
    out="$tmp/attempt-$n.jsonl"
    started=$(date +%s)
    "$bin" -C "$tmp/ws" exec --output-format stream-json "$prompt" > "$out" 2> "$tmp/attempt-$n.err" || true
    took=$(( $(date +%s) - started ))
    called=$(grep -c '"event":"tool_call_begin".*"name":"read"' "$out" || true)
    answered=$(grep '"event":"assistant_delta"' "$out" | sed 's/.*"text":"\([^"]*\)".*/\1/' | tr -d '\n')
    echo "attempt $n (${took}s): read calls=$called, answer: $(printf '%s' "$answered" | cut -c1-200)"
    if [ "$called" -gt 0 ] && printf '%s' "$answered" | grep -q "$word"; then
        echo "ok: $kind $model"
        exit 0
    fi
    sed 's/^/  stderr: /' "$tmp/attempt-$n.err" | tail -n 5
done
echo "failed after $attempts attempts; events in $tmp" >&2
exit 1
