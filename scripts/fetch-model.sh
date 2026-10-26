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
#   ./scripts/fetch-model.sh                  # 下载默认模型（paraformer，237 MB）
#   ./scripts/fetch-model.sh zipformer-zh     # 中文大模型（167 MB）
#   ./scripts/fetch-model.sh small-ctc        # 中文 small-CTC（26 MB，省 CPU）
#   ./scripts/fetch-model.sh bilingual        # 中英双语（198 MB）
#   ./scripts/fetch-model.sh multilingual     # 九语含中英（340 MB）
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
MODELS="zipformer-zh small-ctc bilingual multilingual paraformer sensevoice tts kokoro-zh"

repo_of() {
  case "$1" in
    zipformer-zh) echo "csukuangfj/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30" ;;
    small-ctc) echo "csukuangfj/sherpa-onnx-streaming-zipformer-small-ctc-zh-int8-2025-04-01" ;;
    bilingual) echo "csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20" ;;
    multilingual) echo "csukuangfj/sherpa-onnx-streaming-zipformer-ar_en_id_ja_ru_th_vi_zh-2025-02-10" ;;
    paraformer) echo "csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en" ;;
    sensevoice) echo "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17" ;;
    tts) echo "csukuangfj/sherpa-onnx-vits-zh-ll" ;;
    kokoro-zh) echo "csukuangfj/kokoro-int8-multi-lang-v1_1" ;;
    *) return 1 ;;
  esac
}

dir_of() {
  case "$1" in
    zipformer-zh) echo "streaming-zipformer-zh-int8-2025-06-30" ;;
    small-ctc) echo "streaming-zipformer-small-ctc-zh-int8-2025-04-01" ;;
    bilingual) echo "streaming-zipformer-bilingual-zh-en-2023-02-20" ;;
    multilingual) echo "streaming-zipformer-ar_en_id_ja_ru_th_vi_zh-2025-02-10" ;;
    paraformer) echo "streaming-paraformer-bilingual-zh-en" ;;
    sensevoice) echo "sense-voice-zh-en-ja-ko-yue-2024-07-17" ;;
    tts) echo "vits-zh-ll" ;;
    kokoro-zh) echo "kokoro-int8-multi-lang-v1_1" ;;
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
    bilingual)
      cat <<'EOF'
encoder-epoch-99-avg-1.int8.onnx:8fa764187a261844f859d7143ebaa563af5d10adfece4c18a8f414c88cba2a9b
decoder-epoch-99-avg-1.int8.onnx:1a70c593d71e53f023f5f55b0b4cfff5055abb786ee3992e5f63dc2e273cc4fa
joiner-epoch-99-avg-1.int8.onnx:1ed689c5ed19dbaa725d9d191bb4822b5f4855a39e1ffd28cbc1f340d25b2ee0
tokens.txt:a8e0e4ec53810e433789b54a5c0134a7eaa2ffca595a6334d54c00da858841d3
bpe.model:bcae393dbc5611be5ffa4c7ae0841558978a5a4f484008cb9dff3a2cc97ebe01
EOF
      ;;
    kokoro-zh)
      cat <<'EOF'
model.int8.onnx:05d793700b0b507f760c183c0e39bdd6a97b43d32fdc7bf7b54a5179f1b11222
voices.bin:e64a5a581d8c2a350d848f51c3121657cd83aa07ed6109172177345874a7244c
tokens.txt:931ab2df2400cd65d580a22402024c2347ced8ae9ea300e545144b1aacc48e14
lexicon-zh.txt:fea060803db3e59193987f1a3436f56917bda09f3b3595b6588e46ae60cebf93
lexicon-us-en.txt:7daaab53a181be9885b853a8582bf1838186317e5dadacbcef9c426d6fa0da14
date-zh.fst:eb8aa079ae3cb81d8f4404992f39d61a0cb990947512b5b8d1e54d1f6980e718
number-zh.fst:743f402181fcfebf76cc2f0546b71fa26476e626fbe4e460fb7b4c3a7a8bd5bd
phone-zh.fst:1ac2b6fa56b1442320c4de7db08353bab8963a2b57f365eebcdd3a2d3562f8d7
EOF
      ;;
    tts)
      cat <<'EOF'
