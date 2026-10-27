#!/usr/bin/env bash
# 查看模型占用：models/asr、models/tts、models/llm
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
echo "模型目录：$ROOT/models"
for kind in asr tts llm; do
  echo
  echo "[$kind]"
  du -sh "models/$kind"/* 2>/dev/null | sort -h || echo "  (空)"
done
echo
echo -n "总计："
du -sh models | awk '{print $1}'
