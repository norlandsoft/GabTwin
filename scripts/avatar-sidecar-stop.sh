#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GABTWIN_HOME="${GABTWIN_HOME:-$ROOT/workspace}"
PID_FILE="$GABTWIN_HOME/avatar-sidecar.pid"
if [[ -f "$PID_FILE" ]]; then
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    echo "停止 avatar sidecar pid=$pid"
  fi
  rm -f "$PID_FILE"
else
  echo "没有 avatar sidecar pid 文件"
fi
