# OmniVoice 本机评估（2026-10-30）

结论：**能在 M1 Max 的 MPS 上跑，但 RTF 约 2.1–2.8，且没有流式接口，不适合当前实时对话。**
保留为离线/质量模式备选，不替换 Breeze TTS 2。

## 来源与许可

- 代码：<https://github.com/k2-fsa/OmniVoice>（Apache-2.0）
- 权重：<https://huggingface.co/k2-fsa/OmniVoice>（**CC-BY-NC**，本地自用可以，商用需授权）
- 架构：Qwen3-0.6B + diffusion LM + HiggsAudioV2 tokenizer，24 kHz，约 612M 参数

## 本机文件

下载到 `models/tts/omnivoice/`（gitignore）：

| 文件 | 大小 | SHA256 |
|---|---|---|
| `model.safetensors` | 2,450,344,112 B | `730839316de585f4c8298ec0e1712efc10fb19c6fa4e36eb741cb8d51ebcf6aa` |
| `audio_tokenizer/model.safetensors` | 805,665,628 B | `fe7c5e8785e0a05833e1bfc3e002ec7f55af21e306b2e7154a448c1f54ccfb0d` |
| `tokenizer.json` | 11,423,986 B | `408f669b7e2b045fdf54201d815bd364e6667dbd845115da81239c40bc6dcfd1` |

环境：`.venv-omni`，Python 3.12，torch 2.8.0，torchaudio 2.8.0，
transformers 5.22.0，omnivoice 0.2.1（`--no-deps`）。

## 实测（M1 Max 32GB，MPS，fp32）

参考音：`workspace/tts/gyy-ref.wav`；文本三句（高兴/低落/中性）。

| 句 | 步数 | 生成耗时 | 音频时长 | RTF |
|---|---|---|---|---|
| 高兴 | 16 | 11.69 s（含首次编译） | 3.60 s | 3.25 |
| 低落 | 16 | 5.59 s | 2.60 s | 2.15 |
| 中性 | 16 | 5.04 s | 1.88 s | 2.68 |
| 高兴 | 32 | 10.24 s | 3.60 s | 2.85 |

- 加载：冷启动 2.58 s，缓存后 1.44 s；
- 峰值 RSS 约 2.6 GB，MPS driver 约 3.2 GB；
- F0 中位 192.5 / 170.5 / 162.8 Hz，句内 p5–p95 3.2–6.8 个半音；
- **没有 `stream=True` / 增量 yield**：`generate()` 返回整段 audio，首音必须等整句结束。

## 判断

1. **实时对话**：不换。当前 Breeze TTS 2 首音约 2.3 s，且是流式；OmniVoice 单句要 5–10 s。
2. **质量/离线**：可用。声音克隆、600+ 语言、声音设计（性别/年龄/音高/口音/方言）、
   拼音/CMU 发音纠正、非语言标签都是 Breeze 没有的。
3. **许可**：权重 CC-BY-NC；与 Breeze TTS 2 的非商用许可同类，不是“换成 Apache 就商用”。
4. **Apple Silicon 注意**：audio tokenizer 在 MPS 上不支持，源码强制放 CPU；本次实测
   仍能跑，但速度和内存要按 CPU tokenizer 估算。

## 值得借鉴而不是替换的点

- `create_voice_clone_prompt()`：参考音编码一次、保存/复用；Breeze 侧可确认是否重复编码。
- `omnivoice/utils/text.py`：数字/日期/货币规范化、缩写感知切句、保护拼音/CMU 控制符。
- `cross_fade_chunks()`：分段之间静音 + 淡入淡出，减少拼接爆音。
- voice design 的音高只有五档，且官方 tips 说参考音会压过 instruct；
  **不能替代当前的 PSOLA F0 限制**。

## 复现

```bash
.venv-omni/bin/python - <<'PY'
import time, torch
from omnivoice import OmniVoice, OmniVoiceGenerationConfig
m = OmniVoice.from_pretrained("models/tts/omnivoice", device_map="mps", dtype=torch.float32)
t = time.time()
a = m.generate(
    text="太好了，我太开心了，今天真是最棒的一天。",
    language="zh",
    ref_audio="workspace/tts/gyy-ref.wav",
    ref_text=open("workspace/tts/gyy-ref.txt").read().strip(),
    generation_config=OmniVoiceGenerationConfig(num_step=16),
)
d = len(a[0]) / 24000
print(f"wall {time.time()-t:.2f}s audio {d:.2f}s rtf {(time.time()-t)/d:.2f}")
PY
```
