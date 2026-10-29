# 08 · 照片级真人（本地方案）资源评估

> 状态：提案 · 2026-10-29
> 这是对 [ADR-005 参数化 3D](05-decisions.md) 与 [00 愿景](00-vision.md)「不做照片级写实」的偏离。
> **批准前不改现有 3D 代码**；现有占位角色保留为 sidecar 不可用时的降级路径。
>
> 结论先说：**M1 Max 32GB 有机会跑「视频通话级」的照片真人，但目标要降到 640×480 / 15–20 fps；
> 720p30 的照片级细节在这台机器上不现实。先做 G1 实测，再决定接不接。**

## 0. 先把三个数字对齐

| 目标 | M1 Max 32GB | 说明 |
|---|---|---|
| 640×480，15–20 fps，嘴部 256² 生成 | **目标；需 G1 实测（可能 10–18 fps）** | 类似视频通话；脸占画面 1/3–1/2 时观感最好 |
| 720p30，256² 脸放大 | **不现实** | 输出可以标 720p，但脸只有 256² 的信息量；30 fps 也跑不满 |
| 720p30，照片级细节 | **需要换机器** | M5 Max / M 系列 Ultra，或 NVIDIA 24GB+；或云 GPU |

如果你说的是 **3D 写实（MetaHuman 那种）**，本机更难：Unreal + Audio2Face 基本要 NVIDIA，
不在本方案的讨论范围。这里按 **2D 照片/视频驱动** 规划。

## 1. 路线：MuseTalk 1.5 MLX

| 项 | 内容 |
|---|---|
| 模型 | MuseTalk 1.5（腾讯音乐 Lyra Lab），单步 latent inpainting，不是 diffusion |
| 生成区域 | 256×256 下半脸，贴回源图/源视频 |
| 训练帧率 | 25 fps；官方称 V100 上 30 fps+ |
| Apple Silicon 移植 | `mlx-community/MuseTalk-1.5-{q4,q8,fp16}`，torch-free，MIT |
| 移植性能 | 256² 脸约 **34 张/秒**（batch 8），峰值约 **7 GB** |
| 已跑通的 Mac 参考 | `distillCKirk`：M5 Max 128GB，640×480，目标 20 fps；`dahai80/musetalk-mlx`：M5 Max，640×480，29.8 fps（`DECODE_128`，128² 解码换速度） |

**它只换嘴和下半脸**。头、肩、背景、眨眼、轻微晃动都来自源视频；源视频没有自然动作，
成品就会是「一张会动嘴的照片」。所以人物素材和模型同等重要。

> 风险：MLX 移植是 2026-06 的社区项目（下载量几百，不是官方），稳定性和画质要用 G1 验证；
> 如果它不达标，退回 PyTorch MuseTalk + NVIDIA，或换机器。

为什么不选其它：

- **LivePortrait / SadTalker / EchoMimic / Hallo**：照片驱动、质量好，但多数不是为「边生成边播」设计的；
  在 M1 Max 上要么慢，要么需要额外的音频→表情模型，链路更长。
- **MuseTalk PyTorch**：Mac 上 MPS 支持不稳，且要带 PyTorch；MLX 移植已经把运行时做轻了。
- **端到端视频 diffusion（Hallo2 / Sonic / OmniHuman 类）**：质量高，但消费级本机实时不成立。

## 2. 需要什么资源

### 2.1 机器

| 项 | 最低 | 建议 |
|---|---|---|
| 芯片 | M1 Max（当前） | M5 Max / M 系列 Ultra |
| 统一内存 | 32 GB | 64 GB+ |
| 空闲磁盘 | 20 GB | 40 GB+ |
| 系统 | macOS 14+ | macOS 15+ |
| 外接 GPU | 不支持 eGPU | 若要 NVIDIA，需另配一台机器 |

M1 Max 的 24 核 GPU 比 M5 Max 弱不少；下面所有 fps 都是**待实测的估算**，不是承诺。

### 2.2 人物素材（比模型更决定成败）

推荐给 **10–30 秒、1080p 或 4K、25/30 fps** 的正面视频：

- 固定机位、平视、脸部占画面 1/3–1/2；
- 均匀柔光，没有半边脸死黑；不要强逆光；
- 中性表情、嘴自然闭合、眼睛睁开；
- 不遮脸：没有手、头发、麦克风、眼镜反光挡住嘴；
- 不大幅转头、不走出画面；有自然眨眼和极轻的呼吸/晃动；
- 可循环：首尾姿态接近，方便做 idle 循环；
- 最好 2–3 条：中性、浅笑、倾听（后面做状态切换）。

如果只有 **一张照片**：嘴会动，但头不会动、不会自然眨眼，观感会明显“假”。
那时需要再加 LivePortrait 之类的头部驱动，或接受静态头像。

**授权**：必须是你本人，或已获得书面同意的真人；不要用公众人物/网图。
成品要带“AI 生成”标识；商用需要肖像许可（model release）。

