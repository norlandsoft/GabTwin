# dev/04 · TTS：把回复说出来

> 状态：**已跑通**（2026-10-26）。本机 VITS，句级流式合成，播放期间半双工。
> 音色质量与英文单词支持是已知短板。

## 结构

```text
LLM 流式 delta
   → ClauseSplitter（首句 ≤6 字，之后按标点/30 字）
   → TTS worker（VITS，sherpa-onnx OfflineTts）
   → 重采样 16k → 48k
   → Speaker（cpal 输出 + 队列）
```

- 首句故意很短：6 个字大约 0.4 s 合成，先把声音放出来，后面的句子继续合成。
- 太短的碎片（比如硬切出来的"说。"）不会单独合成，会攒到下一句。
- 播放期间 `mute` 置位，会话层**丢弃麦克风音频**并重置识别器——否则麦克风会把 TTS
  自己的声音当成用户说话，形成自问自答。代价是**说话时不能插话**（M2 的 AEC 再解决）。

## 模型

| 项 | 值 |
|---|---|
| 模型 | `matcha-icefall-zh-en`（中英 Matcha + vocos 16k 声码器） |
| 体积 | model-steps-3 72 MB + vocos 54 MB + lexicon/FST |
| 采样率 | 16 kHz（输出 48 kHz，线性重采样） |
| 许可 | **待核**（来自 ModelScope `matcha_tts_zh_en_20251010`，未标 license） |
| 文本规则 | date-zh / number-zh / phone-zh 三个 FST |
| 备选 | VITS zh-ll（快、单音色、跳过英文词）；Kokoro v1.1（更自然但 RTF≈1，不可流式） |

## 实测（M1 Max）

| 指标 | 值 |
|---|---|
| 模型加载 + 预热 | 约 0.3–1 s（启动时） |
| 首句合成（6–15 字） | **65–100 ms**（Matcha） |
| **句末 → 第一个声音** | **280 ms**（mock LLM，不含真实网络） |
| 首句播放 | 约 1 s 音频 |
| RTF | 0.17–0.33 |
| 输出 | 48 kHz，队列播放 |

真实链路再加 LLM 的首 token（0.3–0.6 s），预计 **说完到出声 1.0–1.5 s**。

## 配置

| 变量 | 作用 |
|---|---|
| `GABTWIN_TTS_MODEL_DIR` | TTS 模型目录（默认 `$GABTWIN_MODELS_DIR/tts/matcha-zh-en`） |
| `GABTWIN_ENGINE=streaming` | 只影响句末定稿，不影响 TTS |

## 单独验 TTS

```bash
cargo run -p gabtwin-probe -- --tts "$ROOT/models/tts/vits-zh-ll" \
  --text "你好，我是 GabTwin。" --out /tmp/tts.wav
afplay /tmp/tts.wav
```

## 断句

TTS 是逐句合成再拼接的，切得不对就会"在句子中间断气"。规则：

1. **句子优先**：等到 `。！？；` 再切；首句最多 40 字、之后 60 字。
2. **超长句**才退到范围内**最后一个逗号**，不是第一个；再不行才硬切。
3. **引号/括号里的内容不切**：等 `」` 闭合再切，最多多等 40 字。
4. **流式安全**：delta 只到"半句 + 逗号"时**不切**，等下一段；否则会出现
   `你刚才说的是「……` / `大」。` 这种断句。
5. LLM 提示词要求"口语短句、句末带标点"，减少长句。

试听分句效果：

```bash
cargo run -p gabtwin-probe -- --tts "$ROOT/models/tts/matcha-zh-en" \
  --split --text "嗯，我听到了。你刚才问的是现在几点。现在大概是晚上八点四十，你想知道具体的时间吗？" \
  --out /tmp/split.wav
```

应用日志里每句会打 `TTS 句：…`，出问题时直接看切点。

## 多音字与自定义读音

Matcha 的 `lexicon.txt` 是"词 → 带声调数字的拼音"，已有 6.8 万词条；读错基本来自两类：

