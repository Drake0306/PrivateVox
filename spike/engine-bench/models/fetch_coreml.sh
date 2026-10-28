#!/usr/bin/env bash
# Downloads the Core ML encoder for large-v3-turbo next to the model files.
# whisper.cpp strips the quantization suffix and looks for "ggml-large-v3-turbo-encoder.mlmodelc" beside the model.
set -euo pipefail
DIR="${PRIVATEVOX_SPIKE_MODELS:-$HOME/.cache/lvw-spike/models}"
cd "$DIR"
ZIP=ggml-large-v3-turbo-encoder.mlmodelc.zip
[ -d ggml-large-v3-turbo-encoder.mlmodelc ] && { echo "ok   coreml encoder present"; exit 0; }
[ -f "$ZIP" ] || curl -sSL -C - --retry 3 -o "$ZIP" "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$ZIP"
unzip -q -o "$ZIP"
rm -rf __MACOSX
rm -f "$ZIP"
ls -la "$DIR"
