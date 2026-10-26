# 01 · 系统架构

## 总览

```text
┌──────────────────────────── GabTwin.app ────────────────────────────┐
│                                                                     │
│  ┌──────────── Rust core（单进程，多线程） ────────────┐              │
│  │                                                     │              │
│  │  cpal 采集 ─► AEC/NS ─► 16k 重采样 ─► VAD ──┐       │              │
│  │      ▲                                       │       │              │
│  │      │ 播放缓冲 ◄─ TTS 解码 ◄─ 句子队列 ◄────┘       │              │
│  │      │            ▲                           │       │              │
│  │      │            └── TTS 引擎 ◄── 文本流 ◄──┼── LLM 客户端 ──► 云  │
│  │      │                                        │       │              │
│  │      └── AEC 参考 ─────────────────────────────┘       │              │
│  │                                                      │              │
│  │  会话状态机 · 打断控制 · 模型管理 · 配置/密钥          │              │
│  └───────────────────────┬──────────────────────────────┘              │
│                          │ IPC（Tauri commands + events）               │
│  ┌───────────────────────▼──────────────────────────────┐              │
│  │  WebView（TypeScript + Three.js）                     │              │
│  │  数字人渲染 · 口型/表情 · 字幕 · 设置界面              │              │
│  └──────────────────────────────────────────────────────┘              │
└─────────────────────────────────────────────────────────────────────┘
```

两条轴：

- **纵向**：Rust 负责实时与推理，WebView 负责画面与交互。
- **横向**：声音进 → 文字 → 回答 → 声音出，数字人只消费"音频 + 时间戳"。

## 模块职责

| 模块 | 职责 | 明确不负责 |
|---|---|---|
| `audio-in` | 设备枚举、采集、AEC/NS/AGC、重采样到 16 kHz | 识别 |
| `vad` | Silero 推理、语音段切分、端点判定 | 语义理解 |
| `asr` | 流式/整段识别，产出 partial 与 final | 断句策略 |
| `dialog` | 会话状态机、上下文、超时、取消 | 具体模型调用 |
| `llm` | 流式请求、SSE 解析、取消、重试、用量统计 | 句子切分 |
| `tts` | 文本→音频流、音色、viseme 时间轴 | 播放调度 |
| `audio-out` | 播放队列、淡入淡出、回声参考回灌 | 合成 |
| `avatar` | 口型/表情/动作状态机、渲染 | 语音 |
| `store` | 配置、密钥（Keychain）、模型缓存、日志 | — |

模块之间只通过事件与队列通信，不互相调用内部方法，方便单测与替换。

## 数据格式

- **内部音频**：20 ms 帧、f32、单声道、16 kHz。设备侧 48 kHz，在采集回调外重采样。
- **网络音频（远期）**：Opus 48 kbps。
- **WAV**：只在调试导出时使用，运行时不落盘。

## 事件协议

Rust → UI：

| 事件 | 载荷 | 时机 |
|---|---|---|
| `asr.partial` | `text`, `stablePrefix` | 流式中间结果 |
| `asr.final` | `text`, `durationMs` | 端点确认 |
| `dialog.state` | `idle / listening / thinking / speaking / interrupted` | 状态切换 |
| `llm.delta` | `text` | 逐 token |
| `tts.chunk` | `seq`, `pcm`（可选）, `visemes[]` | 每个音频包 |
| `error` | `code`, `message`, `recoverable` | 异常 |

UI → Rust：`start_listening`、`stop_listening`、`interrupt`、`set_device`、`set_voice`、`set_persona`。

`visemes[]` 的元素是 `{tMs, viseme, weight}`，`tMs` 相对该音频包起点。

### 实现现状（M0.5 · 语音输入）

第一个分项只用到"进"的一半，事件先合并成一条通道，减少样板：

| 通道 | 载荷 | 说明 |
|---|---|---|
| `session-event` | `{kind: state / device / level / partial / segment / metrics / warning / error, …}` | Rust → UI 的唯一通道 |
| 命令 | `start_listening` / `stop_listening` / `replay_sample` / `list_devices` / `app_info` | UI → Rust |

已在代码里执行的约束：

