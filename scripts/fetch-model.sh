#!/usr/bin/env bash
# GabTwin 模型下载器。
#
# 规则（2026-10-26 起）：
#   - 默认走国内镜像 hf-mirror.com；
#   - 强制直连：清掉代理环境变量，curl 再加 --noproxy '*'；
#   - 并行下载（默认 4 路），每个文件独立重试与断点续传；
#   - 每个文件校验 sha256，校验不过就重下，不把坏文件留给识别器。
#
# 用法：
#   ./scripts/fetch-model.sh                  # 下载全部
#   ./scripts/fetch-model.sh zipformer-zh     # 只下大模型（167 MB）
#   ./scripts/fetch-model.sh small-ctc        # 只下 small-CTC（26 MB）
#   ./scripts/fetch-model.sh --list           # 看有哪些
#   ./scripts/fetch-model.sh --check          # 只校验已下载的文件
#
# 环境变量：
#   GABTWIN_HOME         模型根目录，默认 ~/Library/Application Support/GabTwin
#   GABTWIN_HF_MIRROR    镜像地址，默认 https://hf-mirror.com
#   GABTWIN_FETCH_JOBS   并发数，默认 4

set -euo pipefail

# 禁止代理：子进程继承的代理变量一并清掉，curl 再显式 --noproxy。
unset http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY
export NO_PROXY='*' no_proxy='*'

GABTWIN_HOME="${GABTWIN_HOME:-$HOME/Library/Application Support/GabTwin}"
MIRROR="${GABTWIN_HF_MIRROR:-https://hf-mirror.com}"
JOBS="${GABTWIN_FETCH_JOBS:-4}"
UA='gabtwin/0.1 (local, direct)'
MODELS="zipformer-zh small-ctc"

repo_of() {
  case "$1" in
    zipformer-zh) echo "csukuangfj/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30" ;;
    small-ctc) echo "csukuangfj/sherpa-onnx-streaming-zipformer-small-ctc-zh-int8-2025-04-01" ;;
    *) return 1 ;;
  esac
}

dir_of() {
  case "$1" in
    zipformer-zh) echo "streaming-zipformer-zh-int8-2025-06-30" ;;
    small-ctc) echo "streaming-zipformer-small-ctc-zh-int8-2025-04-01" ;;
  esac
}

# 文件名:sha256
files_of() {
  case "$1" in
    zipformer-zh)
      cat <<'EOF'
encoder.int8.onnx:5ac51e27981bb4dab01bb9be4958453ba50c3b61c063ddda0eab23fd3671aa4f
decoder.onnx:06522ad63cec0fdf6809f4e1db9bb4f7d710c34582e3b35db62ac60eccafac7e
joiner.int8.onnx:b34584dc6f561089e1d747fedebb3765f2caa72c927ef54d7ca55e5ae40a814b
tokens.txt:6193c7ea1c96d0d9a1e9652789b40d13a8a913b434a5451e93158f5a09fd6652
EOF
      ;;
    small-ctc)
      cat <<'EOF'
model.int8.onnx:68c9c943840f7d9cf3e8a4970ba50f404feb5277f611fa82b7e72267786fa84a
bbpe.model:503204e0690eff065e30d0e01898c9ab06d0e6dc376a741eb6846198f95b2f82
tokens.txt:6fed8c6c248516f38e7faa19404b57413e8ce259f1cbc1fa4aebc86eac32fdfd
EOF
      ;;
  esac
}

wavs_of() {
  case "$1" in
    zipformer-zh) echo "0.wav 1.wav" ;;
    *) echo "" ;;
  esac
}

sha() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

