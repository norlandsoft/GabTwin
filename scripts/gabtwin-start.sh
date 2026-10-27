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
export GABTWIN_MODELS_DIR="$ROOT/models"
MODEL_DIR="${GABTWIN_MODEL_DIR:-$GABTWIN_MODELS_DIR/asr/streaming-paraformer-bilingual-zh-en}"
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

# 2. 模型检查（app 里也有检查，这里提前给出可执行的下一步）
shopt -s nullglob
onnx_files=("$MODEL_DIR"/encoder*.onnx "$MODEL_DIR"/model*.onnx)
if [[ ! -f "$MODEL_DIR/tokens.txt" ]] || [[ ${#onnx_files[@]} -eq 0 ]]; then
  echo "模型缺失：$MODEL_DIR" >&2
  echo "先运行：./scripts/fetch-model.sh" >&2
  exit 2
fi

if [[ -f "$ROOT/.env" ]]; then
  perm="$(stat -f %Lp "$ROOT/.env" 2>/dev/null || echo 600)"
  if (( (8#$perm) & 077 )); then
    echo "提示：.env 权限是 ${perm}，建议 chmod 600 .env（里面是 key）"
  fi
fi

OFFLINE_DIR="$GABTWIN_MODELS_DIR/asr/sense-voice-zh-en-ja-ko-yue-2024-07-17"
if [[ ! -d "$OFFLINE_DIR" ]]; then
  echo "提示：未找到 SenseVoice 定稿模型，句末会用流式结果（./scripts/fetch-model.sh sensevoice）"
fi
TTS_DIR="$GABTWIN_MODELS_DIR/tts/matcha-zh-en"
if [[ ! -d "$TTS_DIR" ]]; then
  echo "提示：未找到 TTS 模型，回复只会显示文字（需要 matcha-zh-en；见 docs/dev/04-tts.md）"
fi

# 3. 编译（已是最新时很快）
echo "编译应用…"
if ! cargo build -p gabtwin-app --quiet; then
  echo "编译失败。" >&2
  exit 1
fi

# 3. LLM：默认本地 mlx-lm server（GABTWIN_LLM=deepseek 走云端）
if [[ "${GABTWIN_LLM:-local}" == "local" ]]; then
  if [[ -d "$GABTWIN_MODELS_DIR/llm/qwen2.5-7b-4bit" && -x "$ROOT/.venv-mlx/bin/python" ]]; then
    if ! curl -fsS --max-time 2 http://127.0.0.1:8080/v1/models >/dev/null 2>&1; then
      echo "启动本地 LLM（Qwen2.5-7B-MLX）…"
      nohup "$ROOT/.venv-mlx/bin/mlx_lm.server" \
        --model "$GABTWIN_MODELS_DIR/llm/qwen2.5-7b-4bit" \
        --host 127.0.0.1 --port 8080 >> "$LOG_DIR/llm-server.log" 2>&1 &
      echo $! > "$GABTWIN_HOME/llm-server.pid"
      for _ in $(seq 1 180); do
        curl -fsS --max-time 2 http://127.0.0.1:8080/v1/models >/dev/null 2>&1 && break
        sleep 1
      done
    fi
    export GABTWIN_LLM_BASE="http://127.0.0.1:8080/v1"
    export GABTWIN_LLM_KEY="${GABTWIN_LLM_KEY:-local}"
    export GABTWIN_LLM_MODEL="$GABTWIN_MODELS_DIR/llm/qwen2.5-7b-4bit"
    curl -s --max-time 120 http://127.0.0.1:8080/v1/chat/completions \
      -H 'Content-Type: application/json' \
      -d "{\"model\":\"$GABTWIN_MODELS_DIR/llm/qwen2.5-7b-4bit\",\"messages\":[{\"role\":\"user\",\"content\":\"你好\"}],\"max_tokens\":1}" >/dev/null 2>&1 || true
    echo "LLM：本地 Qwen2.5-7B（127.0.0.1:8080）"
  else
    echo "提示：本地 LLM 模型缺失，回退 .env 的 LLM 配置"
  fi
fi

# 3. TTS 引擎：默认 IndexTTS sidecar（GABTWIN_TTS=local 回退本地）
if [[ "${GABTWIN_TTS:-indextts}" == "indextts" ]]; then
  if [[ -x "$ROOT/.venv-mlx/bin/python" && -d "$GABTWIN_MODELS_DIR/tts/index-tts-2.5-mlx" && -f "$GABTWIN_HOME/tts/index-ref.wav" ]]; then
    if ! curl -fsS --max-time 2 http://127.0.0.1:8798/health >/dev/null 2>&1; then
      echo "启动 IndexTTS sidecar…"
      nohup "$ROOT/.venv-mlx/bin/python" "$ROOT/scripts/index_tts_sidecar.py" \
        --model-dir "$GABTWIN_MODELS_DIR/tts/index-tts-2.5-mlx" \
        --ref "$GABTWIN_HOME/tts/index-ref.wav" \
        --port 8798 >> "$LOG_DIR/tts-sidecar.log" 2>&1 &
      echo $! > "$GABTWIN_HOME/tts-sidecar.pid"
      for _ in $(seq 1 120); do
        curl -fsS --max-time 2 http://127.0.0.1:8798/health >/dev/null 2>&1 && break
        sleep 1
      done
    fi
    export GABTWIN_TTS_URL="http://127.0.0.1:8798"
    echo "TTS：IndexTTS（127.0.0.1:8798）"
  else
    echo "提示：IndexTTS 运行时/模型/参考音缺失，回退本地 TTS"
  fi
fi

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

# 4. 后台启动
echo "启动 GabTwin…"
printf '[%s] gabtwin-start.sh：启动 %s\n' "$(date '+%H:%M:%S')" "$BIN" >>"$LOG"
nohup "$BIN" >>"$LOG" 2>&1 &
pid=$!
echo "$pid" >"$PID_FILE"
printf '[%s] gabtwin-start.sh：pid=%s\n' "$(date '+%H:%M:%S')" "$pid" >>"$LOG"

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
