#!/bin/sh
# Start a local model server on CPU in a container and run the scripted tool-call turn
# against it (docs/plan/13-testing-and-evals.md#ci).
#
# Usage: scripts/local-server-ci.sh <llama-cpp|ollama>
# Env:   DOCKER (default docker; podman works), MODELS_DIR (default ~/.cache/cutoffharness-ci),
#        CUTOFFHARNESS_BIN (default target/debug/cutoffharness, built if missing)
#
# The model is the newest small Qwen at the time of writing: tiny enough for a CPU runner,
# and it uses the same chat template and tool-call format as the larger Qwen models.
set -eu

LLAMA_IMAGE=ghcr.io/ggml-org/llama.cpp:server-b11718
LLAMA_MODEL_URL=https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf
OLLAMA_IMAGE=docker.io/ollama/ollama:0.40.6
OLLAMA_MODEL=qwen3.5:0.8b

kind=$1
docker=${DOCKER:-docker}
models=${MODELS_DIR:-$HOME/.cache/cutoffharness-ci}
name="cutoffharness-ci-$kind"
mkdir -p "$models"

if [ ! -x "${CUTOFFHARNESS_BIN:-target/debug/cutoffharness}" ]; then
    cargo build --locked -p cutoffharness-cli
fi

wait_for() {
    i=0
    until curl -sf "$1" >/dev/null; do
        i=$((i + 1))
        if [ "$i" -gt 180 ]; then
            echo "server did not come up: $1" >&2
            "$docker" logs "$name" 2>&1 | tail -n 40 >&2
            exit 1
        fi
        sleep 1
    done
}

"$docker" rm -f "$name" >/dev/null 2>&1 || true
trap '"$docker" rm -f "$name" >/dev/null 2>&1 || true' EXIT

case $kind in
    llama-cpp)
        file="$models/$(basename "$LLAMA_MODEL_URL")"
        [ -s "$file" ] || curl -fL --retry 3 -o "$file" "$LLAMA_MODEL_URL"
        "$docker" run -d --name "$name" -p 8080:8080 -v "$models:/models:ro" "$LLAMA_IMAGE" \
            -m "/models/$(basename "$file")" --jinja -c 16384 --host 0.0.0.0 --port 8080 >/dev/null
        wait_for http://127.0.0.1:8080/health
        url=http://127.0.0.1:8080
        model=
        ;;
    ollama)
        mkdir -p "$models/ollama"
        "$docker" run -d --name "$name" -p 11434:11434 -v "$models/ollama:/root/.ollama" \
            "$OLLAMA_IMAGE" >/dev/null
        wait_for http://127.0.0.1:11434/api/version
        "$docker" exec "$name" ollama pull "$OLLAMA_MODEL" >/dev/null 2>&1
        url=http://127.0.0.1:11434
        model=$OLLAMA_MODEL
        ;;
    *)
        echo "unknown server kind: $kind" >&2
        exit 2
        ;;
esac

if ! scripts/local-server-turn.sh "$kind" "$url" "$model"; then
    "$docker" logs "$name" 2>&1 | tail -n 40 >&2
    exit 1
fi
