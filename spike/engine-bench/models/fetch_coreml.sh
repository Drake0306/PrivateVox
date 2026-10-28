#!/usr/bin/env bash
# Downloads the Core ML encoder for large-v3-turbo and names it so whisper.cpp finds it next to the q5_0 model.
# whisper.cpp looks for "<model basename without .bin>-encoder.mlmodelc" beside the model file.
set -euo pipefail
DIR="${LVW_SPIKE_MODELS:-$HOME/.cache/lvw-spike/models}"
cd "$DIR"
ZIP=ggml-large-v3-turbo-encoder.mlmodelc.zip
[ -d ggml-large-v3-turbo-q5_0-encoder.mlmodelc ] && { echo "ok   coreml encoder present"; exit 0; }
[ -f "$ZIP" ] || curl -sSL -C - --retry 3 -o "$ZIP" "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$ZIP"
unzip -q -o "$ZIP"
mv ggml-large-v3-turbo-encoder.mlmodelc ggml-large-v3-turbo-q5_0-encoder.mlmodelc
rm -f "$ZIP"
ls -la "$DIR"