### 2.3 模型权重（按家规：只列清单，不自动下载）

**主模型：MuseTalk 1.5 MLX q8（推荐）**

- 仓库：`mlx-community/MuseTalk-1.5-q8`
- 目标目录：`models/avatar/musetalk-1.5-q8/`

| 文件 | 大小 |
|---|---|
| `unet.safetensors` | 1,455,842,674 B（约 1.36 GiB） |
| `vae.safetensors` | 167,335,279 B（约 160 MiB） |
| `whisper_encoder.safetensors` | 16,423,174 B（约 16 MiB） |
| `config.json` | 223 B |
| **合计** | **约 1.53 GiB（1.64 GB）** |

备选：

| 变体 | 合计 | 取舍 |
|---|---|---|
| `MuseTalk-1.5-q4` | 约 1.41 GiB（1.51 GB） | 更省内存，画质略低（误差 2.74/255） |
| `MuseTalk-1.5-fp16` | 约 1.75 GiB（1.88 GB） | 画质最好，内存/带宽最高 |

**人脸预处理（CPU）**

- DWPose：`yzd-v/DWPose` 的 `dw-ll_ucoco_384.onnx`，134,399,116 B（约 128 MiB），Apache-2.0；
  目标 `models/avatar/dwpose/`。
- 人脸检测/对齐：S3FD 或 MediaPipe（MediaPipe 是 Apache-2.0，较省事；S3FD 许可需核）。
- 人脸分割/贴回遮罩：BiSeNet `79999_iter.pth`（上游 Google Drive，约 50–100 MB，许可需核）；
  也可以先用羽化椭圆遮罩，质量差一点但能跑。
- 可选超分：GFPGAN / CodeFormer（约 300 MB），只在对 256² 贴回的脸不满意时加；
  它会增加每帧耗时，实时路径慎用。

**已有、不用重复下载**

- TTS：本机 Qwen3-TTS 1.7B（现有 sidecar）。
- ASR：streaming paraformer + SenseVoice（现有）。
- LLM：本机 Qwen3.5-9B 4bit（现有）；如果显存吃紧，可切回云端 DeepSeek。

### 2.4 软件与运行时

- Python 3.12 sidecar（和现有 TTS sidecar 并列），依赖：
  - `mlx` / `mlx-metal`（按移植仓库锁版本）；
  - `musetalk-mlx`（`xocialize/musetalk-mlx`，MIT）或 `fusion-mlx`（更重的优化版）；
  - OpenCV、NumPy、`ffmpeg`；可选 `coremltools`（把 UNet 转 ANE，见下）。
- Rust 侧：sidecar 管理、16 kHz PCM → sidecar、JPEG/PNG 帧 → UI、`PlaybackTap` 时钟、打断清队列。
- UI：把 Three.js canvas 换成 `<img>`/`<canvas>` 帧播放器；20 fps、640×480 JPEG 本地 WebSocket
  约 1–2 MB/s，不需要走 Tauri IPC。
- 打包：sidecar 可选；缺失时降级回 3D 占位角色。

> Core ML/ANE 是可选的加速路径：`distillCKirk` 在 M5 Max 上用 ANE 跑 UNet、GPU 跑 VAE，
> 做到 640×480 / 20 fps。M1 Max 的 ANE 更弱，转换也麻烦，**不要作为第一步**。

### 2.5 许可与合规

| 资产 | 许可 | 备注 |
|---|---|---|
| MuseTalk 代码 | MIT | 上游明确允许商用 |
| MuseTalk 权重 | MIT / 允许商用 | 上游声明 any purpose, even commercially |
| Whisper-tiny | MIT | 随 MLX 仓库 |
| sd-vae-ft-mse | CreativeML OpenRAIL-M | 商用允许，但有使用限制，需读条款 |
| DWPose | Apache-2.0 | 干净 |
| S3FD / BiSeNet | 需逐个核 | 上游/第三方权重来源要记录 |
| 人物视频/照片 | 肖像许可 | 必须本人或书面授权；公众人物不行 |
| 成品 | AIGC 标识 | 对用户可见；商用前过一遍当地法规 |

原则：每个权重记录来源、哈希、许可；许可不明的不进商用版本。

### 2.6 工程时间

| 阶段 | 内容 | 估算 |
|---|---|---|
| G1 spike | 单模型跑 M1 Max，256² 脸、640×480 合成，量 fps/内存 | 1–2 天 |
| G2 并发 | 与 Qwen3-TTS/ASR 同时跑，量掉帧与首响 | 2–3 天 |
| 集成 | sidecar 协议、帧传输、音画同步、打断、idle 循环 | 2–3 周 |
| 质量 | 人脸裁剪/贴回、颜色匹配、抖动、可选超分 | 1–2 周 |
| 合计 | 到可日常对话 | **约 4–6 周**（不含人物拍摄） |

## 3. M1 Max 上的预期与瓶颈

### 内存账（粗略）