model.onnx:6c349bdd73dc928234dd7bc86929748bba32cd5264d32d915bf7b7aa0595965b
lexicon.txt:b3a82f16b286c424953dea3686039e7ab465fa8e15d87ef8abd0ec69175beb21
tokens.txt:34b035b9aeb070df6188b022f29c00e0e142c7ade9f25611ced65db5e9cc8402
date.fst:eb8aa079ae3cb81d8f4404992f39d61a0cb990947512b5b8d1e54d1f6980e718
number.fst:743f402181fcfebf76cc2f0546b71fa26476e626fbe4e460fb7b4c3a7a8bd5bd
phone.fst:1ac2b6fa56b1442320c4de7db08353bab8963a2b57f365eebcdd3a2d3562f8d7
new_heteronym.fst:ca14b2127e27baa571664e4bb791e143e7425f56a6bc29db08d74f97e6aa4e29
EOF
      ;;
    sensevoice)
      cat <<'EOF'
model.int8.onnx:c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51
tokens.txt:f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc
EOF
      ;;
    paraformer)
      cat <<'EOF'
encoder.int8.onnx:81a70226a8934e6ed92aa1d4fc486b428b5398e2f2619ed4897b7294cab90e9a
decoder.int8.onnx:f3cca9f77bb9d93c8fcbfb63ae617b6b1ee96818df3aa3b151c40658fe38594f
tokens.txt:59aba8873a2ed1e122c25fee421e25f283b63290efbde85c1f01a853d83cb6e6
EOF
      ;;
    multilingual)
      cat <<'EOF'
encoder-epoch-75-avg-11-chunk-16-left-128.int8.onnx:f9001ed7a9e46d0294438c1a30cd7c72d1cc4bdd4e7880edbcda36f67081e32e
decoder-epoch-75-avg-11-chunk-16-left-128.onnx:7ebc63f34b21c8efb4a41a5a2eee7fe1448829ce0230ecc5369e67fc14d90d48
joiner-epoch-75-avg-11-chunk-16-left-128.int8.onnx:db88e3172323551abaa99b91b18fb422a27ea4a834fd0db10389f9478816f917
tokens.txt:784f24950f6bcce1b0021035632dd60fd4617ecd8ca0581ab57d7b39d77ba5ab
bpe.model:027731f33cff7266f2878c6fb7e478cf4af983962e311bb565112792794c13cd
EOF
      ;;
  esac
}

wavs_of() {
  case "$1" in
    zipformer-zh) echo "0.wav 1.wav" ;;
    multilingual) echo "zh.wav en.wav" ;;
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

  # 所有模型共用一个示例音频，供应用里的「示例音频」按钮使用
  mkdir -p "$GABTWIN_HOME/samples"
  if [[ ! -s "$GABTWIN_HOME/samples/0.wav" ]]; then
    curl -fL --noproxy '*' -A "$UA" --retry 2 --retry-delay 3 -sS \
      -o "$GABTWIN_HOME/samples/0.wav" \
      "$MIRROR/csukuangfj/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30/resolve/main/test_wavs/0.wav?download=true" \
      || echo "  ! 示例音频下载失败（不影响识别）" >&2
  fi

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
    all|zipformer-zh|small-ctc|bilingual|multilingual|paraformer|sensevoice|tts|kokoro-zh) TARGET="$1"; shift ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$TARGET" ]]; then
  # 没参数时下"当前默认组合"：流式 paraformer + 句末 SenseVoice + TTS
  for m in paraformer sensevoice tts; do fetch_model "$m"; done
  echo
  echo "完成。校验：./scripts/fetch-model.sh --check"
  exit 0
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
