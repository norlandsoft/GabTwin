# 07 · 人物形象（数字人）规划

> 状态：M3.0 已落地 · 2026-10-29
> **2026-10-29 方向变更**：用户要求「照片级真人 + 本地」，3D 路线暂缓；
> 资源评估与可行性见 [08 照片级真人](08-photoreal-avatar.md)。ADR-005 需重审后才能改产品范围。
> 现有 3D 占位角色保留为 sidecar 不可用时的降级路径。
> 目标：在现有「语音输入 → LLM → TTS → 播放」链路上，加一个能看、会动、能被打断的角色形象。
> 关联：[ADR-005 参数化 3D](05-decisions.md)、[ADR-008 角色不是分身](05-decisions.md)、[M3](04-roadmap.md)。
>
> 已落地：`PlaybackTap`（播出帧 + 10 ms RMS）、`avatar-event`（30 Hz）、
> Three.js 程序化占位头与 `ui/vendor/avatar.js`、振幅口型与听/想/说/打断状态机。
> 尚未落地：VRM 资产、拼音 viseme、情绪标签。

## 0. 结论（先看这段）

1. **路线选参数化 3D**：VRM 1.0 / glTF + blendshape，Three.js 渲染；不做逐帧视频生成。
2. **口型分三级**：L0 振幅（第一版，必定同步）→ L1 拼音 viseme（第二版）→ L2 强制对齐（可选）。
3. **数字人只吃三条流**：对话状态、语音时间轴、播放遥测。音频仍留在 Rust，不过 IPC。
4. **先做主窗口内的人物区**；独立透明悬浮窗放到第二阶段，先验证 Tauri 在 macOS 上的透明窗行为。
5. **资产不自动下载**：先用程序化占位头把链路跑通，选定 VRM 后放入 `assets/avatar/<id>/`。

一句话：**第一版不追求「像真人」，先追求「嘴对得上、打断会闭嘴、不说话时还活着」。**

---

## 1. 当前数字人技术：四条路线

| 路线 | 代表 | 本机实时性（M1 Max） | 强项 | 弱项 |
|---|---|---|---|---|
| **参数化 3D** | VRM / ARKit blendshape、three-vrm、MetaHuman、VTuber | 720p 30–60 fps 轻松；只吃渲染 | 任意文本、可立即打断、离线、可控 | 写实度取决于资产；定制建模有成本 |
| **2D 照片驱动** | LivePortrait、MuseTalk、SadTalker、EchoMimic、Hallo、Sonic | 256–512 px 可实时；720p30 在本机不现实（估算 10–20 fps，且与 TTS 抢 GPU） | 一张照片就「像」；表情细节好 | 侧脸/遮挡/长时稳定性；身份与肖像授权；逐帧生成有延迟 |
| **端到端神经 3D** | GaussianTalker、SyncTalk、3DGS avatar | 需逐人训练；消费级实时不成立 | 自由视角、写实 | 训练与算力成本高；资产管线重 |
| **云数字人服务** | HeyGen、Tavus、D-ID、Anam | 25–30 fps 流式，延迟 300–800 ms | 最快得到照片级效果；不用本机 GPU | 音视频出本机；订阅与用量成本；供应商锁定 |

> 上表的「本机实时性」是规划估算，不是实测；M3.0 会先量渲染，再量 2D 方案（如果需要）。

### 「实时对话」难的不是渲染

一个能对话的数字人，真正难的是下面五件事，渲染只是最后一层：

1. **首响**：现在链路的首音在 1.5 s 预算内，但本机 Qwen3-TTS 1.7B 的日志里出现过 3.6 s；数字人不能让它更慢。
2. **打断**：人一开口，声音要停、嘴要闭、LLM 要取消；闭嘴比停音频更显眼。
3. **轮次**：听、想、说、被打断，四种状态要有不同的脸和身体，不能只有「说话/不说话」。
4. **口型时钟**：口型必须跟**实际播出的样本**对齐，不能跟「网络包到达时间」对齐。
5. **生命感**：不说话时眨眼、呼吸、看人；否则再精致的模型也像一张会动的海报。

---

## 2. 为什么 GabTwin 选参数化 3D

- **算力账**：本机已经在跑 Qwen3-TTS 1.7B（MLX/GPU）、ASR、VAD；再加 720p 逐帧视频生成，会抢同一块 GPU，首音和帧率一起掉。
- **交互账**：聊天回复任意长、任意内容，随时可能被打断；参数化 3D 的每一帧都是「当前状态」，没有生成延迟，也没有「这句话生成到一半」的问题。
- **资产账**：VRM 1.0 标准自带 5 个口型（aa / ih / ou / ee / oh）、眨眼、6 种情绪、lookAt、spring bone；一次制作，可换角色。
- **产品账**：ADR-008 已把产品定为「角色」，不是用户分身；预设角色可以一次性做资产，不涉及用户人脸/声纹。
- **写实账**：3D 写实也能做，但资产成本高；照片级 2D 作为**同一渲染接口下的第二种实现**，需要时再接，不绑死第一版。