1. **词典里没有的词**（专名、新词、部分短语），前端退回按单字读；
2. **语法助词**：`地`、`得` 的单字默认是 `di4`、`de2`，所以"慢慢地走""跑得快"
   必错（应为 `de5`）。

修法是一个**合并词典**，优先级：模型词典 < 内置修正 < 用户词条。

- 内置修正：`crates/gabtwin-tts/data/corrections.txt`（地/得 短语、地方、看着、
  教室、颜色、记得…）。
- 用户词条：模型目录下 `user_lexicon.txt`，一行一个 `词 拼音`，声调用数字、
  轻声用 `5`：

  ```text
  慢慢吞吞 man4 man4 tun1 tun1
  伽罗 jia1 luo2
  ```

  改完重启应用生效（启动时合并成 `lexicon.merged.txt`）。
- 仍然无法自动判断的上下文歧义（"行长" vs "行走"、"重庆" vs "重复"）靠词条覆盖；
  要做到完全自动，需要 G2P 消歧模型（pypinyin 短语表已基本被 Matcha 词典覆盖）。

试听：

```bash
afplay /tmp/matcha-poly.wav   # 慢慢地 / 地方 / 跑得快 / 说得对
```

## 音色候选（2026-10-26 实测）

同一句话 "你好，我是你的数字伙伴。今天想聊点什么？"：

| 引擎 | 采样率 | 合成耗时 | RTF | 说明 |
|---|---|---|---|---|
| VITS zh-ll（备选） | 16 kHz | 780 ms | 0.27 | 快，但音色偏机械；英文词跳过 |
| **Matcha zh-en（当前默认）** | 16 kHz | 100–340 ms | 0.04–0.11 | 很快，中英混读；音色待你听 |
| Kokoro v1.1（sid 3） | 24 kHz | 3.7–4.0 s | ≈1.0 | 更自然，但**无法实时流式**，未采用 |

试听：

```bash
afplay /tmp/tts-vits-sample.wav     # VITS（备选）
afplay /tmp/matcha-sample.wav       # Matcha（当前默认）
afplay /tmp/kokoro-3.wav            # Kokoro（更自然但太慢）
afplay /tmp/say-tingting.wav        # macOS Tingting（零依赖）
```

`Matcha` 的模型在 `/opt/GabTwin/models/tts/matcha-zh-en/`；
它的声码器 `vocos-16khz-univ.onnx` 要从 GitHub release 单独下（下载慢，已在本机）。
选定后把 `GABTWIN_TTS_MODEL_DIR` 指过去即可，应用会自动识别 VITS / Matcha / Kokoro。

## 插话（barge-in）

- **耳机**：输出设备被识别为 Headphones/Headset 时，播放期间**不闭麦**；
  一旦识别到你开口（partial），立即停止播放、取消这一轮回复，你的话进入新一轮。
- **外放**：仍是半双工。麦克风会收到 TTS 自己的声音，没有 AEC 时任何"插话检测"都会
  被自己的回声触发。要真正外放插话，需要 AEC（WebRTC APM 或 macOS VPIO），是下一步。
- 任何情况下都可以点「停止」立即静音。

## 已知限制

1. **英文/生词会被跳过**：中文 lexicon 里没有的词（GabTwin、API、bug）直接忽略，不会念。
2. **只有一个人声音色**；3 个角色各自的声音还没做。
3. **外放时听不见你**：半双工；戴耳机可以直接插话（见上）。外放插话要等 AEC。
4. **没有视觉层**：数字人/口型已移出范围（2026-10-30），只保留声音。
5. 首句可能在短语中间断开（"对我做了介绍" / "啊，那么…"），听感上是一个短停顿。
6. 输出设备固定为系统默认，不能选。
7. 线性重采样 16k→48k，没有抗混叠；语音可接受，音乐不行。

## 下一步

- [ ] 3 个角色 × 3 个音色（VITS 多说话人 / Kokoro / Melo 评估）
- [ ] 英文词的中文读法（API → "A P I" 或"接口"），或换中英混合 TTS
- [x] 数字人/口型移出范围，不再排期
- [ ] AEC + 真打断（M2）
- [ ] 播放设备选择


