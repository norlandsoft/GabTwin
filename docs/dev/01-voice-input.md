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
| 模型 | `sherpa-onnx-streaming-zipformer-ar_en_id_ja_ru_th_vi_zh-2025-02-10`（中英混说；对照见 [dev/02](02-asr-model-compare.md)） |
| 来源 | https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-ar_en_id_ja_ru_th_vi_zh-2025-02-10 |
| 体积 | encoder.int8 289 MB + decoder 32 MB + joiner.int8 8 MB + tokens + bpe |
| 类型 | streaming transducer（zipformer，chunk 16，9 语含中英） |
| 采样率 | 16 kHz（输入 48 kHz，sherpa-onnx 内部重采样） |
| 许可 | **待核**（来自 `yangb05/PengChengStarling`，仓库未标 license；中文专用的 zipformer 也待核） |
| 备选 | `streaming-zipformer-bilingual-zh-en-2023-02-20`（**Apache-2.0**，英文最好，但中文有重复字）；纯中文模型见 dev/02 |

落盘位置（`GABTWIN_HOME` 可覆盖）：

```text
~/Library/Application Support/GabTwin/models/streaming-zipformer-ar_en_id_ja_ru_th_vi_zh-2025-02-10/
├── encoder-epoch-75-avg-11-chunk-16-left-128.int8.onnx  f9001ed7a9e46d0294438c1a30cd7c72d1cc4bdd4e7880edbcda36f67081e32e
├── decoder-epoch-75-avg-11-chunk-16-left-128.onnx       7ebc63f34b21c8efb4a41a5a2eee7fe1448829ce0230ecc5369e67fc14d90d48
├── joiner-epoch-75-avg-11-chunk-16-left-128.int8.onnx   db88e3172323551abaa99b91b18fb422a27ea4a834fd0db10389f9478816f917
├── tokens.txt                                           784f24950f6bcce1b0021035632dd60fd4617ecd8ca0581ab57d7b39d77ba5ab
├── bpe.model                                            027731f33cff7266f2878c6fb7e478cf4af983962e311bb565112792794c13cd
└── test_wavs/{zh,en}.wav                                模型自带样例
```

换模型不改代码：`GABTWIN_MODEL_DIR=/path/to/model ./scripts/gabtwin-start.sh`。
中文专用模型（`zipformer-zh`）仍在，适合只说中文、想省 170 MB 内存的场景。

## 怎么跑

```bash
# 一次把模型下好（国内镜像、禁代理直连、并行、sha256 校验，可重复执行）
./scripts/fetch-model.sh zipformer-zh

# 设备清单（默认设备带 *）
cargo run -p gabtwin-probe -- --list-devices

# 无麦克风：用模型自带 WAV
cargo run -p gabtwin-probe -- --wav "$HOME/Library/Application Support/GabTwin/models/streaming-zipformer-zh-int8-2025-06-30/test_wavs/0.wav"

# 按真实语速回放：看"字幕跟着说话出现"的节奏
cargo run -p gabtwin-probe -- --wav ".../test_wavs/0.wav" --realtime

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
| 模型冷加载 | 1.89 s | 进程内第一次创建识别器 |
| RTF | 0.087–0.102 | 5.61 s 音频 / 0.49–0.57 s 识别，2 线程 CPU |
| 首字 | 343–519 ms | 从起始点到第一段非空 partial（中/英，多次） |
| 句尾定稿 | 规则 0.8 s 静音 | 实测静音 1.2–1.4 s，含端点判定与解码滞后 |
| 采集 | 48 kHz 单声道 | 外置麦克风；采集 3.00 s 得到 3.05 s 音频 |
| 内存 | 峰值 RSS 873 MB | 含模型；`/usr/bin/time -l` 实测（比纯中文模型多约 340 MB） |

两处与流式模型相关的小处理（都在代码里）：

- **预热 150 ms**：会话开始先喂 150 ms 静音。不预热时，用户开口的第一个词（尤其中英切换时）容易被吃掉；
  预热太长（≥600 ms 总静音）又会让模型多吐"嗯/于/X"这类开头噪声，150 ms 是实测的折中。
- **去掉开头语气词**：长静音后模型偶尔以"嗯/啊/哦"开头，显示前剥掉一个（只剥一个，且后面还有内容时）。
- **端点门控**：模型自己算的 trailing silence 会被预热静音骗到，出现"静音 0 ms 却断句"。
  现在只有检测到 **≥250 ms 真实静音** 才允许定稿（`min_silence_ms`），无停顿处不会被切开。

口径说明（复现时看这三个数）：

- **首字**：起始点（第一个 RMS > 0.01 的帧）→ 第一段非空 partial 的墙钟时间。只在 `--realtime`
  或麦克风模式下有意义；非实时回放会得到几十毫秒的假数字。
- **静音**：最后一个有声帧之后累计的音频时长。它与回放速度无关，所以实时/非实时/麦克风同一口径。
- **RTF**：真正的识别耗时 / 音频时长，不含回放等待。**不是**墙钟/音频（那是 1.0 左右）。

## 界面

![实时字幕：浅色是稳定前缀，灰色是还在改的尾巴](shots/01-live-partial.png)

![定稿：句尾静音 861 ms 后落定，底部是首字/静音/RTF](shots/02-segment-final.png)

![英文：九语模型整句识别，显示层把全大写收成正常大小写](shots/03-english.png)

## 已知限制（按影响排序）

1. **没有 VAD 预筛**：现在所有音频都送识别器，安静时也在解码（CPU 空转）。M2 接 Silero VAD 后
   应当只在有语音时解码。
2. **端点只有规则**：0.8 s 静音。句中出现较长停顿时会切断（"我想想……"会被拆成两句）。
3. **没有标点**：模型输出不带标点；显示与后续 TTS 都需要一个标点/断句步骤。
4. **单流**：一次只能一个会话；停止靠旗标，线程退出时 Stream 才释放。
5. **模型许可未核**：见上表，商用前必须确认。
6. **首字约 340–520 ms**：主要来自模型 chunk 与预热。已与 small-CTC、中英双语、纯中文三个模型对照，
   结论见 [dev/02](02-asr-model-compare.md)。
7. **设备热插拔未处理**：AirPods 进出会改采样率/路由，当前会直接报流错误，需要重启会话。
8. **长跑不够**：最长单次约 104 秒（一次真实测试）；10 分钟连续采集、内存增长、环形缓冲溢出率都还没测。
9. **中英混说仍弱**：整句英文已经很好（8 句全对），但"这个 bug""用 JSON"这类嵌在中文里的英文词
   错误率仍高（TTS 合成口音下 27.5%）。下一步用 hotwords / 领域词表补。

## 下一步

- [x] 用同一段音频对比 small-CTC 模型的首字延迟与字错率（[dev/02](02-asr-model-compare.md)：
      大模型 CER 0.0% / 首字 450 ms；small-CTC 0.7% / 750 ms，默认仍用大模型）
- [ ] Silero VAD 预筛 + 只在有声时解码（降 CPU）
- [ ] 标点恢复（sherpa-onnx 自带 online punctuation 模型可直接接）
- [ ] hotwords / 用户词表：把"过滤""GitHub""JSON"这类常错词加进去
- [ ] 把 trace 落成 JSONL（现在 probe 已有 `--jsonl`，应用侧还没接）
- [ ] 应用侧去掉逐条 `eprintln!`，改成可选 `GABTWIN_DEBUG`
