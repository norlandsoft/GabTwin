# dev/06 · Breeze TTS 2（本地 MLX）

> 状态：**已在本机跑通**（2026-10-26）。4-bit MLX，中文克隆，RTF 0.83–1.10。
> 许可证：**Breezeblue 研究/非商用**，商用需单独授权。

## 为什么能用

`mlx-audio` 0.6.1 内置了 `BreezeTTS` 实现（模型卡本身没写用法，容易误判为"只有权重"）。
它加载 `mlx-community/Breeze-TTS-2-mlx-4bit`，24 kHz 单声道输出。

## 安装

```bash
/opt/homebrew/bin/python3.12 -m venv .venv-mlx
.venv-mlx/bin/pip install -U mlx-audio   # 0.6.1
```

模型（HF，国内直连 hf-mirror；hf-xet 会卡，用分片下载）：

```text
models/breeze-tts2-4bit/
  model.safetensors            2.33 GB
  audio_tokenizer/model.safetensors 682 MB
  tokenizer.json / config.json / ...
```

## 用法

```bash
./scripts/test-breeze.sh
# 或手动：
.venv-mlx/bin/mlx_audio.tts.generate \
  --model models/breeze-tts2-4bit --lang_code zh \
  --ref_audio /tmp/ref-fish.wav --ref_text "参考音逐字文字" \
  --text "要合成的话" --output_path /tmp --file_prefix breeze --verbose
```

**中文必须加 `--lang_code zh`**，否则 CLI 默认按 `en` 处理。

## 实测（M1 Max，4-bit）

| 文本长度 | 音频 | 生成耗时 | RTF | 峰值内存 |
|---|---|---|---|---|
| 短句（3.84 s） | 3.84 s | 4.24 s | 1.10 | 5.8 GB |
| 长句（10.0 s） | 10.0 s | 8.34 s | **0.83** | 6.9 GB |

- 首次运行含编译，约 25 s；之后单次 CLI 含模型加载 ~11 s（加载 3–4 s）；
- 24 kHz；克隆参考 15 s（talk.mp3 3.8–18.8 s，已降噪）。

## 和 ZipVoice 的取舍

| | Breeze 2 (4-bit MLX) | ZipVoice (int8 CPU) |
|---|---|---|
| 音质/表现力 | 更强（开源权重榜首） | 一般 |
| 中文克隆 | 支持（需 ref 文本） | 支持 |
| RTF | 0.83–1.10 | 0.6–1.1 |
| 内存 | 6–7 GB | ~1 GB |
| 首音（整句） | 3–5 s | 1–3 s |
| 许可 | **非商用** | Apache-2.0（模型待核） |

Breeze 的短板是**首音**：句子级合成要等整句生成完。要用在实时对话里，需要
`--stream` 流式输出 + 边收边播；已有 `xzf-thu/BreezeTTS2_Mac_Streaming` 在做
Apple Silicon 的流式优化（RTF ~4 → ~1）。

## 下一步（如果音质接受）

1. 写一个 MLX sidecar（加载一次模型，HTTP/管道返回 WAV 或流式 PCM）；
2. 应用侧按句送文本，边收边播，把首音压到 1–2 s；
3. 对比 8-bit（4.6 GB）看音质是否值得；
4. **许可证**：非商用，正式产品需先解决授权。
