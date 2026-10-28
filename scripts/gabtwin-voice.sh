#!/usr/bin/env bash
# 切换克隆音色：./scripts/gabtwin-voice.sh [list|talk|talk2]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -f "$ROOT/.env" ]]; then
  set -a
  . "$ROOT/.env"
  set +a
fi
HOME_DIR="${GABTWIN_HOME:-$ROOT/workspace}"
export GABTWIN_HOME="$HOME_DIR"
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
if [[ "$name" == "off" ]]; then
  [[ -f "$TARGET/reference.wav" ]] && mv "$TARGET/reference.wav" "$TARGET/reference.wav.disabled"
  [[ -f "$TARGET/reference.txt" ]] && mv "$TARGET/reference.txt" "$TARGET/reference.txt.disabled"
  echo "已停用参考音：应用将回退 Matcha 内置音色（重启生效）"
  exit 0
fi
if [[ ! -f "$VOICES/$name/reference.wav" ]]; then
  echo "没有这个音色：${name}" >&2; list >&2; exit 1
fi
cp "$VOICES/$name/reference.wav" "$TARGET/reference.wav"
cp "$VOICES/$name/reference.txt" "$TARGET/reference.txt"
echo "已切换到音色：${name}（重启应用生效）"
