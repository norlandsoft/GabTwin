# dev/01 · 语音输入 + 实时字幕（第一个分项验证）

> 状态：**已跑通**。日期 2026-10-26。本文记录做了什么、怎么跑、实测数字与已知限制。

## 这一片验证什么

只验证两件事：

1. 麦克风采集在 macOS 上稳定、不丢帧、不卡主线程；
2. 流式识别能把"正在说的话"以约 3 次/秒的节奏变成屏幕上的文字。

明确**不**包含：VAD 常开、端点语义模型、AEC、TTS、LLM、数字人。
所以这一片跑通不代表"能对话"，只代表"耳朵和字幕"可用。

## 结构

```text
crates/
├── gabtwin-audio/      cpal 采集、环形缓冲、电平、降混
├── gabtwin-asr/        sherpa-onnx 流式识别封装（partial / segment）
├── gabtwin-session/    采集 + 识别 → 事件（不依赖 Tauri）
├── gabtwin-probe/      无头验证工具（WAV / 麦克风）
src-tauri/              Tauri 2 应用：命令 + 事件 + 窗口
ui/                     静态前端：大字号字幕、电平、延迟读数
```

分层的关键：`session` 不认识 Tauri，所以可以先在终端里把延迟和识别质量验掉，
再套 UI。UI 只消费事件，不参与音频。

## 模型

| 项 | 值 |
|---|---|
| 模型 | `streaming-paraformer-bilingual-zh-en`（中英，FunASR paraformer） |
| 来源 | https://huggingface.co/csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en |
| 体积 | encoder.int8 166 MB + decoder.int8 72 MB + tokens |
| 类型 | streaming paraformer（非自回归，encoder + decoder） |
| 采样率 | 16 kHz（输入 48 kHz，sherpa-onnx 内部重采样） |
| 许可 | **Apache-2.0** |
| 备选 | 中英 zipformer（英文最好、中文重复字）；纯中文 zipformer（中文 0%）；九语 zipformer（会把中文口音英文串成日/越语，不推荐）；small-CTC（省 CPU） |

落盘位置（`GABTWIN_HOME` 可覆盖）：

```text
~/Library/Application Support/GabTwin/models/streaming-paraformer-bilingual-zh-en/
├── encoder.int8.onnx  81a70226a8934e6ed92aa1d4fc486b428b5398e2f2619ed4897b7294cab90e9a
├── decoder.int8.onnx  f3cca9f77bb9d93c8fcbfb63ae617b6b1ee96818df3aa3b151c40658fe38594f
└── tokens.txt         59aba8873a2ed1e122c25fee421e25f283b63290efbde85c1f01a853d83cb6e6
```

示例音频放在 `$GABTWIN_HOME/samples/0.wav`，所有模型共用。

换模型不改代码：`GABTWIN_MODEL_DIR=/path/to/model ./scripts/gabtwin-start.sh`。

## 怎么跑

```bash
# 一次把默认模型下好（国内镜像、禁代理直连、并行、sha256 校验，可重复执行）
./scripts/fetch-model.sh

# 设备清单（默认设备带 *）
cargo run -p gabtwin-probe -- --list-devices

# 无麦克风：用共用示例 WAV
cargo run -p gabtwin-probe -- --wav "$HOME/Library/Application Support/GabTwin/samples/0.wav"

# 按真实语速回放：看"字幕跟着说话出现"的节奏
cargo run -p gabtwin-probe -- --wav "$HOME/Library/Application Support/GabTwin/samples/0.wav" --realtime

# 真实麦克风，10 秒
cargo run -p gabtwin-probe -- --mic --seconds 10 --verbose

# 桌面应用（自动编译、后台运行、防重复启动）
./scripts/gabtwin-start.sh
./scripts/gabtwin-stop.sh

# 不点按钮也能自动跑一遍示例（用于截图 / 回归）
GABTWIN_AUTOREPLAY=1 ./scripts/gabtwin-start.sh
```

启动脚本的行为：

- `gabtwin-start.sh`：检查模型 → `cargo build -p gabtwin-app` → `nohup` 后台启动 →
  写 `$GABTWIN_HOME/gabtwin-app.pid` → 日志追加到 `$GABTWIN_HOME/logs/app.log`。
  已经在跑（包括手动启动的实例）就直接退出，不会起第二个。
- `gabtwin-stop.sh`：**默认只停 PID 文件里记录的那个实例**（即 `gabtwin-start.sh`
  启动的）。手动启动的实例需要显式 `--all`，避免误杀同一台机器上另一份正在用的实例。
  先 TERM，5 秒内没退出再 KILL。
- 日志带时间戳；start / stop 脚本各留一行标记，应用收到 SIGTERM / SIGINT 时会写
  `收到终止信号 N，退出`。也就是说，"它怎么停的"现在能从日志里查出来。

环境变量（脚本原样传给应用）：

