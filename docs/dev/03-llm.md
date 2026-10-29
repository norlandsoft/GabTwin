# dev/03 · LLM 接话（文字流式回复）

> 状态：**已跑通**（2026-10-26）。本地 mock 验证了完整链路；真实 DeepSeek key 的
> 首 token 数字待你用真 key 测一次。

## 目标

识别定稿之后，让模型**马上**接一句话：先把字流出来，不做"转圈等全文"。
TTS 是下一步，这一片只做文字。

## 流程

```text
麦克风 → 流式字幕（paraformer）
            │
            └─ 句末定稿（SenseVoice，带标点）
                    │  Segment 事件
                    ▼
              对话上下文（system + 最近 N 条）
                    │
                    ▼
              POST /chat/completions（stream=true）
                    │  SSE delta
                    ▼
              UI 逐字追加（llm-delta）
                    │
                    ▼
              llm-done（首 token / 总耗时）
```

## 关键决定

| 问题 | 做法 | 原因 |
|---|---|---|
| 什么时候发 | **句末定稿后**，不是 partial | partial 还会改，发出去会浪费且回复对不上 |
| 怎么等 | **SSE 流式**，来一个字显示一个字 | 首 token 通常 300–600 ms，比等全文快得多 |
| 用户又说了一句 | **取消上一轮**（turn_id + 取消旗标） | 新语音优先；旧 delta 到 UI 后被 turn_id 丢弃 |
| 上下文 | system + 最近 12 条、≤4000 字 | 足够闲聊，避免长上下文拖首 token |
| 口语化 | system prompt 强制短句、无 Markdown、无列表 | 后面要接 TTS，书面格式会被念出来 |
| 没有 key | 只显示识别结果，并把原因显示出来 | 不静默失败 |
| 错误 | 401/超时/空回复都进 `llm-error` | 用户可以看见，不会以为"它不理我" |

## 配置

推荐把 key 放进仓库根目录的 `.env`（已被 gitignore，权限 600）：

```bash
cp .env.example .env
chmod 600 .env
# 编辑 .env，填入 DEEPSEEK_API_KEY=sk-...
./scripts/gabtwin-start.sh
```

也支持环境变量（优先级高于 `.env`）：

```bash
export DEEPSEEK_API_KEY=sk-...          # 或 GABTWIN_LLM_KEY
export GABTWIN_LLM_BASE=https://api.deepseek.com   # 默认
export GABTWIN_LLM_MODEL=deepseek-chat             # 默认
```

应用启动时先读当前目录的 `.env`，再读 `$GABTWIN_HOME/.env`；已经存在的环境变量不会被覆盖。
没有 key 也能跑识别；右下角会写"未配置 LLM key"。

## 事件

| 事件 | 载荷 | 说明 |
|---|---|---|
| `llm-thinking` | `turn_id` | 已发起，等待首 token |
| `llm-delta` | `turn_id`, `text` | 增量文本 |
| `llm-done` | `turn_id`, `text`, `first_token_ms`, `total_ms` | 完成 |
| `llm-cancelled` | `turn_id` | 被新语音或停止取消 |
| `llm-error` | `turn_id`, `message` | 失败原因 |

## 不用真 key 也能验证链路

```bash
python3 scripts/mock-llm.py &        # 本地 SSE，回显你这句话
DEEPSEEK_API_KEY=dummy GABTWIN_LLM_BASE=http://127.0.0.1:8787/v1 \
  ./scripts/gabtwin-start.sh
```

![对话：灰色是你说的话，大字是流式回复](shots/04-chat.png)

## 延迟预算（到首 token）

| 阶段 | 实测/预期 |
|---|---|
| 句末定稿（SenseVoice） | 约 200–400 ms |
| 请求 + 首 token（DeepSeek） | 预期 300–600 ms（待真 key 实测） |
| **合计** | **约 0.5–1.0 s** |

UI 的"首 token"读数就是这一段；mock 下是 1 ms，不要拿它当真实网络数字。

## 已知限制

1. **没有持久化**：重启应用对话历史清零；也没有"记忆"。
2. **只有一个默认人设**；3 个角色还没接（等 TTS 音色）。
3. **key 在环境变量里**，还没进 Keychain。
4. 没有重试与限流；DeepSeek 偶发 429 会直接显示错误。
5. 没有内容安全过滤；自用可接受，对外要先补。
6. 用户在一句话里停顿会被切成两轮（端点 0.8 s 决定），模型可能把半句当完整问题。