| 组件 | 峰值 |
|---|---|
| MuseTalk q8（含 MLX 缓存） | 6–8 GB |
| Qwen3-TTS 1.7B | 3–4 GB |
| ASR（流式 + 定稿） | 约 1 GB |
| 本地 LLM 9B 4bit | 5–6 GB |
| macOS + 应用 + 帧缓冲 | 5–6 GB |
| **合计** | **约 20–25 GB / 32 GB** |

能放下，但 GPU 是共享的：TTS 合成、MuseTalk UNet/VAE、LLM 解码会互相抢。
最直接的缓解是 **LLM 走云**（只出文本，不出音频/视频），把本地 GPU 留给 TTS + 数字人。

### 延迟链

```text
用户说完 → ASR 定稿 0.3–0.7 s
        → LLM 首 token 0.3–0.6 s（本地 9B 可能更慢）
        → TTS 首块 0.7–3 s（Qwen3-TTS 1.7B 的现状）
        → MuseTalk 首帧 0.08–0.15 s（M1 Max 估算）
        → 屏幕上的嘴开始动
```

**照片级数字人不会把 3.6 s 变成 1.5 s。** 首响仍然是 TTS/LLM 那条线；数字人只负责“等的时候有脸”。
如果首响要进 2 s，得先解决 TTS 首块（更小的 TTS、短首句、流式参考音）。

### 画质边界

- 脸只有 256² 的生成分辨率，贴回 640×480 是 1:1 左右；贴回 720p 会被放大，细节变软。
- MuseTalk 官方承认：胡子、唇形/唇色等身份细节保留一般；单帧生成会有轻微抖动。
- 背景/身体是源视频，静止机位最稳；大幅头动会出现贴回边界。

## 4. 三个 gate（先测，不先承诺）

### G1 · 单模型跑分（1–2 天）

- 用公开测试素材（不用你的脸），跑 `MuseTalk-1.5-q8`；
- 量：256² 脸/秒、640×480 合成 fps、峰值内存、首帧延迟；
- 通过线：**≥15 fps**（低于 12 fps 就降 384/256 或换机器）。

### G2 · 与 TTS 并发（2–3 天）

- 同时跑 Qwen3-TTS + MuseTalk + ASR；
- 量：TTS 首块是否变慢、MuseTalk 是否掉到 10 fps 以下、内存是否 swap；
- 不通过：LLM 切云、TTS 换更小模型，或降到 384×384。

### G3 · 人物素材（你提供后 1 天）

- 10–30 s 视频 → 抽帧 → 人脸裁剪 → 贴回 → 看嘴/下巴/脖子边界；
- 不行就换素材/补光/重拍；只有照片则先接受静态头。

## 5. 三条交付路径

| 路径 | 结果 | 代价 |
|---|---|---|
| **A. 本机 M1 Max** | 640×480，15–20 fps，视频通话感 | 不买硬件；画质/帧率有上限 |
| **B. 换机器** | 720p30，接近参考 demo | M5 Max / Ultra 64GB+，或 NVIDIA 24GB+ |
| **C. 云 GPU** | 最容易到 720p30 | 音视频出本机，违背“本地”原则，不推荐 |

如果“本地”是硬约束，建议先走 A，用 G1/G2 的数字决定是否值得换 B。

## 6. 下一步：需要你给三样东西

1. **是谁的肖像**：你本人 / 已书面同意的真人 / 有授权的合成脸。
2. **素材**：有没有 10–30 s 的正面视频？只有照片也可以，但先接受“头不动”。
3. **目标**：640×480 / 15–20 fps 够不够？还是必须 720p30（那就得换机器）。

另外，按家规我不会自动下载权重。G1 需要你点头下载：

- `mlx-community/MuseTalk-1.5-q8`，4 个文件，约 **1.53 GiB**；
- `yzd-v/DWPose` 的 `dw-ll_ucoco_384.onnx`，约 **128 MiB**；
- 目标目录：`models/avatar/`。

你确认后，我先用公开测试素材跑 G1，不碰你的照片；拿到 M1 Max 的真实 fps 和内存，再决定要不要接进 GabTwin。

## 参考

- MuseTalk 上游（256²、25 fps、30 fps V100、许可）：<https://github.com/TMElyralab/MuseTalk>
- MuseTalk 1.5 MLX q8（1.64 GB、34 faces/s、7 GB、MIT）：<https://huggingface.co/mlx-community/MuseTalk-1.5-q8>
- MuseTalk 1.5 MLX q4（1.51 GB）：<https://huggingface.co/mlx-community/MuseTalk-1.5-q4>
- MLX 移植代码（torch-free）：<https://github.com/xocialize/musetalk-mlx>
- M5 Max 实时业务层（640×480、29.8 fps、4 GB 目标）：<https://github.com/dahai80/musetalk-mlx>
- Mac 上跑通的照片级参考（M5 Max 128GB、640×480 20 fps、ANE+GPU）：<https://github.com/liwagu/distillCKirk>
- DWPose 权重（Apache-2.0）：<https://huggingface.co/yzd-v/DWPose>