## Breeze TTS 2（默认，2026-10-28）

- sidecar：`scripts/breeze_tts_sidecar.py`，加载 `models/tts/breeze-tts2-4bit` 一次，
  HTTP `:8799` 返回 24 kHz WAV；
- 参考音：`$GABTWIN_HOME/tts/index-ref.wav`（GLM 干净音色）+ 逐字文字；
- 启动脚本默认 `GABTWIN_TTS=breeze` 并自动拉起 sidecar；
- 回 IndexTTS：`GABTWIN_TTS=indextts`；回本地 Matcha/ZipVoice：`GABTWIN_TTS=local`；
- 实测首音约 4.2 s（IndexTTS 约 8 s），后续每句仍受 Breeze RTF 限制；
- **许可证：Breezeblue 研究/非商用**，商用需单独授权。


## 按 LLM 最终结果合成（默认）

`GABTWIN_TTS_FINAL=1`（默认）：等 LLM 完整返回后，用最终文本一次性送 TTS。
这样念出来的内容与 LLM 的最终回答严格一致，不会把中途分片或旧轮次混进来；
不打开 `GABTWIN_TTS_OVERLAP` 时，代价是首音要等 LLM 结束（本地 9B 通常 0.5–2 s）。

`GABTWIN_TTS_FINAL=0` 恢复边收 delta 边合成，首音更早，但以流式文本为准。


### 流式首句（`GABTWIN_TTS_OVERLAP=1`）

默认 `FINAL=1` 的代价是首音要等 LLM 全文。打开 `OVERLAP=1` 后：

- LLM 边输出边攒，第一句完整（`。！？；`）就立刻送 TTS；
- 第一句之后的内容不再切，等 LLM 结束后**整段一次**送 TTS；
- 整条回复最多一个拼接点（第一句之后），不是每句一个；
- 首句上限 `GABTWIN_TTS_OVERLAP_MAX_FIRST`（默认 30 字）；只有超长句才退到最后一个逗号；
- 实测（Breeze，184 字回复）：首音 **1.98 s**（之前整段 6.68 s），日志里只有两条 `TTS 句`。

第一句仍然来自流式 delta，`strip_stage_directions` 目前只作用于最终全文；如果发现 AI 把括号里的舞台提示念出来，需要把首句也过一遍清理。


### 取消过期请求

- Breeze sidecar 用 `stream=True` + 全局 `REQUEST_ID`：新请求到来时，旧请求在下一个
  chunk 处退出，不再一直占着模型；
- TTS worker 在合成返回后**再次检查 generation**，旧轮次的音频直接丢弃；
- `Flush` 也带 generation，旧轮次的 flush 不会恢复麦克风/干扰新一轮。

之前首句 4–7 s 的主因不是 Breeze 本身（实测 RTF 0.6），而是旧请求占着 sidecar；
取消后首句合成约 2.8 s。


### 流式 PCM（默认）

- Breeze sidecar 的 `/tts_stream` 用 HTTP/1.1 chunked 边生成边推 s16le PCM（24 kHz）；
- 应用 `RemoteTts::stream_pcm` 边读边重采样到扬声器，首块不再等整句；
- 实测：sidecar 首块 0.35 s（长句），应用内首句首块约 1.9 s，
  首音从 4.18 s 降到 **3.09 s**；
- `GABTWIN_TTS_STREAM=0` 可回退整句 WAV；
- IndexTTS 等没有 `/tts_stream` 的引擎自动回退整句。


## Qwen3-TTS 1.7B Base（默认，2026-10-28）

- sidecar：`scripts/qwen3_tts_sidecar.py`，端口 8800，`/tts` + `/tts_stream`；
- 模型：`models/tts/qwen3-tts-1.7b-base-8bit`（软链到 HF 缓存的
  `mlx-community/Qwen3-TTS-12Hz-1.7B-Base-8bit` 快照）；