- **音频不过 IPC**：PCM 留在 Rust，UI 只收文本、电平与指标。
- Tauri 的事件名不允许点号，所以是 `session-event` 而不是 `session.event`。
- **播放时钟尚未引入**（还没有音频输出）；接 TTS 时必须补，否则口型会跟"包到达时间"漂移。
- 端点延迟按**音频时间**计（最后一个有声帧之后的静音时长），这样实时与回放同一口径。
- **LLM 事件**（`llm-thinking` / `llm-delta` / `llm-done` / `llm-cancelled` / `llm-error`）
  由应用层直接发；每句定稿（`segment`）触发一轮，`turn_id` 防止旧流污染新回合。

## 会话状态机

```text
Idle ──按下 / 检测到语音──► Listening ──端点──► Thinking ──TTS 首包──► Speaking
  ▲                            │                  │                    │
  └────── 超时 / 取消 ──────────┘                  │        打断（人声 > 200 ms）
  └──────────────────── 播放完毕且静音 ────────────┴────────────────────┘
```

| 迁移 | 条件 | 动作 |
|---|---|---|
| Idle → Listening | 按键或 VAD 触发 | 开流、清空缓冲 |
| Listening → Thinking | 静音 600 ms（可调）或语义端点 | 关闭 ASR 流，发 final |
| Thinking → Speaking | TTS 首包到达 | 启动播放，开始计时 |
| Speaking → Listening | 播放完 + 静音 400 ms | 回到监听 |
| Speaking → Interrupted | AEC 后的人声 > 200 ms | 停播、取消 LLM、清队列 |

状态机是唯一的真相来源；UI 只渲染 `dialog.state`，不自己推断。

## 线程模型

| 线程 | 内容 | 约束 |
|---|---|---|
| audio callback | 采集/播放，memcpy + 环形缓冲 | 硬实时：不分配、不加锁、不打印 |
| vad/asr | VAD 每 10 ms，ASR 每 320 ms | 软实时，可丢帧但要记录 |
| tokio runtime | LLM/TTS 网络、IPC、定时器 | 异步，不限 |
| render | WebView 60 fps | 系统调度 |

跨线程一律用 `ringbuf` 或 `crossbeam` 无锁队列；音频回调里禁止 `await`、`println!`、`String` 分配。

## 进程模型

- **主进程**：Rust core + Tauri 壳。
- **可选 sidecar**：Python（CosyVoice 等没有 Rust 绑定的 TTS），本地 gRPC/HTTP 流式通信。
  sidecar 崩溃不影响主进程，主进程降级到 Piper。
- **不把 Python 作为必需依赖**，否则安装包和分发都会变重。

## 延迟预算（目标，M1 Max）

| 阶段 | 目标 | 依据 |
|---|---|---|
| 采集 + 重采样 | 20–40 ms | 20 ms 帧 |
| VAD 判定 | 10–30 ms | Silero，512 样本窗 |
| ASR 首字 | 150–300 ms | 实测 RTF 0.025（5.58 s → 139 ms） |
| 端点 | 500–700 ms | 静音阈值，可调 |
| LLM 首 token | 300–600 ms | DeepSeek 云端，就近节点 |
| TTS 首包 | 150–300 ms | Piper 本地 / 云 TTS |
| 播放启动 | 30–60 ms | 2 帧缓冲 |
| **首响合计** | **1.0–1.6 s** | 串行相加；实际靠重叠更低 |

**重叠策略**：ASR final 一到就发 LLM；LLM 第一句（≥ 8 字或遇到标点）就发 TTS；
TTS 首包一到就开播，不等全文。三处重叠可省 300–600 ms。

## 目录规划（代码阶段）

```text
src/
├── core/            Rust
│   ├── audio/       采集、播放、AEC、重采样
│   ├── vad/         Silero
│   ├── asr/         sherpa-onnx 封装
│   ├── tts/         本地与云 TTS 的统一接口
│   ├── dialog/      状态机、上下文
│   └── llm/         流式客户端
├── app/             Tauri 入口、IPC、权限
└── ui/              TypeScript：渲染、字幕、设置
assets/
├── avatar/          glTF/VRM、贴图、动画
└── voices/          音色与 LICENSES.md
```

## 失败与降级

| 失败 | 行为 |
|---|---|
| LLM 超时 / 断网 | 提示，回 Idle；可选切本地模型（M4） |
| TTS 失败 | 降级到备用引擎；都失败则只显示文字 |
| 麦克风被占用 | 提示设备错误，允许重选 |
| 模型缺失/校验失败 | 引导重新下载，不进入 Listening |
| sidecar 崩溃 | 重启一次；再崩则禁用该音色 |
