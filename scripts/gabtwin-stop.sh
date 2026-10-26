#!/usr/bin/env bash
# 停止 GabTwin。
#
#   ./scripts/gabtwin-stop.sh
#
# 默认只停「由 gabtwin-start.sh 启动」的那个实例（PID 文件里记录的那个）。
# 手动启动的实例需要显式 --all，避免误杀同一台机器上另一份正在用的实例。
# 先 TERM，5 秒内没退出再 KILL。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GABTWIN_HOME="${GABTWIN_HOME:-$HOME/Library/Application Support/GabTwin}"
BIN="$ROOT/target/debug/gabtwin-app"
PID_FILE="$GABTWIN_HOME/gabtwin-app.pid"
LOG="$GABTWIN_HOME/logs/app.log"

MODE=pid
case "${1:-}" in
  --all) MODE=all ;;
  "") ;;
  *) echo "用法：$0 [--all]" >&2; exit 2 ;;
esac

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

stop_sidecar() {
  local pid_file="$GABTWIN_HOME/tts-sidecar.pid"
  [[ -f "$pid_file" ]] || return 0
  local pid
  pid="$(cat "$pid_file" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    echo "停止 TTS sidecar（pid ${pid}）…"
    kill "$pid" 2>/dev/null || true
  fi
  rm -f "$pid_file"
}

stop_sidecar

log_marker() {
  local dir
  dir="$(dirname "$LOG")"
  mkdir -p "$dir"
  echo "[$(date '+%H:%M:%S')] gabtwin-stop.sh：$1" >>"$LOG" 2>/dev/null || true
}

stop_pid() {
  local pid="$1"
  echo "停止（pid ${pid}）…"
  kill "$pid" 2>/dev/null || true
  local i
  for i in $(seq 1 50); do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "已停止。"
      return 0
    fi
    sleep 0.1
  done
  echo "5 秒内没有退出，强制结束。"
  kill -9 "$pid" 2>/dev/null || true
  echo "已强制停止。"
}

found=0

# 1. 首选 PID 文件：这是本脚本启动的实例，默认只停它
if [[ -f "$PID_FILE" ]]; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if is_our_pid "${pid:-}"; then
    log_marker "停止 pid=${pid}"
    stop_pid "$pid"
    found=1
  else
    echo "PID 文件已失效，清理。"
  fi
  rm -f "$PID_FILE"
fi

# 2. 没有 PID 记录时：默认不动别人的实例
if [[ "$found" -eq 0 ]]; then
  pids="$(pids_of_app | tr '\n' ' ')"
  if [[ -z "${pids// /}" ]]; then
    echo "GabTwin 没有在运行。"
    stop_sidecar
exit 0
  fi
  if [[ "$MODE" != all ]]; then
    echo "发现实例（pid ${pids% }），但它不是本脚本启动的（没有 PID 文件），未动它。"
    echo "确认要停，用：./scripts/gabtwin-stop.sh --all"
    exit 0
  fi
  for pid in $pids; do
    log_marker "停止 pid=${pid}（--all）"
    stop_pid "$pid"
  done
fi

exit 0