- 克隆：`--ref` + `--ref-text` 用 `$GABTWIN_HOME/tts/index-ref.wav`（你的音色）；
- 24 kHz，RTF 约 0.5；实测首块 0.26–0.9 s，应用内首音 **1.77 s**；
- 切回 Breeze：`GABTWIN_TTS=breeze`；IndexTTS：`GABTWIN_TTS=indextts`。
- 注意：Base 模型没有显式情绪指令；要情绪控制需要 CustomVoice（不克隆你的音色）。


### Qwen3-TTS 的参考音与长句

- Base 模型的韵律跟随参考音：默认用 GLM 语料的 **高兴** 片段
  `workspace/tts/qwen-ref-lively.wav`（c0041）做参考，避免中性参考导致的“死气沉沉”；
- 想换情绪就换参考音（c0042/c0043 高兴、c0057 惊讶、c0063+ 悲伤等），并同步
  `--ref-text`；Base 没有 `instruct` 情绪控制；
- **长文本**：整段 100 字一次生成需 22.2 s（RTF 0.81），后半段容易劣化；
  应用按 **40 字 + 逗号优先** 切分后每段 RTF 0.42–0.53，明显更稳。


### 标点级断句（默认）

- **句子标点**（。！？；）优先切；
- **逗号**（，、：）在引号外、且累计 ≥10 字（首句）/≥12 字（后续）时直接切；
  不再等 40 字硬上限，避免“虚拟对话的 / 人越来越少了”这种切词；
- 首句硬上限 30 字，后续 40 字，仅作为无标点时的兜底；
- 每段独立送 TTS 合成/播放，长句不会一次性生成，后段失真明显减少。

实测应用内首音约 1.25 s（首块 0.31 s）。


### 缺省女声（不使用用户参考音）

Qwen3-TTS Base **无参考音时是随机说话人**（实测 f0 196/242/141 Hz，会出现男声），
不能直接当稳定缺省音色。当前用一段 Qwen 自己生成的固定女声做参考：

```text
workspace/tts/qwen-ref-default-female.wav   （3.5 s，f0 ~230–247 Hz）
--ref-text "早上好，今天天气不错，适合出去走走。"
```

它不包含用户声音，音色固定；要真正的“无参考预设女声”，需要
`Qwen3-TTS-12Hz-1.7B-CustomVoice-8bit`（3.08 GB，预设 speaker + 情绪），
或退回 Matcha 内置女声。

标点断句：首句逗号阈值 8 字、后续 12 字，句子标点优先；首句硬上限 30、后续 40。


### 对话程序使用 gyy 参考音

- 参考音：`workspace/tts/gyy-ref.wav`（`/opt/Workspace/gyy.mov` 提取的 8.85 s 真实语音）；
- 参考文本：`workspace/tts/gyy-ref.txt`；
- Qwen3-TTS 1.7B Base 本地克隆，GLM 不参与运行时；
- 实测首音 1.49 s（首块 0.38 s）；
- GLM 生成的 50 条 15 s 参考在 `workspace/dataset/glm/gyy/refs/`，可用于训练或对比；
- 如果觉得真实 8.85 s 参考的底噪/停顿影响音色，可把 sidecar 的 `--ref` 换成
  `refs/g001.wav`（GLM 干净版）和对应文本。


### 整段一次生成（默认）

`GABTWIN_TTS_ONE_SHOT=1`（默认）：LLM 最终回答整段交给 Qwen3-TTS，一次采样、
内部流式播放。

- 一个回答只有一次采样，句间声调/音量天然一致，没有拼接点；
- 97 字实测：`stream=False` 16.16 s、`stream=True` 17.84 s，ASR 都完整；
- 播放环形缓冲仍有 6 s 背压，不会因生成快而丢样本；
- 如果超长回答仍出现音色漂移，可设 `GABTWIN_TTS_ONE_SHOT=0` 回到
  分段 + 会话音色锁定 + 淡入淡出。

### 语速与标点停顿### 语速与标点停顿

- `GABTWIN_TTS_SPEED=0.8`（比 1.0 慢 20%）；
- 标点停顿：只扩展 ≥300 ms 的真实静音，每个追加 **500 ms**
  （`GABTWIN_TTS_PAUSE_MIN` / `GABTWIN_TTS_PAUSE_EXTRA`）；
