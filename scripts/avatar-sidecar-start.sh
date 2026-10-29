#!/usr/bin/env bash
# 启动照片级数字人 sidecar（MuseTalk 1.5 MLX，端口 8811）。
# 只在本机跑；模型/venv/frames 都在 gitignore 里。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
GABTWIN_HOME="${GABTWIN_HOME:-$ROOT/workspace}"
PORT="${GABTWIN_AVATAR_PORT:-8811}"
PID_FILE="$GABTWIN_HOME/avatar-sidecar.pid"
LOG="$GABTWIN_HOME/logs/avatar-sidecar.log"

if curl -fsS --noproxy '*' --max-time 2 "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
  echo "avatar sidecar 已在运行：http://127.0.0.1:${PORT}"
  exit 0
fi

if [[ ! -x "$ROOT/.venv-avatar/bin/python" ]]; then
  echo "缺少 .venv-avatar；先按 docs/08 安装 MLX + musetalk-mlx" >&2
  exit 2
fi
if [[ ! -d "$ROOT/models/avatar/musetalk-1.5-q8" ]]; then
  echo "缺少 models/avatar/musetalk-1.5-q8" >&2
  exit 2
fi
if [[ ! -f "$GABTWIN_HOME/avatar/preview.wav" ]]; then
  echo "缺少 $GABTWIN_HOME/avatar/preview.wav（3 秒预览音频）" >&2
  exit 2
fi

mkdir -p "$(dirname "$LOG")"
unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY
export NO_PROXY='*' no_proxy='*'

nohup "$ROOT/.venv-avatar/bin/python" "$ROOT/scripts/avatar_sidecar.py" \
  --model "$ROOT/models/avatar/musetalk-1.5-q8" \
  --frames "$GABTWIN_HOME/avatar/frames3s" \
  --audio "$GABTWIN_HOME/avatar/preview.wav" \
  --port "$PORT" >>"$LOG" 2>&1 &
pid=$!
echo "$pid" >"$PID_FILE"
echo "启动 avatar sidecar pid=${pid}，等待模型…"
for _ in $(seq 1 90); do
  if curl -fsS --noproxy '*' --max-time 2 "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
    echo "就绪：http://127.0.0.1:${PORT}"
    echo "日志：$LOG"
    exit 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "启动失败，最近日志：" >&2
    tail -20 "$LOG" >&2
    rm -f "$PID_FILE"
    exit 1
  fi
  sleep 1
done
echo "启动超时，见 $LOG" >&2
exit 1
