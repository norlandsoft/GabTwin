# GabTwin

和你的数字分身闲聊。

GabTwin 是一个本地优先的实时语音对话应用：你说话，它听懂、回答，并用一张会动嘴的脸看着你。

**状态：M0 规划阶段，尚无运行代码。**

## 文档

| 文档 | 内容 |
|---|---|
| [00 愿景与范围](docs/00-vision.md) | 做什么、不做什么、成功标准 |
| [01 系统架构](docs/01-architecture.md) | 模块、数据流、状态机、延迟预算 |
| [02 技术选型](docs/02-tech-stack.md) | Rust / Tauri / Three.js 与依赖清单 |
| [03 模型与资源](docs/03-models.md) | ASR / TTS / LLM / 数字人模型与许可 |
| [04 路线图](docs/04-roadmap.md) | M0–M4 里程碑与验收标准 |
| [05 关键决策](docs/05-decisions.md) | ADR 与理由 |
| [06 风险与开放问题](docs/06-risks.md) | 风险表、待验证清单 |

## 目标平台

macOS 14+ / Apple Silicon。开发基线：M1 Max，32 GB，10 核 CPU，24 核 GPU。

## 三条原则

1. **音频不出本机** —— 只有文本进 LLM 请求。
2. **先半双工，再全双工** —— 先跑通"说完再答"，再处理打断。
3. **数字人是可替换的渲染层** —— 晚接不亏，不绑架核心链路。

## 参考实现

DSH 的语音输入插件（`@deepseek-ai/dsh-experimental-speech-to-text-sensevoice`）
已经验证了 SenseVoice + sherpa-onnx 在本机的可行性，可作为 ASR 部分的参照；
但它只做听写，没有端点、对话回路和数字人。

## 名字

GabTwin = **gab**（闲聊）+ **twin**（分身），读作 /ɡæb twɪn/。
