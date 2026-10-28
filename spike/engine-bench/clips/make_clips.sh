#!/usr/bin/env bash
# Generates the three benchmark clips (16 kHz mono 16-bit WAV) from the scripts with macOS text-to-speech.
set -euo pipefail
cd "$(dirname "$0")"
for name in 30s 10s short; do
  say -v Samantha -r 175 -o "tmp_$name.aiff" -f "script_$name.txt"
  afconvert -f WAVE -d LEI16@16000 -c 1 "tmp_$name.aiff" "clip_$name.wav"
  rm -f "tmp_$name.aiff"
  printf '%-10s ' "clip_$name.wav"; afinfo "clip_$name.wav" | grep "estimated duration" | sed 's/^ *//'
done
