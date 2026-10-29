#!/usr/bin/env bash
# 一条命令启动「照片级预览」测试环境：avatar sidecar + 应用窗口。
# 不启动本地 LLM/TTS sidecar；预览只用固定样音。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
GABTWIN_HOME="${GABTWIN_HOME:-$ROOT/workspace}"
export GABTWIN_HOME
LOG="$GABTWIN_HOME/logs/app.log"
PID_FILE="$GABTWIN_HOME/gabtwin-app.pid"
BIN="$ROOT/target/debug/gabtwin-app"

if [[ ! -x "$BIN" ]]; then
  echo "缺少 $BIN；先 cargo build -p gabtwin-app" >&2
  exit 1
fi
if pgrep -f "$BIN" >/dev/null 2>&1; then
  echo "GabTwin 已在运行；先 ./scripts/gabtwin-stop.sh --all"
  exit 1
fi

"$ROOT/scripts/avatar-sidecar-start.sh"

mkdir -p "$(dirname "$LOG")"
nohup env GABTWIN_TTS=local GABTWIN_LLM=deepseek GABTWIN_AVATAR=1 GABTWIN_AVATAR_LEAD_MS=900 GABTWIN_TTS_OVERLAP=0 GABTWIN_TTS_FIRST_CAP=16 "$BIN" >>"$LOG" 2>&1 &
pid=$!
echo "$pid" >"$PID_FILE"
echo "GabTwin 启动 pid=${pid}，等窗口…"
sleep 2
echo
echo "现在点应用窗口底部的「照片级预览」。"
echo "如果按钮暂时没出现：等 3–5 秒（sidecar 连上后才会显示）。"
echo "停止：./scripts/avatar-sidecar-stop.sh && ./scripts/gabtwin-stop.sh"