**结论**：3D 是默认路径；2D 照片驱动是可选路径，不作为 M3 的前置。

---

## 3. 目标与验收

| 阶段 | 目标 | 判据 |
|---|---|---|
| M3.0 管道 | 占位头 + 振幅口型 + 状态机骨架 | 720p ≥30 fps；嘴随声音开合；打断 ≤100 ms 闭嘴；首音延迟不增加 |
| M3.1 口型 | 拼音 viseme + 时间轴 | 口型偏差 p50 ≤80 ms、p90 ≤120 ms；20 s 长句末段漂移 ≤150 ms |
| M3.2 生命感 | 眨眼/呼吸/注视/微表情/6 情绪 | 静置 60 s 不僵；10 轮对话不崩；GPU ≤50%；内存 10 min 增长 <100 MB |
| 回归 | 不拖慢语音链路 | 首响 p50 与接入前同量级；断网仍可识别与合成 |

> **先说明**：数字人不会缩短首响。它只能把「等 2–3 秒」变得可看（倾听、思考、点头），不能把 3.6 s 变成 1.5 s。首响是 TTS/LLM 那条线的事。

---

## 4. 架构：数字人只消费，不拥有

```text
TTS worker ── clause(text, start_ms, duration_ms, visemes) ──► app.emit("avatar-event")
     │                                                              │
     └─ PCM ─► Speaker ─► output callback ─► level / played_frames ─► 30 Hz emitter
                                                              │
WebView：Three.js + three-vrm ◄───────────────────────────────┘
     ▲
     └── session-event（state / level / segment / tts-*）
```

三条硬规则：

1. **音频不过 IPC**：PCM 仍在 Rust，WebView 只收时间轴、电平、状态。
2. **数字人不阻塞 TTS**：所有事件 fire-and-forget；viseme 计算在 TTS worker 里做，UI 不做文本解析。
3. **只有一个时钟**：输出回调里的 `played_frames`。口型、表情、打断都以它为准。

### 4.1 事件协议

新增一个 `avatar-event` 通道（Tauri 事件名不能带点）：

| kind | 载荷 | 时机 |
|---|---|---|
| `state` | `{ state: idle \| listening \| thinking \| speaking \| interrupted \| error }` | 对话状态切换 |
| `clause` | `{ id, text, start_ms, duration_ms, visemes: [{ t, v, w }] }` | 每个 TTS 小句入队时 |
| `frame` | `{ rms, played_ms, playing }` | 30 Hz，来自输出回调 |
| `stop` | `{ reason }` | 打断 / 清队列 / 出错 |

- `start_ms`：该小句在「本轮音频队列」里的起点（从第一次 push 算起）。
- `played_ms`：输出回调实际播出的毫秒数（同样从第一次 push 算起）。
- 当前小句的播放位置 = `played_ms - start_ms`；小于 0 表示还没轮到它。
- 设备输出缓冲会带来 10–30 ms 的「先播后到」；用 `GABTWIN_AVATAR_LATENCY_MS` 校准，默认 0，实测后填。

> M3.0 只实现了 `frame` 与 `stop`；`state` 仍由 UI 从现有 `session-event` / `llm-*` / `tts-*` 映射。
> `clause` 与 viseme 时间轴在 M3.1 随拼音映射一起接。

### 4.2 代码落点

| 位置 | 改动 |
|---|---|
| `crates/gabtwin-audio/src/lib.rs` | 新增 `PlaybackTap`：在 `fill_f32` / `fill_i16` / `fill_u16` 里累计播出帧与 10 ms RMS；不分配、不加锁 |
| `src-tauri/src/lib.rs` | 起一个 30 Hz emitter 读 `PlaybackTap` 发 `frame`；`TtsCommand::Stop` 发 `stop`。M3.1 再在 `Clause` 入队时发 `clause` |
| 新 crate `crates/gabtwin-avatar` | `VisemeTrack::from_text(text, duration_ms)`；拼音→viseme 映射；纯逻辑，可单测 |
| `tools/avatar-build/src/main.js` + `ui/vendor/avatar.js` | Three.js 渲染；状态机；口型混合；idle 程序化动作（后续换 three-vrm） |
| `ui/index.html` / `ui/main.js` | 人物区 canvas；监听 `avatar-event`；把现有 `session-event` 映射成状态 |
| `assets/avatar/<id>/` | `model.vrm` + `LICENSE.md` + 可选贴图/动画 |

