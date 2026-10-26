# dev/04 · TTS：把回复说出来

> 状态：**已跑通**（2026-10-26）。本机 VITS，句级流式合成，播放期间半双工。
> 音色质量与英文单词支持是已知短板。

## 结构

```text
LLM 流式 delta
   → ClauseSplitter（首句 ≤6 字，之后按标点/30 字）
   → TTS worker（VITS，sherpa-onnx OfflineTts）
   → 重采样 16k → 48k
   → Speaker（cpal 输出 + 队列）
```

- 首句故意很短：6 个字大约 0.4 s 合成，先把声音放出来，后面的句子继续合成。
- 太短的碎片（比如硬切出来的"说。"）不会单独合成，会攒到下一句。
- 播放期间 `mute` 置位，会话层**丢弃麦克风音频**并重置识别器——否则麦克风会把 TTS
  自己的声音当成用户说话，形成自问自答。代价是**说话时不能插话**（M2 的 AEC 再解决）。

## 模型

| 项 | 值 |
|---|---|
| 模型 | `sherpa-onnx-vits-zh-ll`（中文 VITS，单音色女声） |
| 体积 | model.onnx 121 MB + lexicon 0.4 MB + 4 个 FST |
| 采样率 | 16 kHz（输出 48 kHz，线性重采样） |
| 许可 | **待核**（社区贡献，基于 VITS-fast-fine-tuning，未标 license） |
| 文本规则 | date / number / phone / new_heteronym 四个 FST |

## 实测（M1 Max）

| 指标 | 值 |
|---|---|
| 模型加载 + 预热 | 约 0.3–1 s（启动时） |
| 首句合成（6 字） | 400–440 ms |
| **句末 → 第一个声音** | **575–609 ms**（mock LLM，不含真实网络） |
| 首句播放 | 约 1 s 音频 |
| RTF | 0.17–0.33 |
| 输出 | 48 kHz，队列播放 |

真实链路再加 LLM 的首 token（0.3–0.6 s），预计 **说完到出声 1.0–1.5 s**。

## 配置

| 变量 | 作用 |
|---|---|
| `GABTWIN_TTS_MODEL_DIR` | TTS 模型目录（默认 `$GABTWIN_HOME/models/vits-zh-ll`） |
| `GABTWIN_ENGINE=streaming` | 只影响句末定稿，不影响 TTS |

## 单独验 TTS

```bash
cargo run -p gabtwin-probe -- --tts "$HOME/Library/Application Support/GabTwin/models/vits-zh-ll" \
  --text "你好，我是 GabTwin。" --out /tmp/tts.wav
afplay /tmp/tts.wav
```

## 音色候选（2026-10-26 实测）

同一句话 "你好，我是你的数字伙伴。今天想聊点什么？"：

| 引擎 | 采样率 | 合成耗时 | RTF | 说明 |
|---|---|---|---|---|
| VITS zh-ll（当前默认） | 16 kHz | 780 ms | 0.27 | 快，但音色偏机械 |
| Matcha zh-en | 16 kHz | 120–340 ms | 0.04–0.11 | 很快，中英混读；音色待你听 |
| Kokoro v1.1（sid 3） | 24 kHz | 3.7–4.0 s | ≈1.0 | 更自然，但**无法实时流式**，未采用 |

试听：

```bash
afplay /tmp/tts-vits-sample.wav     # VITS
afplay /tmp/matcha-sample.wav       # Matcha
afplay /tmp/kokoro-3.wav            # Kokoro
```

`Matcha` 的模型在 `~/Library/Application Support/GabTwin/models/matcha-zh-en/`；
它的声码器 `vocos-16khz-univ.onnx` 要从 GitHub release 单独下（下载慢，已在本机）。
选定后把 `GABTWIN_TTS_MODEL_DIR` 指过去即可，应用会自动识别 VITS / Matcha / Kokoro。

## 插话（barge-in）

- **耳机**：输出设备被识别为 Headphones/Headset 时，播放期间**不闭麦**；
  一旦识别到你开口（partial），立即停止播放、取消这一轮回复，你的话进入新一轮。
- **外放**：仍是半双工。麦克风会收到 TTS 自己的声音，没有 AEC 时任何"插话检测"都会
  被自己的回声触发。要真正外放插话，需要 AEC（WebRTC APM 或 macOS VPIO），是下一步。
- 任何情况下都可以点「停止」立即静音。

## 已知限制

1. **英文/生词会被跳过**：中文 lexicon 里没有的词（GabTwin、API、bug）直接忽略，不会念。
2. **只有一个人声音色**；3 个角色各自的声音还没做。
3. **外放时听不见你**：半双工；戴耳机可以直接插话（见上）。外放插话要等 AEC。
4. **没有口型**：viseme 时间轴还没接（M3）。
5. 首句可能在短语中间断开（"对我做了介绍" / "啊，那么…"），听感上是一个短停顿。
6. 输出设备固定为系统默认，不能选。
7. 线性重采样 16k→48k，没有抗混叠；语音可接受，音乐不行。

## 下一步

- [ ] 3 个角色 × 3 个音色（VITS 多说话人 / Kokoro / Melo 评估）
- [ ] 英文词的中文读法（API → "A P I" 或"接口"），或换中英混合 TTS
- [ ] viseme 时间轴（文本拼音 → 口型）
- [ ] AEC + 真打断（M2）
- [ ] 播放设备选择
