#!/usr/bin/env bash
# Downloads the four spike models into ~/.cache/lvw-spike/models (about 3.4 GB).
# Each file is verified against the server's Content-Length.
set -euo pipefail
DIR="${LVW_SPIKE_MODELS:-$HOME/.cache/lvw-spike/models}"
mkdir -p "$DIR"
fetch() { # url, file
  local url="$1" file="$DIR/$2"
  local want; want="$(curl -sIL "$url" | tr -d '\r' | awk 'tolower($1)=="content-length:"{v=$2} END{print v}')"
  if [ -f "$file" ] && [ -n "$want" ] && [ "$(stat -f %z "$file")" -eq "$want" ]; then echo "ok   $2 ($want bytes)"; return; fi
  echo "get  $2 ($want bytes)"; curl -sSL -C - --retry 3 -o "$file" "$url"
  local got; got="$(stat -f %z "$file")"
  [ -z "$want" ] || [ "$got" -eq "$want" ] || { echo "size mismatch for $2: got $got want $want" >&2; exit 1; }
  echo "done $2"
}
HF=https://huggingface.co
fetch "$HF/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin"                             ggml-large-v3-turbo-q5_0.bin
fetch "$HF/handy-computer/whisper-large-v3-turbo-gguf/resolve/main/whisper-large-v3-turbo-Q5_K_M.gguf"  whisper-large-v3-turbo-Q5_K_M.gguf
fetch "$HF/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin"                                   ggml-large-v3-q5_0.bin
fetch "$HF/handy-computer/whisper-large-v3-gguf/resolve/main/whisper-large-v3-Q5_K_M.gguf"              whisper-large-v3-Q5_K_M.gguf
ls -la "$DIR"
