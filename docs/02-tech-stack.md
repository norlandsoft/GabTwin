# 02 · 技术选型

## 结论

| 层 | 选择 | 备选 | 理由 |
|---|---|---|---|
| 核心语言 | **Rust** | TypeScript | 延迟可预测、单二进制分发、音频生态够用 |
| 桌面壳 | **Tauri 2** | Electron | 体积小、原生 Rust 后端、WebView 够渲染 |
| UI / 数字人 | **TypeScript + Three.js** | wgpu / bevy | glTF + morph target 工具链成熟，迭代快 |
| 音频 I/O | **cpal** | coreaudio-rs | 跨平台、维护活跃 |
| 重采样 | **rubato** | speexdsp | 实时、任意比率、质量好 |
| AEC / NS / AGC | **webrtc-audio-processing** | aec3-rs | WebRTC APM 的生产级实现 |
| VAD | **Silero VAD（ONNX）** | sherpa 内置 VAD | 已验证，1.8 MB |
| ASR | **sherpa-onnx Rust 绑定** | whisper-rs | SenseVoice / zipformer 都能加载 |
| LLM | **HTTP + SSE（reqwest）** | mistral.rs、llama.cpp | 云端质量优先，本地只做兜底 |
| TTS | **piper-rs（本地）** / 云 TTS | CosyVoice sidecar | 先跑通延迟，再换音色 |
| 本地通信 | Tauri IPC | WebSocket | 单机不需要网络 |
| 存储 | TOML + 文件 | SQLite | 配置量小；对话记录后续再定 |

## 为什么核心用 Rust

1. **音频回调是硬实时**：不能有 GC 停顿、不能有 JIT 预热，Rust 没有运行时。
2. **分发**：单个 `.app`，不用带 Python 解释器和几百兆依赖。
3. **对接**：sherpa-onnx 是 C API，官方有 Rust 绑定，比 TS 绑定少一层。
4. **AEC**：`webrtc-audio-processing` 是现成的 C++ 绑定，Rust 侧调用直接。

代价：

- 开发慢，尤其是 UI 迭代；
- 中文 TTS 的前端（分词、多音字、韵律）在 Rust 里最薄；
- 生态里没有现成的"语音对话编排"框架，状态机要自己写。

## 为什么不用 Python 做主栈

`pipecat`、`LiveKit Agents` 能在一两周拼出闭环，但：

- 打包要带解释器与依赖，安装包 200 MB+；
- GIL 对音频回调不友好，实时部分最终还是要下沉到 C/Rust；
- 结论：Python 只做 sidecar（CosyVoice 这类），且**可选**。

## 前端

- Tauri 2 + Vite + TypeScript
- 3D：Three.js（GLTFLoader、MorphTarget、AnimationMixer）
- 状态：轻量 store（zustand 或自写 reducer）
- 样式：CSS 变量 + 少量原生 CSS，不引入重 UI 框架

## 平台与权限

- macOS 14+，Apple Silicon 优先；Intel 只做兼容性验证。
- `Info.plist` 需要 `NSMicrophoneUsageDescription`，文案要写清"用于语音对话"。
- 麦克风权限由 Tauri 的 bundle 配置注入。
- API Key 存 Keychain（`security-framework` crate），**不落配置文件**。

## 构建与分发

- `cargo tauri build` 产出 `.app` / `.dmg`
- Developer ID 签名 + notarize，否则用户首次打开会被拦
- 模型不进安装包：首次启动下载（约 240 MB），见 [03](03-models.md)
- 自动更新：M4 再定（Tauri updater 或 Sparkle）

## 依赖清单（初始）

```toml
[dependencies]
cpal = "0.18"
ringbuf = "0.4"
sherpa-onnx = "2.0"           # ASR / VAD / TTS 一个运行时，自带重采样
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["stream", "json"] }
serde = { version = "1", features = ["derive"] }
tauri = "2"
# webrtc-audio-processing = "2.1"   # 仅在 macOS VoiceProcessingIO 不可行时启用
```

> 以上是 2026-10-26 实际编译通过的组合（精确版本见 `Cargo.lock`，应用必须提交它）。
> 相对初稿的改动：
> - `cpal` 0.15 → 0.18（API 有破坏性变化：`device.name()` 改为 `description()`，`build_input_stream` 收 `StreamConfig` 值）；
> - 去掉 `rubato`：sherpa-onnx 的 `accept_waveform(sample_rate, …)` 内部重采样；
> - 去掉 `ort`：sherpa-onnx 已捆绑 ONNX Runtime，VAD/TTS 不需要第二个运行时；
> - `sherpa-onnx` 用 crates.io 的 2.0，不跟 git master（可复现构建）。

## 待验证（M0 结束前）

- [ ] `sherpa-onnx` Rust crate 能在 M1 Max 上加载 SenseVoice int8，并达到 DSH 的 RTF 水平。
- [ ] Tauri 的 WKWebView 里 Three.js 720p 60 fps 的实际 GPU 占用。
- [ ] `webrtc-audio-processing` 的 C++ 编译依赖是否顺畅（需要 clang / CMake）。
- [ ] `piper-rs` 的中文音色可用性与许可。
- [ ] Tauri 麦克风权限在首次启动时的弹窗行为。

## 备选方案（若 Rust 阻塞进度）

允许用 TypeScript/Electron 做一个 **spike** 验证交互与延迟（直接复用 `sherpa-onnx-node`），
但接口按本文件定义，后续替换为 Rust 实现时 UI 与事件协议不变。

判断标准：如果 M1 第 1 周结束时还不能跑通"采集 → 识别 → 打印文字"，
就切 spike，先把体验跑出来，再回头补 Rust。
