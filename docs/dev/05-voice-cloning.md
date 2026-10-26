# dev/05 · 本地自定义音色（ZipVoice 零样本克隆）

> 状态：**链路已通**（2026-10-26）。用一段参考录音克隆音色，本地推理，24 kHz。
> 首音约 2.1 s、RTF 0.89（2 步）——比 Matcha 慢，但音色是"你自己的"。

## 为什么是 ZipVoice

- sherpa-onnx 自带，Rust 直接调，**不需要 Python**；
- 零样本：给 5–15 s 参考音频 + 对应文字即可，不用训练；
- 中英混读，24 kHz 声码器（vocos）。

## 模型

```text
~/Library/Application Support/GabTwin/models/zipvoice-zh-en/
  encoder.int8.onnx     5.3 MB
  decoder.int8.onnx     119 MB
  tokens.txt / lexicon.txt / espeak-ng-data/
  vocos_24khz.onnx      54 MB（声码器）
  test_wavs/            官方样音（leijun-1 / news-female-*）
```

GitHub 主站被墙时用 gh-proxy 下载：

```bash
curl -L -o zipvoice.tar.bz2 \
  "https://gh-proxy.com/https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-zipvoice-distill-int8-zh-en-emilia.tar.bz2"
curl -L -o vocos_24khz.onnx \
  "https://gh-proxy.com/https://github.com/k2-fsa/sherpa-onnx/releases/download/vocoder-models/vocos_24khz.onnx"
```

（tar 解压后放进上面的目录，声码器也放同一层。）

## 当前状态（本机）

- 音色：`talk-dense-clean`（talk.mp3 第 3.8–8.8 s，5 秒密集语音，已降噪）；
- 参考音：`$GABTWIN_HOME/tts/reference.wav`，文字在同目录 `reference.txt`；
- 有参考音时应用**自动优先用 ZipVoice**，否则回退 Matcha；
- 实测首音 **1.16 s**（负载正常时）、长句 RTF 0.5–0.8；
  ComfyUI 等重负载会把首音推到 3–6 s，**速度对负载非常敏感**。

### 参考音降噪

手机/视频录音的环境噪声会被一起克隆。先做高通 + FFT 降噪 + 响度归一：

```bash
ffmpeg -ss 3.8 -t 5 -i talk.wav \
  -af "highpass=f=80,afftdn=nf=-25,loudnorm=I=-18:TP=-2" \
  -ac 1 -ar 24000 reference.wav
```

降噪后 RTF 从 1.6 回到 0.74（同负载下），杂音也明显减少。

切换音色：

```bash
./scripts/gabtwin-voice.sh list
./scripts/gabtwin-voice.sh talk        # talk.mp3 开头 6 秒
./scripts/gabtwin-voice.sh talk2       # talk2.mov 开头 6 秒
./scripts/gabtwin-voice.sh talk-dense  # talk.mp3 3.8–8.8 秒（未降噪）
./scripts/gabtwin-voice.sh talk-dense-clean   # 降噪版（当前）
./scripts/gabtwin-voice.sh talk2-dense-clean  # talk2 降噪版
```

## 用法

```bash
export GABTWIN_TTS_MODEL_DIR="$HOME/Library/Application Support/GabTwin/models/zipvoice-zh-en"
export GABTWIN_TTS_REF_WAV="/path/to/reference.wav"
export GABTWIN_TTS_REF_TEXT="参考录音里说的那句话"
./scripts/gabtwin-start.sh
```

也可以把参考音放在 `$GABTWIN_HOME/tts/reference.wav` 和 `reference.txt`，
不设环境变量也会自动用。

`GABTWIN_TTS_STEPS`：流匹配步数，默认 2（RTF≈0.89）；4 步质量更好但 RTF≈1.9，
会卡顿。

单测：

```bash
cargo run -p gabtwin-probe -- --tts "$GABTWIN_TTS_MODEL_DIR" \
  --reference-wav "$GABTWIN_TTS_REF_WAV" --reference-text "$GABTWIN_TTS_REF_TEXT" \
  --text "你好，我是你的数字伙伴。" --out /tmp/clone.wav
```

## 实测（M1 Max，步骤 2）

| 指标 | 值 |
|---|---|
| 首音（应用内，首句 7 字） | 2.1 s |
| 首句合成 | 1.9 s（固定开销为主） |
| 长句 RTF | 0.89 |
| 4 步 RTF | 1.87（不实时） |

**固定开销**：每句都要重新编码一次参考音，所以短句合成不划算；分句器已经按句子切，
必要时可以把相邻短句合并到 20–30 字再合成。

## 参考录音要求

- 5–15 s，单人，安静环境，无音乐/混响；
- 采样率 ≥16 kHz，WAV/M4A/MP3 均可（应用会转成单声道）；
- 参考文字要和音频**逐字对应**（可以用 SenseVoice 转写后人工校正）。

## 已知限制

- 2 步的音质/韵律不如 4 步，需要你听感验收；
- 首音 2.1 s，比 Matcha 的 0.28 s 慢一个量级；
- 每个角色一套参考音 + 文字，UI 还没做（先用环境变量）；
- 克隆音色的相似度取决于参考音质量；正式产品建议 1–5 分钟数据 + GPT-SoVITS 微调
  （推理更快、更像），ZipVoice 适合先跑通体验。