---

## 5. 口型：三级方案，先保证「同步」再追求「像」

### L0 · 振幅口型（第一版，M3.0）

- 输出回调每 10 ms 算一次 RMS（只算平方和，不分配）；30 Hz 发 `rms`。
- UI：`jawOpen = smoothstep(rms)`，attack 20–40 ms、release 80–120 ms。
- 优点：**永远不会不同步**，因为电平就是正在播出的样本；换任何 TTS 都成立。
- 缺点：只有下巴开合，没有口型形状；适合先验证管道与性能。

### L1 · 拼音 viseme（第二版，M3.1）

- 文本 → 拼音（Rust `pinyin` crate；多音字用词组上下文，或前端用 `pinyin-pro` 兜底）。
- 去声调，按声母/韵母映射到 6–8 类 viseme；按音节估算时长，铺到该小句的 `duration_ms` 上。
- `visemes` 只发关键帧（每秒 4–8 个），UI 插值；**下巴开合仍由 RMS 控制**，viseme 只决定嘴形。
- 每个小句重新对齐一次，误差不跨句累积；单句超过 15 s 时在中点重锚一次。

| 拼音 | viseme | 例 |
|---|---|---|
| b / p / m | 闭唇 → aa | 爸、妈 |
| f | FF / ih | 发 |
| d / t / n / l | DD / ih | 大、天 |
| g / k / h | kk / aa | 哈 |
| j / q / x、zh / ch / sh、z / c / s | CH / SS / ih | 知、吃、西 |
| a / ai / ao / an / ang | aa | 啊 |
| o / ou / ong | oh | 哦 |
| e / ei / en / eng / er | E | 呃 |
| i / ie / in / ing | ih | 一 |
| u / ü | ou | 乌、鱼 |
| 句尾 / 标点 | sil | — |

> VRM 1.0 标准口型只有 5 个（aa / ih / ou / ee / oh）。6–8 类已经够用，不要为了「精确」堆到 15 类；中文的视觉区分度主要在下巴开合与圆唇/扁唇。

### L2 · 强制对齐（可选，M3.3）

- Qwen3-TTS 本身不吐时间戳；官方建议合成后用 **Qwen3-ForcedAligner 0.6B** 对成品音频做一次对齐，拿词级 `[start, end]`。
- 代价：多一个模型（约 1.2 GB）、多一次推理；如果与 TTS 抢 GPU，首音会变慢。
- 只有 L1 实测达不到 ≤80 ms 时才考虑；模型由用户自己下载，脚本只给仓库 id、文件、体积、目标目录。
- 另一条路是换带原生 viseme 时间戳的云 TTS（Azure / 火山）；这会把文本送出本机，作为「高质量可选」而不是默认。

### 同步与打断的验收方法

- **口型偏差**：录屏 + 回环录音，逐帧量「嘴张开」相对「音频起振」的差；10 次取 p50/p90。
- **闭嘴延迟**：按 Esc / 触发 barge-in，量 `jawOpen` 落到 0.05 以下的时间；目标 ≤100 ms。
- **长句漂移**：20 s 小句，比较句首与句末的偏差；目标 ≤150 ms。
- **校准**：如果整体「嘴早于声」，把 `GABTWIN_AVATAR_LATENCY_MS` 调大；「嘴晚于声」调小。

---

## 6. 资产：VRM 1.0，先占位后定制

### 规格

| 项 | 要求 |
|---|---|
| 格式 | VRM 1.0（首选）或 glTF 2.0 + morph target |
| 范围 | 头 + 颈 + 上半身；不做全身行走 |
| 面数 | ≤8 万三角面 |
| 贴图 | 2K PBR；皮肤可用预积分 SSS，但不要依赖复杂 shader |
| 表情 | 5 口型 + 眨眼 + 6 情绪（中性/微笑/害羞/疑惑/思考/惊讶） |
| 骨骼 | VRM humanoid；头发/饰品 spring bone 可选 |
| 动画 | idle 呼吸、眨眼、点头、摇头；优先程序化，减少 clip 依赖 |

### 来源与许可

- **VRoid Studio**：免费，适合二次元风格；导出 VRM，授权按 VRoid 条款确认。
- **Ready Player Me**：免费头像，适合偏写实卡通；导出 VRM/GLB，商用前看条款。
- **Booth / 定制建模**：质量最高，但每个资产的许可不同，常见「个人使用 only」。
- **Mixamo**：动画库；重定向到 VRM humanoid，商用前确认 Adobe 条款。
- **原则**：`assets/avatar/<id>/LICENSE.md` 记录来源、作者、许可、是否可商用；许可不明 = 不用于商用版本。