## 下一步

- [ ] TTS：把回复按句切分、逐句合成、边合成边播（首句 ≤10 字）
- [ ] 3 个角色：人设文件 + 各自音色
- [ ] key 进 Keychain，设置页可填
- [ ] 429/超时重试一次；失败时说一句固定话术
- [ ] 上下文摘要（长对话）

---

## 本地小模型（历史：Qwen2.5-7B，MLX）

> 2026-10-27：聊天默认切到本地，不再依赖 DeepSeek。云端只保留为可选回退。

- 运行时：`mlx-lm 0.32`，经典入口 `mlx_lm.server`（启动时加载权重）提供 OpenAI 兼容的
  `/v1/chat/completions`（SSE 流式）；**请求里的 `model` 必须是本地绝对路径**，
  否则 server 会把它当成 HF repo 去拉；
- 2026-10-28 已删除 7B；当前默认见下方 **Qwen3.5-9B abliterated** 一节。
- 启动脚本默认：
  ```bash
  ./scripts/gabtwin-start.sh
  # 自动启动 mlx_lm server (:8080)，并导出
  # GABTWIN_LLM_BASE=http://127.0.0.1:8080/v1
  # GABTWIN_LLM_KEY=local
  # GABTWIN_LLM_MODEL=<本地模型绝对路径>
  ```
- 回云端：`GABTWIN_LLM=deepseek ./scripts/gabtwin-start.sh`（需要 `.env`
  里的 `DEEPSEEK_API_KEY`，脚本不会起本地 server）。
- 模型缺省时自动回退 `.env` 的 LLM 配置。

### 注意

- 本地 7B 的首 token 和吞吐取决于机器负载；对话只保留 12 条消息 / 4000 字，
  上下文不会撑爆。
- `mlx_lm.server` 是单机实验服务，不是生产级并发；当前一人用足够。
- 首次启动要加载 4.3 GB 权重（约 5–10 秒），之后常驻。
- 模型许可：Qwen2.5 为 Apache-2.0。

---

## 默认切到 Qwen3.5-9B abliterated（2026-10-28）

- 权重：`lukey03/Qwen3.5-9B-abliterated-MLX-4bit`，5.04 GB，sha256 已校验；
- 目录：`models/llm/qwen3.5-9b-abliterated-4bit/`；
- `mlx-lm 0.32.2` 只认 `qwen3_5`，不认仓库里的 `qwen3_5_text`，因此把
  `config.json` 的 `model_type` 改为 `qwen3_5`（原文件备份为 `config.json.orig`）；
- 该模型默认开启思考，会把 40 token 预算吃光；启动 server 时加
  `--chat-template-args '{"enable_thinking": false}'`，并在 system prompt 首行
  保留 Qwen 官方身份句；
- 实测首 token 0.65 s（32 GB M1 Max，5.1 GB 权重），不再有 35B 的换页问题。

### GABTWIN_LLM_MODEL

用**本地绝对路径**，不是 HF 仓库名：

```bash
GABTWIN_LLM_MODEL=/opt/GabTwin/models/llm/qwen3.5-9b-abliterated-4bit
```

启动脚本会把它设为默认并传给 `mlx_lm.server`；手动起 server 时，请求里的
`model` 字段也必须是这个绝对路径。


### 回复长度与截断

- 单轮 LLM 回复上限默认 **512 tokens**（`GABTWIN_LLM_MAX_TOKENS`）；
- 之前是 200，长段落最后 1–2 个字会被截掉；
- 收到 `finish_reason=length` 时日志会写「LLM 回复达到 max_tokens，可能被截断」；
- 校对调用也提高到 512，避免长 ASR 文本被截。


### 对话人设（设置页）

- 界面「设置」打开弹层，编辑角色的系统提示词：背景、和用户的关系、说话风格；
- 保存到 `workspace/persona.txt`，启动时加载；保存后**热更新**当前 `Dialog` 的 system，保留已有对话历史；
- 系统会自动追加口语规则（中文、两三句、不用 Markdown、不念动作等），文本框里只写人设本身；
- 「恢复默认」写回内置默认人设；
- 文件不存在时使用默认值；空人设不允许保存。