download_one() {
  local name="$1" want="$2" repo="$3" dest="$4"
  local url="$MIRROR/$repo/resolve/main/$name?download=true"
  local part="$dest.part"
  local attempt=1

  if [[ -f "$dest" ]]; then
    if [[ "$(sha "$dest")" == "$want" ]]; then
      printf '  ✓ %-20s 已就绪 %s\n' "$name" "$(du -h "$dest" | cut -f1 | tr -d ' ')"
      return 0
    fi
    rm -f "$dest"
  fi

  while (( attempt <= 3 )); do
    local started
    started=$(date +%s)
    if curl -fL --noproxy '*' -A "$UA" --retry 2 --retry-delay 3 \
      -C - -sS -o "$part" "$url"; then
      if [[ "$(sha "$part")" == "$want" ]]; then
        mv "$part" "$dest"
        local secs=$(( $(date +%s) - started ))
        (( secs < 1 )) && secs=1
        printf '  ✓ %-20s %s  %ss\n' "$name" "$(du -h "$dest" | cut -f1 | tr -d ' ')" "$secs"
        return 0
      fi
      printf '  ! %s 校验失败，重下（%d/3）\n' "$name" "$attempt" >&2
      rm -f "$part"
    else
      printf '  ! %s 下载失败，重试（%d/3）\n' "$name" "$attempt" >&2
      sleep $(( attempt * 3 ))
    fi
    attempt=$((attempt + 1))
  done
  return 1
}

fetch_model() {
  local model="$1"
  local repo dir
  repo="$(repo_of "$model")" || { echo "未知模型：$model" >&2; return 1; }
  dir="$GABTWIN_HOME/models/$(dir_of "$model")"

  mkdir -p "$dir/test_wavs"
  echo
  echo "模型 $model"
  echo "  目录 $dir"
  echo "  镜像 ${MIRROR}（直连，不走代理；${JOBS} 路并发）"

  local pids=() name want fail=0
  while IFS=: read -r name want; do
    [[ -z "${name:-}" ]] && continue
    while [[ "$(jobs -rp | wc -l | tr -d ' ')" -ge "$JOBS" ]]; do
      sleep 0.2
    done
    download_one "$name" "$want" "$repo" "$dir/$name" &
    pids+=("$!")
  done < <(files_of "$model")

  local pid
  for pid in "${pids[@]}"; do
    wait "$pid" || fail=1
  done

  local wav
  for wav in $(wavs_of "$model"); do
    if [[ -s "$dir/test_wavs/$wav" ]]; then
      continue
    fi
    curl -fL --noproxy '*' -A "$UA" --retry 2 --retry-delay 3 -sS \
      -o "$dir/test_wavs/$wav" "$MIRROR/$repo/resolve/main/test_wavs/$wav?download=true" \
      || echo "  ! 测试音频 $wav 下载失败（不影响识别）" >&2
  done

  if (( fail != 0 )); then
    echo "  ✗ $model 有文件没下成功" >&2
    return 1
  fi
  return 0
}

check_model() {
  local model="$1" repo dir name want bad=0
  repo="$(repo_of "$model")" || return 1
  dir="$GABTWIN_HOME/models/$(dir_of "$model")"
  echo
  echo "校验 $model"
  while IFS=: read -r name want; do
    [[ -z "${name:-}" ]] && continue
    if [[ ! -f "$dir/$name" ]]; then
      printf '  ✗ %-20s 缺失\n' "$name"; bad=1
    elif [[ "$(sha "$dir/$name")" != "$want" ]]; then
      printf '  ✗ %-20s 哈希不符\n' "$name"; bad=1
    else
      printf '  ✓ %-20s\n' "$name"
    fi
  done < <(files_of "$model")
  return $bad
}

usage() {
  sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
}

MODE=fetch
TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --list) echo "可用模型：$MODELS"; exit 0 ;;
    --check) MODE=check; shift ;;
    --jobs)
      JOBS="${2:?--jobs 需要一个数字}"
      [[ "$JOBS" =~ ^[0-9]+$ ]] || { echo "--jobs 需要数字" >&2; exit 2; }
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    all|zipformer-zh|small-ctc) TARGET="$1"; shift ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$TARGET" ]]; then
  TARGET=all
fi

if [[ "$MODE" == check ]]; then
  rc=0
  for m in $MODELS; do check_model "$m" || rc=1; done
  exit $rc
fi

if [[ "$TARGET" == all ]]; then
  for m in $MODELS; do fetch_model "$m"; done
else
  fetch_model "$TARGET"
fi

echo
echo "完成。校验：./scripts/fetch-model.sh --check"
echo "试跑：cargo run -p gabtwin-probe -- --model-dir \"$GABTWIN_HOME/models/$(dir_of "$TARGET" 2>/dev/null || echo streaming-zipformer-zh-int8-2025-06-30)\" --wav \"$GABTWIN_HOME/models/streaming-zipformer-zh-int8-2025-06-30/test_wavs/0.wav\""
