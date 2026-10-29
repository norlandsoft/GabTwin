#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GABTWIN_HOME="${GABTWIN_HOME:-$ROOT/workspace}"
PIDFILE="$GABTWIN_HOME/gfpgan-restore.pid"
LOG="$GABTWIN_HOME/logs/gfpgan-restore.log"
mkdir -p "$GABTWIN_HOME/logs"
if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  echo "GFPGAN restore 已在运行：pid $(cat "$PIDFILE")"
  exit 0
fi
nohup env GABTWIN_HOME="$GABTWIN_HOME" \
  "$ROOT/.venv-cosy/bin/python" "$ROOT/scripts/gfpgan_restore_server.py" \
  >>"$LOG" 2>&1 &
pid=$!
echo "$pid" >"$PIDFILE"
echo "启动 GFPGAN restore pid=${pid}，等待模型…"
for _ in $(seq 1 180); do
  if curl -fsS --noproxy '*' --max-time 1 http://127.0.0.1:8812/health >/dev/null 2>&1; then
    echo "就绪：http://127.0.0.1:8812（日志 $LOG）"
    exit 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "启动失败，看 $LOG" >&2
    tail -20 "$LOG" >&2
    exit 1
  fi
  sleep 1
done
echo "等待超时，看 $LOG" >&2
exit 1
