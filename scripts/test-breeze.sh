#!/usr/bin/env bash
# Breeze TTS 2（MLX 4-bit）中文克隆冒烟测试
# 用法：./scripts/test-breeze.sh [模型目录] [参考音] [文本]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODEL="${1:-$ROOT/models/breeze-tts2-4bit}"
REF="${2:-/tmp/ref-fish.wav}"
TEXT="${3:-你好，我是你的数字伙伴。最近在忙什么呢？}"
REF_TEXT="收到那俄罗斯人给我的打款了，我就给了他支付宝账号，他支付宝里有钱。嗯，但是我也不知道他是国外的银行的钱呢，还是支付宝的这个。"

"$ROOT/.venv-mlx/bin/mlx_audio.tts.generate" \
  --model "$MODEL" \
  --lang_code zh \
  --ref_audio "$REF" \
  --ref_text "$REF_TEXT" \
  --text "$TEXT" \
  --output_path /tmp \
  --file_prefix breeze \
  --verbose
