# 03 · 模型与资源

## 清单

| 环节 | 模型 | 体积 | 运行时 | 许可 | 状态 |
|---|---|---|---|---|---|
| VAD | Silero VAD | 1.8 MB | ONNX | 待核（MIT？） | 已验证（DSH 在用） |
| ASR | SenseVoiceSmall int8 | 239 MB | sherpa-onnx | 待核 | 已验证，RTF 0.025 |
| ASR 备选 | streaming zipformer 中英 | 100–300 MB | sherpa-onnx | 待核（多为 Apache-2.0） | 未验证 |
| ASR 流式（当前默认） | streaming zipformer 9 语 int8（中英等） | 340 MB | sherpa-onnx | 待核（来自 PengChengStarling，仓库未标 license） | **已验证**：英文 0%、中文 1.4%，首字约 350–520 ms，RTF 0.09–0.10 |
| ASR 流式（中文专用） | streaming zipformer zh int8 2025-06-30 | 167 MB | sherpa-onnx | 待核（icefall 代码 Apache-2.0，训练数据多源） | 已验证，中文 0%，英文不可用；内存少约 340 MB |
| ASR 流式（中英双语） | streaming zipformer bilingual zh-en 2023-02-20 | 198 MB | sherpa-onnx | **Apache-2.0** | 已验证，英文 2.2% 但中文有重复字（6.2%），未采用 |
| 端点 | smart-turn 类小模型 | 待定 | ONNX | 待核 | 可选，先用规则 |
| LLM | DeepSeek（云） | — | HTTPS | 服务条款 | 默认 |
| LLM 兜底 | Qwen3-4B/8B 4bit | 2–5 GB | MLX / llama.cpp | Apache-2.0（待核） | 未接 |
| TTS 轻量 | Piper 中文 | 60–100 MB/音色 | piper-rs | **音色各自不同** | 候选 |
| TTS 轻量 | Kokoro 82M | ~300 MB | ONNX | Apache-2.0（待核） | 候选 |
| TTS 高质量 | CosyVoice 2 0.5B | 1–2 GB | Python sidecar | 待核 | 后续 |
| TTS 云 | Azure / 火山 | — | WSS | 服务条款 | 备选，带 viseme |
| 数字人 | glTF + blendshape | 5–30 MB | Three.js | 资产授权 | 自建或采购 |

## 实测数据（M1 Max，SenseVoice int8，2 线程）

| 输入 | 耗时 | RTF |
|---|---|---|
| 5.58 s 整段 | 139 ms | 0.025 |
| VAD 分段全流程 | 158 ms | 0.028 |
| 3 s 窗 × 每 0.8 s 重解 | 69 ms/次 | — |
| 模型加载 | 581 ms | — |

测试语料是合成语音（`say` + `afconvert` 转 16 kHz 单声道 WAV），干净无噪声；
真实麦克风输入会略慢，但同一量级。

结论：**ASR 不是瓶颈**。瓶颈在端点判定、LLM 首 token、TTS 首包。

## 分阶段选型

### M1（先跑通）
- ASR：流式 zipformer 9 语（中英，已跑通，见 [dev/01](dev/01-voice-input.md)）；
  纯中文 zipformer 与 Apache-2.0 双语模型作为备选；SenseVoice 整段保留为离线/高精度备选
- LLM：DeepSeek 流式
- TTS：Piper 本地（CPU，快）

### M2（全双工）
- VAD：Silero 常开
- 端点：静音阈值 600 ms；若误判多，再挂小模型
- AEC：WebRTC APM

### M3（数字人）
- 口型：中文拼音 → 12 类 viseme 映射表
- 表情：12 个 blendshape 的状态机
- 资产：VRM / Ready Player Me 占位，后续换定制

### M4（质量）
- TTS 评估 CosyVoice 2（音色）与云 TTS（viseme 时间戳）
- 本地 LLM 兜底

## 存储布局

```text
~/Library/Application Support/GabTwin/
├── config.toml
├── models/
│   ├── sensevoice/
│   │   ├── model.int8.onnx
│   │   └── tokens.txt
│   ├── silero/
│   │   └── silero_vad.onnx
│   └── tts/
│       └── <voice-id>/
├── voices/
│   └── LICENSES.md
└── logs/
```

规则：

- 每个文件记录 `size` + `sha256`，启动时校验，失败即重下。
- 下载默认走国内镜像 `hf-mirror.com`（`GABTWIN_HF_MIRROR` 可覆盖），**强制直连、不走代理**，
  4 路并行，逐文件 sha256 校验；见 [ADR-009](05-decisions.md) 与 `scripts/fetch-model.sh`。
- 断点续传；未完成的文件用 `.part` 后缀，校验通过才改名。
- 模型目录可用环境变量 `GABTWIN_HOME` 覆盖（便于测试与离线部署）。

## 许可与合规

**这是本项目最容易踩的坑，商用前必须逐项确认。**

- [ ] SenseVoiceSmall 权重许可（代码 Apache-2.0，权重可能另有条款）
- [ ] streaming zipformer zh 权重许可（icefall 训练代码 Apache-2.0；训练数据为多源中文合集）
- [ ] streaming zipformer 9 语权重许可（PengChengStarling，仓库未标 license —— 当前默认模型，商用前必须确认）
- [ ] Silero VAD 权重许可
- [ ] Piper 各音色许可（部分为 CC BY-NC，**不可商用**）
- [ ] Kokoro 权重与音色许可
- [ ] CosyVoice 权重许可
- [ ] 数字人资产（VRM / 购买模型）的商用与再分发范围
- [ ] 云端 LLM / TTS 的数据使用与留存条款

原则：每个权重、每个音色都要有来源、版本、许可记录，统一写在
`assets/voices/LICENSES.md`，随安装包分发。

## 模型管理

- 首次启动引导下载 ASR + VAD（约 240 MB），显示进度与体积。
- TTS 音色按需下载，不预装。
- 离线模式：无网络时跳过下载，提示"仅本地模型可用"。
- 版本固定：每个模型锁定 revision 与哈希，不做自动升级。

## 数字人资产规格

| 项 | 要求 |
|---|---|
| 格式 | glTF 2.0（.glb），或 VRM 1.0 |
| 拓扑 | 头颈 + 上半身，面数 ≤ 8 万 |
| 表情 | blendshape ≥ 12 个（含口型与情绪） |
| 贴图 | 2K，PBR，皮肤可用预积分 SSS |
| 动画 | idle 呼吸、眨眼、点头、摇头 |
| 目标 | 720p / 30 fps，M1 Max GPU 占用 ≤ 50% |
