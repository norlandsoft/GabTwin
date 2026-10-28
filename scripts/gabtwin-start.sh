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

if [[ -f "$ROOT/.env" ]]; then
  set -a
  . "$ROOT/.env"
  set +a
fi
GABTWIN_HOME="${GABTWIN_HOME:-$ROOT/workspace}"
export GABTWIN_HOME
export GABTWIN_MODELS_DIR="$ROOT/models"
export GABTWIN_TTS="${GABTWIN_TTS:-qwen3tts}"
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
  LLM_DIR="${GABTWIN_LLM_DIR:-$GABTWIN_MODELS_DIR/llm/qwen3.5-9b-abliterated-4bit}"
  if [[ -d "$LLM_DIR" && -x "$ROOT/.venv-mlx/bin/python" ]]; then
    if ! curl -fsS --max-time 2 http://127.0.0.1:8080/v1/models >/dev/null 2>&1; then
      echo "启动本地 LLM（$(basename "$LLM_DIR")）…"
      nohup "$ROOT/.venv-mlx/bin/mlx_lm.server" \
        --model "$LLM_DIR" \
        --host 127.0.0.1 --port 8080 \
        --chat-template-args '{"enable_thinking": false}'  >> "$LOG_DIR/llm-server.log" 2>&1 &
      echo $! > "$GABTWIN_HOME/llm-server.pid"
      for _ in $(seq 1 180); do
        curl -fsS --max-time 2 http://127.0.0.1:8080/v1/models >/dev/null 2>&1 && break
        sleep 1
      done
    fi
    export GABTWIN_LLM_BASE="http://127.0.0.1:8080/v1"
    export GABTWIN_LLM_KEY="${GABTWIN_LLM_KEY:-local}"
    export GABTWIN_LLM_MODEL="$LLM_DIR"
    curl -s --max-time 120 http://127.0.0.1:8080/v1/chat/completions \
      -H 'Content-Type: application/json' \
      -d "{\"model\":\"$LLM_DIR\",\"messages\":[{\"role\":\"user\",\"content\":\"你好\"}],\"max_tokens\":1}" >/dev/null 2>&1 || true
    echo "LLM：本地 $(basename "$LLM_DIR")（127.0.0.1:8080）"
  else
    echo "提示：本地 LLM 模型缺失，回退 .env 的 LLM 配置"
  fi
fi

# 3. TTS 引擎：默认 Breeze sidecar；GABTWIN_TTS=indextts 切 IndexTTS；local 回退本地
if [[ "${GABTWIN_TTS:-breeze}" == "indextts" ]]; then
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
elif [[ "${GABTWIN_TTS:-qwen3tts}" == "qwen3tts" ]]; then
  QWEN_TTS_DIR="$GABTWIN_MODELS_DIR/tts/qwen3-tts-1.7b-base-8bit"
  if [[ -x "$ROOT/.venv-mlx/bin/python" && -d "$QWEN_TTS_DIR" && -f "$GABTWIN_HOME/tts/gyy-ref.wav" ]]; then
    if ! curl -fsS --max-time 2 http://127.0.0.1:8800/health >/dev/null 2>&1; then
      echo "启动 Qwen3-TTS 1.7B sidecar…"
      nohup "$ROOT/.venv-mlx/bin/python" "$ROOT/scripts/qwen3_tts_sidecar.py" \
        --model "$QWEN_TTS_DIR" \
        --ref "$GABTWIN_HOME/tts/gyy-ref.wav" \
        --ref-text "思维方式的后遗症就是你你你会很珍惜当下这个东西。但是其实你走过来，你会发现每一刻都挺值得珍惜的。" \
        --port 8800 >> "$LOG_DIR/tts-sidecar.log" 2>&1 &
      echo $! > "$GABTWIN_HOME/tts-sidecar.pid"
      for _ in $(seq 1 180); do
        curl -fsS --max-time 2 http://127.0.0.1:8800/health >/dev/null 2>&1 && break
        sleep 1
      done
    fi
    export GABTWIN_TTS_URL="http://127.0.0.1:8800"
    echo "TTS：Qwen3-TTS 1.7B（127.0.0.1:8800）"
  else
    echo "提示：Qwen3-TTS 模型/参考音缺失，回退本地 TTS"
  fi
elif [[ "${GABTWIN_TTS:-qwen3tts}" == "breeze" ]]; then
  if [[ -x "$ROOT/.venv-mlx/bin/python" && -d "$GABTWIN_MODELS_DIR/tts/breeze-tts2-4bit" && -f "$GABTWIN_HOME/tts/index-ref.wav" ]]; then
    if ! curl -fsS --max-time 2 http://127.0.0.1:8799/health >/dev/null 2>&1; then
      echo "启动 Breeze TTS sidecar…"
      nohup "$ROOT/.venv-mlx/bin/python" "$ROOT/scripts/breeze_tts_sidecar.py" \
        --model "$GABTWIN_MODELS_DIR/tts/breeze-tts2-4bit" \
        --ref "$GABTWIN_HOME/tts/index-ref.wav" \
        --ref-text "今天早上我起得有点晚，差一点就错过地铁了。" \
        --port 8799 >> "$LOG_DIR/tts-sidecar.log" 2>&1 &
      echo $! > "$GABTWIN_HOME/tts-sidecar.pid"
      for _ in $(seq 1 180); do
        curl -fsS --max-time 2 http://127.0.0.1:8799/health >/dev/null 2>&1 && break
        sleep 1
      done
    fi
    export GABTWIN_TTS_URL="http://127.0.0.1:8799"
    echo "TTS：Breeze（127.0.0.1:8799）"
  else
    echo "提示：Breeze 模型/参考音缺失，回退本地 TTS"
  fi
else
  echo "TTS：本地引擎（GABTWIN_TTS=${GABTWIN_TTS:-breeze}）"
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