- 段间 350 ms + 标点 500 ms，整体节奏更舒缓。
- 每段首尾 40 ms 淡入 / 60 ms 淡出；每个标点停顿后 30 ms 缓入。

### 标点停顿（默认开）

- sidecar 检测音频里的真实静音，只有**长度 ≥300 ms** 的静音（标点/换气）才追加 **250 ms**；
- 低于 300 ms 的词间缝隙不动，避免流式分块边界被误判；
- 该逻辑同时用于 `/tts` 和 `/tts_stream`，保持一次生成的音色；
- 配置：`GABTWIN_TTS_PAUSE=1`、`GABTWIN_TTS_PAUSE_MIN=300`、
  `GABTWIN_TTS_PAUSE_EXTRA=250`；
- 语速：`GABTWIN_TTS_SPEED=0.9`。


### 播放背压

分段合成（RTF ~0.5）会比 1× 播放快；如果不加限制，10 s 播放环形缓冲会被塞满，
从某一帧开始持续丢样本，听起来就是“突然变乱、断字”。

- TTS worker 在每段开始前检查队列积压：`queued_ms - played`；
- 积压超过 **6 s** 就等待播放追上，再合成下一段；
- 配置：`GABTWIN_TTS_BACKLOG_MS=6000`；
- 日志里不应再出现 `[audio] 播放缓冲满，丢弃 ...`。


### 会话音色锁定

分段生成时每段独立采样，音调会飘。现在同一对话内：

- 前 1–2 段仍用 gyy 参考音；
- sidecar 累计这些段的音频，达到 **3 秒**后写成
  `/tmp/gabtwin-session-ref.wav`，作为后续所有段的参考音；
- 后续段仍独立生成（长文本稳定），但音色/音调跟随第一段；
- 生成温度降到 **0.7**，减少段间随机差异；
- 清空记忆时 app 调 `POST /voice_reset`，下个对话重新锁定；
- 日志：`[tts] session ref locked: 5.12s`。


### 分段拼接淡入淡出

每段音频首尾做 **10 ms 淡入 / 20 ms 淡出**，再做标点停顿和 250 ms 段间静音，
消除拼接处的爆音/过渡杂音。

一次性生成本身：97 字实测 `stream=False` 16.16 s、`stream=True` 17.84 s，
ASR 都完整；如果仍想试整段生成，可设 `GABTWIN_TTS_ONE_SHOT=1`。
之前长文“混乱”的主因是播放环形缓冲满丢样本（已加 6 s 背压）。


### 低频底音

Qwen 输出带一点低频/直流分量，流式路径原本没有走应用里的 80 Hz 高通。
sidecar 现在对每段先做 **90 Hz 二阶高通**，再做淡入淡出、标点停顿和段间静音；
`/tts` 与 `/tts_stream` 都生效。


## Breeze 情绪稳定（2026-10-29，已回退）

> 实机试用后回退：`GABTWIN_BREEZE_*` 稳定模式（greedy + instruct + 逐 0.25 s AGC）
> 出现明显卡顿和噪音失真，当前**未启用**；下面仅保留实验记录。

问题：Breeze 默认 `temperature=0.9`、无 instruct 时，同一段高兴文本的 f0 会随机
拔高——实测中位 237 Hz、p95 386 Hz（+8.4 半音）、瞬时最高 500+ Hz，听起来就是
“突然很尖锐”。换 seed / 降温度并不能解决，反而会整体移调（中位 313 Hz）。

当前稳定模式：

- `GABTWIN_BREEZE_INSTRUCT`：英文 instruct，要求“高兴但克制，音高范围、音量、音色
  与参考音一致，不要拔高、不要喊”；
- `GABTWIN_BREEZE_CFG_SCALE=2.0`：让 instruct 真正生效；
- `GABTWIN_BREEZE_TEMPERATURE=0.0`：greedy，去掉随机选到高音调的可能性；
- `GABTWIN_BREEZE_AGC=1`：sidecar 内慢速自动增益，把不同情绪的段拉到约 −20 dBFS；
- 少于 8 个字的短句（如开场的“你好”）不套稳定模式，避免 greedy 在极短文本上
  不收敛、生成几十秒。