| 变量 | 作用 |
|---|---|
| `GABTWIN_HOME` | 根目录，默认 `~/Library/Application Support/GabTwin` |
| `GABTWIN_MODEL_DIR` | 直接指定模型目录 |
| `GABTWIN_DEVICE` | 指定输入设备名 |
| `GABTWIN_ENDPOINT` | 句尾静音秒数（默认 0.8） |
| `GABTWIN_SAMPLE_WAV` | 示例按钮使用的 WAV |
| `GABTWIN_AUTOREPLAY` | 启动后自动回放示例 |

## 实测（M1 Max，32 GB，macOS 27）

| 指标 | 值 | 说明 |
|---|---|---|
| 模型冷加载 | 0.72 s | 进程内第一次创建识别器 |
| RTF | 0.169–0.185 | 5.61 s 音频 / 0.95–1.04 s 识别，2 线程 CPU |
| 首字 | 250–550 ms | 从起始点到第一段非空 partial（中/英，多次） |
| 句尾定稿 | 规则 0.8 s 静音 | 实测静音 0.7–1.2 s，含端点判定与解码滞后 |
| 采集 | 48 kHz 单声道 | 外置麦克风；采集 3.00 s 得到 3.05 s 音频 |
| 内存 | 峰值 RSS 530 MB | 含模型；`/usr/bin/time -l` 实测 |

与流式模型相关的处理（都在代码里）：

- **去掉开头语气词**：偶尔以"嗯/啊/哦"开头，显示前剥掉一个（只剥一个，且后面还有内容时）。
- **预热默认 0**：曾用 150 ms 预热来保住英文首词，但它会被模型算成"句尾静音"，在没有停顿处断句。
- **端点不门控（重要）**：为了压掉假断句，曾经在"真实静音不足 250 ms"时忽略 endpoint。
  结果 sherpa 的在线流一旦 endpoint 为真就不再 ready，**整条流永久卡死**（14:37 那次 40 秒没有输出）。
  现在的规则是：模型报 endpoint 就 reset；不要"忽略但不动"。

口径说明（复现时看这三个数）：

- **首字**：起始点（第一个 RMS > 0.01 的帧）→ 第一段非空 partial 的墙钟时间。只在 `--realtime`
  或麦克风模式下有意义；非实时回放会得到几十毫秒的假数字。
- **静音**：最后一个有声帧之后累计的音频时长。它与回放速度无关，所以实时/非实时/麦克风同一口径。
- **RTF**：真正的识别耗时 / 音频时长，不含回放等待。**不是**墙钟/音频（那是 1.0 左右）。

## 界面

![实时字幕：浅色是稳定前缀，灰色是还在改的尾巴](shots/01-live-partial.png)

![定稿：句尾静音 861 ms 后落定，底部是首字/静音/RTF](shots/02-segment-final.png)

![英文：能识别，但会漏词——paraformer 的短板，见 dev/02](shots/03-english.png)

## 已知限制（按影响排序）

1. **没有 VAD 预筛**：现在所有音频都送识别器，安静时也在解码（CPU 空转）。M2 接 Silero VAD 后
   应当只在有语音时解码。
2. **端点只有规则**：0.8 s 静音。句中出现较长停顿时会切断（"我想想……"会被拆成两句）。
3. **没有标点**：模型输出不带标点；显示与后续 TTS 都需要一个标点/断句步骤。
4. **单流**：一次只能一个会话；停止靠旗标，线程退出时 Stream 才释放。
5. **英文会漏词**：英文 10.9%、中英混说 25.5%，比中英 zipformer（英文 2.2%）差；
   要英文优先可切 `GABTWIN_MODEL_DIR=.../streaming-zipformer-bilingual-zh-en-2023-02-20`。
6. **首字 250–550 ms**：比中英 zipformer（385 ms）略慢，但中文更准；对照见 [dev/02](02-asr-model-compare.md)。
7. **设备热插拔未处理**：AirPods 进出会改采样率/路由，当前会直接报流错误，需要重启会话。
8. **长跑不够**：最长单次约 104 秒（一次真实测试）；10 分钟连续采集、内存增长、环形缓冲溢出率都还没测。
9. **中英混说仍弱**：TTS 合成口音下 25.5%；"这个 bug""用 JSON"这类嵌在中文里的英文词仍会错。
   下一步用 hotwords / 领域词表补。

## 下一步

- [x] 用同一段音频对比 small-CTC 模型的首字延迟与字错率（[dev/02](02-asr-model-compare.md)：
      大模型 CER 0.0% / 首字 450 ms；small-CTC 0.7% / 750 ms，默认仍用大模型）
- [ ] Silero VAD 预筛 + 只在有声时解码（降 CPU）
- [ ] 标点恢复（sherpa-onnx 自带 online punctuation 模型可直接接）
- [ ] hotwords / 用户词表：把"过滤""GitHub""JSON"这类常错词加进去
- [ ] 把 trace 落成 JSONL（现在 probe 已有 `--jsonl`，应用侧还没接）
- [ ] 应用侧去掉逐条 `eprintln!`，改成可选 `GABTWIN_DEBUG`
