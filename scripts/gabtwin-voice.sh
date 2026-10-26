#!/usr/bin/env bash
# 切换克隆音色：./scripts/gabtwin-voice.sh [list|talk|talk2]
set -euo pipefail
HOME_DIR="${GABTWIN_HOME:-$HOME/Library/Application Support/GabTwin}"
VOICES="$HOME_DIR/tts/voices"
TARGET="$HOME_DIR/tts"

list() {
  echo "可用音色："
  for dir in "$VOICES"/*/; do
    [[ -f "$dir/reference.wav" ]] || continue
    printf '  %-8s %s\n' "$(basename "$dir")" "$(head -c 60 "$dir/reference.txt" 2>/dev/null || true)"
  done
}

name="${1:-list}"
if [[ "$name" == "list" ]]; then list; exit 0; fi
if [[ ! -f "$VOICES/$name/reference.wav" ]]; then
  echo "没有这个音色：${name}" >&2; list >&2; exit 1
fi
cp "$VOICES/$name/reference.wav" "$TARGET/reference.wav"
cp "$VOICES/$name/reference.txt" "$TARGET/reference.txt"
echo "已切换到音色：${name}（重启应用生效）"