### 占位头（M3.0 用，不依赖外部资产）

用 Three.js 基本几何搭一个「球头 + 眼睛 + 嘴」的占位角色：

- 验证相机、光照、口型、状态机、性能；
- 不涉及任何资产许可；
- 等用户选定 VRM 后，替换 `AvatarRenderer` 的 asset 层，协议不变。

**不自动下载任何 VRM / 模型。** 需要时只列：仓库或商店链接、文件名、体积、目标目录，由用户自己放。

---

## 7. 渲染与性能

### 技术栈

- M3.0 只用 Three.js；M3.1 接 VRM 时再加 `@pixiv/three-vrm`（VRM 1.0）。
- 当前 `ui/` 是纯 JS，没有打包器；用 esbuild 把 `three`（后续再加 `three-vrm`）+ `tools/avatar-build/src/main.js` 打成**一个** `ui/vendor/avatar.js`，提交进仓库。这样应用离线可跑，不依赖 CDN，也不要求每次启动都 build。
- 当前已按这个方式落地：源码在 `tools/avatar-build/src/main.js`，产物 `ui/vendor/avatar.js`。重建：`cd tools/avatar-build && npm install && npm run build`。
- `ui/index.html` 加 `<canvas id="avatarCanvas">` 与 `<script src="vendor/avatar.js">`。

### 场景

- 相机：35–50 mm 人像焦段，平视，人物占画面 60–70%。
- 光：1 主光 + 1 补光 + 环境贴图（RoomEnvironment 或简单渐变）；阴影默认关，需要时 1024 单张。
- 背景：先纯色/渐变；后续再考虑房间、景深、bloom。
- 渲染：`antialias: true`，`devicePixelRatio` 上限 2，ACES tone mapping，sRGB 输出。
- 720p 内部渲染，CSS 自适应；窗口隐藏时暂停 `requestAnimationFrame`。

### 预算

| 项 | 目标 |
|---|---|
| 帧率 | 720p 稳定 30 fps（上限 30，不追 60） |
| 单帧 CPU | ≤8 ms（JS + 蒙皮 + 口型） |
| Draw calls | ≤80 |
| 三角面 | ≤8 万 |
| GPU | ≤50%（Activity Monitor / `powermetrics` 实测） |
| 内存 | WebView 增量 ≤400 MB；10 min 增长 <100 MB |

> WebView 里拿不到可靠的 GPU 占用；先用「帧时间 p95 ≤20 ms」做代理指标，再用 Activity Monitor 复核。

M3.0 占位角色的场景开销（`renderer.info`，与 GPU 无关）：**40 draw calls、1.77 万三角面、3 个 shader program**；渲染循环已限 30 fps。真机 GPU 占用待用 Activity Monitor 复核。

---

## 8. 状态机与「活着」

| 状态 | 触发 | 脸 | 身体 | 备注 |
|---|---|---|---|---|
| `idle` | 无输入 | 中性/浅笑；眨眼 3–6 s | 呼吸 4 s；轻微重心摆动；每 2–5 s 微扫视 | 不能是静止模型 |
| `listening` | 按下说话 / VAD | 看镜头；眨眼变慢；眉微抬 | 前倾 2°；在语音停顿时轻点头 | 可用现有 `level` 驱动点头幅度 |
| `thinking` | 句末定稿 → 首音前 | 视线略向上；唇微张；眉轻皱 | 缓慢点头；偶发「嗯」口型 | 最长 3 s，之后回 listening |
| `speaking` | `tts-start` | 口型 + 句末眉/眼加强 | 句末点头；换气；眨眼 | 长句也要有微动作，不能只动嘴 |
| `interrupted` | 打断 / Esc | 50 ms 闭嘴；眼睛睁大 200 ms | 身体回正 | 不要做「说完」的收尾动作 |
| `error` | TTS/LLM 错 | 关切/困惑；不动嘴 | 轻微摇头 | 字幕说明原因 |

### 情绪

- **MVP**：标点 + 小词表（「！」→ 明亮/惊讶，「？」→ 疑惑，「……」→ 柔软，亲昵词 → 微笑）；加随机微表情。
- **v2**：让 LLM 在回复里带一个不可见标签（如 `<emo:smile>`），显示与 TTS 前剥离；或用一个本地小分类器。不要为了情绪标签把首 token 拖慢。
- 情绪只叠加在口型之上，不能覆盖口型；说话时嘴永远优先。

