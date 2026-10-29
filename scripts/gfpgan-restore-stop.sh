#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIDFILE="${GABTWIN_HOME:-$ROOT/workspace}/gfpgan-restore.pid"
if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  pid=$(cat "$PIDFILE")
  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  echo "停止 GFPGAN restore（pid ${pid}）。"
else
  echo "没有 GFPGAN restore pid 文件。"
fi
rm -f "$PIDFILE"
