#!/usr/bin/env bash
# 启动 GabTwin（后台运行，带 PID 与日志）。
#
#   ./scripts/gabtwin-start.sh
#   GABTWIN_AUTOREPLAY=1 ./scripts/gabtwin-start.sh   # 自动回放示例，不用麦克风
#   GABTWIN_DEVICE="MacBook Pro麦克风" ./scripts/gabtwin-start.sh
#
# 环境变量原样传给应用（GABTWIN_MODEL_DIR / GABTWIN_DEVICE / GABTWIN_ENDPOINT /
# GABTWIN_AUTOSTART / GABTWIN_AUTOREPLAY）。
#
# 日志：$GABTWIN_HOME/logs/app.log
# 停止：./scripts/gabtwin-stop.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

GABTWIN_HOME="${GABTWIN_HOME:-$HOME/Library/Application Support/GabTwin}"
MODEL_DIR="${GABTWIN_MODEL_DIR:-$GABTWIN_HOME/models/streaming-zipformer-zh-int8-2025-06-30}"
BIN="$ROOT/target/debug/gabtwin-app"
PID_FILE="$GABTWIN_HOME/gabtwin-app.pid"
LOG_DIR="$GABTWIN_HOME/logs"
LOG="$LOG_DIR/app.log"

is_our_pid() {
  [[ -n "${1:-}" ]] && kill -0 "$1" 2>/dev/null && ps -p "$1" -o command= 2>/dev/null | grep -q "gabtwin-app"
}

# 按可执行文件名找实例（兼容 ./target/... 这种相对路径启动）
pids_of_app() {
  ps -axo pid=,comm= | awk -v want="gabtwin-app" '{
    pid = $1
    comm = $0
    sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "", comm)
    name = comm
    sub(/.*\//, "", name)
    if (name == want) print pid
  }'
}

mkdir -p "$LOG_DIR"

# 1. 已经在跑就不再起第二个（包括手动启动、没有 PID 记录的实例）
if [[ -f "$PID_FILE" ]]; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if is_our_pid "${pid:-}"; then
    echo "GabTwin 已在运行（pid ${pid}）。"
    echo "日志：$LOG"
    echo "停止：./scripts/gabtwin-stop.sh"
    exit 0
  fi
  rm -f "$PID_FILE"
fi
existing="$(pids_of_app | head -1)"
if [[ -n "$existing" ]]; then
  echo "GabTwin 已在运行（pid ${existing}，不是本脚本启动的）。"
  echo "停止：./scripts/gabtwin-stop.sh"
  exit 0
fi

# 2. 模型检查（app 里也有检查，这里提前给出可执行的下一步）
if [[ ! -f "$MODEL_DIR/tokens.txt" ]] ||
  { [[ ! -f "$MODEL_DIR/encoder.int8.onnx" ]] && [[ ! -f "$MODEL_DIR/model.int8.onnx" ]]; }; then
  echo "模型缺失：$MODEL_DIR" >&2
  echo "先运行：./scripts/fetch-model.sh" >&2
  exit 2
fi

# 3. 编译（已是最新时很快）
echo "编译应用…"
if ! cargo build -p gabtwin-app --quiet; then
  echo "编译失败。" >&2
  exit 1
fi

# 4. 后台启动
echo "启动 GabTwin…"
nohup "$BIN" >>"$LOG" 2>&1 &
pid=$!
echo "$pid" >"$PID_FILE"

# 5. 起来后确认没立刻退出（模型缺失、权限问题会在日志里）
sleep 1
if ! is_our_pid "$pid"; then
  echo "启动失败，最近日志：" >&2
  tail -20 "$LOG" >&2
  rm -f "$PID_FILE"
  exit 1
fi

echo "已启动（pid ${pid}）"
echo "日志：$LOG"
echo "停止：./scripts/gabtwin-stop.sh"