实测（同一段“高兴”）：

| | f0 中位 | p95 | 最高 | p95 半音 | RMS | 质心 |
|---|---:|---:|---:|---:|---:|---:|
| 原设置 | 237 | 386 | 500+ | +8.4 | −24.2 | 2658 |
| 稳定模式 | **203** | **280** | **314** | **+5.6** | −21.0 | 2853 |

- 三段不同情绪（高兴×2、中性）的整段 RMS 落在 −21.0 / −21.9 / −22.7 dB，差约 1.7 dB；
- 首块 **0.74 s**，9.12 s 音频总合成 8.4 s（RTF≈0.92），sidecar 启动约 3 s；
- 代价：greedy 可能比采样版“平”一点；如果觉得太收，把
  `GABTWIN_BREEZE_TEMPERATURE` 调到 `0.2`、`CFG_SCALE` 调到 `2.5` 试折中。

回退：把 `GABTWIN_BREEZE_TEMPERATURE` 改回 `0.9`，清空
`GABTWIN_BREEZE_INSTRUCT` / `GABTWIN_BREEZE_CFG_SCALE`，或把 `GABTWIN_BREEZE_AGC=0`，
重启生效。

对照试听：`workspace/eval/breeze-stability/compare.html`。


## 感叹号降级（2026-10-29，当前生效）

日志里抓到了真正的触发点：

```text
[18:41:27] TTS 句：狸猫！
```

Breeze 把孤立的“狸猫！”合成成了 420 Hz 左右的一声喊（RMS −14.5 dB）。同一句改成
“狸猫。”或“狸猫，那种特别瘦的野猫……”后，f0 回到 176–180 Hz、RMS 降到 −29 dB 左右。

现在在**送 TTS 之前**统一处理（显示文本不变）：

- `！` / `!` → `，`；
- 连续逗号合并；
- 结尾的逗号/顿号去掉，补 `。`；
- 问号保留。

实现：`gabtwin-tts::sanitize_for_tts`，`TtsState::speak` / `finish_overlap` / `flush`
都会先过一遍；`OverlapSplitter` 的文本清理也会把 `！` 降成 `，`，所以“狸猫！”
不会再被切成一个孤立的短段。首段最小字数另由
`GABTWIN_TTS_OVERLAP_MIN_FIRST`（默认 12）兜底。

代价：感叹号带来的“拔高”没有了，情绪主要靠用词和语气；如果以后觉得太平，
可以只对长句保留 `！`，但短感叹（≤8 字）必须降级。

## 音调区间限制（2026-10-30，当前默认）

Breeze 的情绪句会出现整句中位 F0 大幅漂移（实测同一音色下 164 / 273 / 298 Hz），
感叹号降级只能处理短感叹，不能处理整句情绪。现在在 sidecar 里加一道**整句移调**：

- 用 `librosa.yin` 估每句的中位 F0；
- 目标 `GABTWIN_BREEZE_PITCH_TARGET=220` Hz；
- 单句最多移调 `GABTWIN_BREEZE_PITCH_MAX_SHIFT=4.0` 个半音；
- 句内偏离按 `GABTWIN_BREEZE_PITCH_COMPRESS=0.65` 压窄，并夹在
  `GABTWIN_BREEZE_PITCH_RANGE=3.0` 个半音内；
- `GABTWIN_BREEZE_PITCH=0` 关闭；
- 用 `psola`（Praat PSOLA）按目标 F0 轮廓重合成，保留共振峰，不改变时长。

实测三句（高兴/低落/中性）在开启后中位 F0 为 **219.8 / 219.8 / 217.3 Hz**，
p5–p95 跨度 2.9–5.2 个半音；未开启时中位可差 130 Hz 以上。

注意：`/tts_stream` 现在会先合成完整句、移调、再分块返回，首音会比纯流式晚
（实测 6 秒音频约 2.7 秒返回，含合成与 PSOLA）。