### 打断

- `stop` 一到：清 viseme 队列 → `jawOpen` 目标 0（release 50 ms）→ 表情 `interrupted` 200 ms → `idle`/`listening`。
- 与 `SpeakerHandle::clear()` 同一个事件循环里发出，保证「声音停」和「嘴闭」几乎同时。
- 外放半双工阶段，用户按 Esc 也要走同一条路径。

---

## 9. 实施阶段

### M3.0 · 会动嘴的占位头（3–5 天）

- vendor Three.js + three-vrm；加 canvas；占位头；idle 眨眼/呼吸。
- 输出回调加 `PlaybackTap`；30 Hz `avatar-event` frame；`jawOpen` 跟 RMS。
- 现有 `session-event` 映射成 `listening / thinking / speaking / idle`。
- 判据：720p ≥30 fps；嘴随声音开合；打断 ≤100 ms 闭嘴；首音延迟不增加；连续 10 分钟不漂。

### M3.1 · 对得上口型（约 1 周）

- `gabtwin-avatar`：拼音 → viseme 轨道；TTS worker 发 `clause`；UI 插值 + 共articulation。
- 校准 `GABTWIN_AVATAR_LATENCY_MS`；长句重锚。
- 判据：偏差 p50 ≤80 ms、p90 ≤120 ms；20 s 长句末段 ≤150 ms；字幕与口型不互抢。

### M3.2 · 像个活人（约 1 周）

- 注视/扫视、微点头、6 情绪、句末表情；设置页加形象选择、画质、置顶。
- 可选：独立透明悬浮窗（先验证 Tauri macOS 透明窗；不通过就留在主窗口）。
- 判据：静置 60 s 不僵；10 轮对话不崩；GPU ≤50%；内存稳定。

### M3.3 · 写实（可选，不排期）

- L2 强制对齐，或云 TTS 原生 viseme；或 2D 照片驱动 renderer。
- 只有当 M3.2 的「像」不够、且用户接受云/额外模型时才做。

---

## 10. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| GPU 争抢（TTS vs 渲染） | 首音变慢、掉帧 | 30 fps 上限；DPR 2；阴影关；实测后降分辨率/帧率；渲染让路 |
| 资产许可不清 | 不能商用/再分发 | `LICENSE.md` 台账；许可不明不商用；优先 VRoid/RPM |
| 口型漂移 | 像配音对不上 | 每句重锚；RMS 控制下巴；校准延迟；长句重锚 |
| 写实预期 | 用户觉得「不像人」 | 先说明 3D 风格；写实走云或 2D，单独评估 |
| WebView 性能 | 掉帧、发热 | 单文件 bundle、无 CDN、DPR 上限、隐藏暂停 |
| 透明窗不成立 | 桌宠形态做不了 | 先主窗口；透明窗单独 spike，不阻塞主线 |
| 首响本身偏慢 | 数字人再像也像「反应慢」 | 用 listening/thinking 行为覆盖等待；首响优化留在 TTS/LLM 线 |

---

## 11. 待定决策（定了 1–3 就能开 M3.0）

1. **形象风格**：3D 二次元 VRM（推荐）/ 3D 写实 / 2D 照片驱动。
2. **资产来源**：现成免费 VRM 占位 / 定制建模 / 你提供参考图或模型。
3. **窗口形态**：主窗口内的人物区（推荐先做）/ 独立透明悬浮窗（桌宠）。
4. **写实是否接受云**：可以晚定；不影响 M3.0–M3.2。

---

## 附：和现有文档的关系

- [01 架构](01-architecture.md) 已把 `avatar` 定义为「只消费音频 + 时间戳」的渲染层；本文把这条落成事件协议。
- [03 模型](03-models.md) 的「数字人资产规格」是本文第 6 节的来源；本文补充了 VRM 1.0 的 5 口型与许可台账。
- [04 路线图](04-roadmap.md) 的 M3 是本文第 9 节的粗粒度版本；本文把 M3.0 拆成「先振幅、再 viseme」。
- [05 ADR-005](05-decisions.md) 决定「参数化 3D，不做实时视频生成」；本文没有推翻它，只是把渲染接口留出 2D 第二种实现。

## 参考

- Qwen3-TTS 不直接吐时间戳、官方建议合成后用 Qwen3-ForcedAligner 对齐：<https://ryanwelch.co.uk/blog/qwen3-forced-aligner-word-timestamps/>
- three-vrm 的口型同步参考实现：<https://www.npmjs.com/package/three-vrm-lip-sync>
- three-vrm 仓库：<https://github.com/pixiv/three-vrm>
