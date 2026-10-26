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
| 模型 | `sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30`（对照模型见 [dev/02](02-asr-model-compare.md)） |
| 来源 | https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30 |
| 体积 | encoder.int8 161 MB + decoder 5 MB + joiner.int8 1 MB + tokens |
| 类型 | streaming transducer（zipformer），中文字符级 |
| 采样率 | 16 kHz（输入 48 kHz，sherpa-onnx 内部重采样） |
| 许可 | **待核**（icefall 训练代码 Apache-2.0；训练数据为 multi-zh-hans 合集，需逐项确认） |

落盘位置（`GABTWIN_HOME` 可覆盖）：

```text
~/Library/Application Support/GabTwin/models/streaming-zipformer-zh-int8-2025-06-30/
├── encoder.int8.onnx   5ac51e27981bb4dab01bb9be4958453ba50c3b61c063ddda0eab23fd3671aa4f
├── decoder.onnx        06522ad63cec0fdf6809f4e1db9bb4f7d710c34582e3b35db62ac60eccafac7e
├── joiner.int8.onnx    b34584dc6f561089e1d747fedebb3765f2caa72c927ef54d7ca55e5ae40a814b
├── tokens.txt          6193c7ea1c96d0d9a1e9652789b40d13a8a913b434a5451e93158f5a09fd6652
└── test_wavs/0.wav     模型自带样例，用于无麦克风回归
```

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

# 桌面应用
cargo run -p gabtwin-app

# 不点按钮也能自动跑一遍示例（用于截图 / 回归）
GABTWIN_AUTOREPLAY=1 cargo run -p gabtwin-app
```

环境变量：

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
| 模型冷加载 | 1.77 s | 进程内第一次创建识别器 |
| RTF | 0.071–0.094 | 5.61 s 音频 / 0.40–0.53 s 识别，2 线程 CPU（空闲 0.071，实时回放含补尾 0.094） |
| 首字 | 418–431 ms | 从起始点到第一段非空 partial（实时回放，6 次） |
| 字幕更新间隔 | 约 300 ms | 与模型 chunk 对齐，5.61 s 内 14 次 |
| 句尾定稿 | 静音 861 ms | 规则 0.8 s + 50 ms 音频块（861 = 800 + 61） |
| 采集 | 48 kHz 单声道 | 外置麦克风；采集 3.00 s 得到 3.05 s 音频 |
| 内存 | 峰值 RSS 533 MB | 含模型；`/usr/bin/time -l` 实测 |

口径说明（复现时看这三个数）：

- **首字**：起始点（第一个 RMS > 0.01 的帧）→ 第一段非空 partial 的墙钟时间。只在 `--realtime`
  或麦克风模式下有意义；非实时回放会得到几十毫秒的假数字。
- **静音**：最后一个有声帧之后累计的音频时长。它与回放速度无关，所以实时/非实时/麦克风同一口径。
- **RTF**：真正的识别耗时 / 音频时长，不含回放等待。**不是**墙钟/音频（那是 1.0 左右）。

## 界面

![实时字幕：浅色是稳定前缀，灰色是还在改的尾巴](shots/01-live-partial.png)

![定稿：句尾静音 861 ms 后落定，底部是首字/静音/RTF](shots/02-segment-final.png)

## 已知限制（按影响排序）

1. **没有 VAD 预筛**：现在所有音频都送识别器，安静时也在解码（CPU 空转）。M2 接 Silero VAD 后
   应当只在有语音时解码。
2. **端点只有规则**：0.8 s 静音。句中出现较长停顿时会切断（"我想想……"会被拆成两句）。
3. **没有标点**：模型输出不带标点；显示与后续 TTS 都需要一个标点/断句步骤。
4. **单流**：一次只能一个会话；停止靠旗标，线程退出时 Stream 才释放。
5. **模型许可未核**：见上表，商用前必须确认。
6. **首字约 420 ms 偏慢**：主要来自模型 chunk（320 ms 量级）。已与 26 MB 的 small-CTC 对照：
   小模型首字反而更慢（750 ms），只是 RTF 更低（0.025 vs 0.078），结论见 [dev/02](02-asr-model-compare.md)。
7. **设备热插拔未处理**：AirPods 进出会改采样率/路由，当前会直接报流错误，需要重启会话。
8. **没有长跑**：目前最长单次 6 秒；10 分钟连续采集、内存增长、环形缓冲溢出率都还没测。

## 下一步

- [x] 用同一段音频对比 small-CTC 模型的首字延迟与字错率（[dev/02](02-asr-model-compare.md)：
      大模型 CER 0.0% / 首字 450 ms；small-CTC 0.7% / 750 ms，默认仍用大模型）
- [ ] Silero VAD 预筛 + 只在有声时解码（降 CPU）
- [ ] 标点恢复（sherpa-onnx 自带 online punctuation 模型可直接接）
- [ ] 把 trace 落成 JSONL（现在 probe 已有 `--jsonl`，应用侧还没接）
- [ ] 应用侧去掉逐条 `eprintln!`，改成可选 `GABTWIN_DEBUG`
