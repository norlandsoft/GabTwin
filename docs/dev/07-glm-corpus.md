# dev/07 · 用 GLM 克隆批量生产训练语料

> 目标：先用商业 API（智谱 GLM-TTS-Clone）生成**大量、干净、同一音色**的语音，
> 作为后续本地克隆/微调（GPT-SoVITS、IndexTTS fine-tune 等）的数据集。

## 流程

```text
参考音（3–30s）
  → POST /files（purpose=voice-clone-input）→ file_id
  → POST /voice/clone（model=glm-tts-clone）→ voice_id
  → POST /audio/speech（model=glm-tts）→ WAV
```

- 接口：`https://open.bigmodel.cn/api/paas/v4`
- 认证：`Authorization: Bearer $ZHIPU_API_KEY`
- 参考音：`$GABTWIN_HOME/tts/glm-ref.wav`（当前是 talk.mp3 3.8–11.0s，7.2 秒，降噪）
- 参考文字：`$GABTWIN_HOME/tts/glm-ref.txt`（必须与录音逐字一致）

## 脚本

```bash
# 0) key 放 .env：ZHIPU_API_KEY=...（gitignored，600）

# 1) 克隆（只做一次，voice_id 存到 $GABTWIN_HOME/voices/glm/gabtwin.json）
.venv-mlx/bin/python scripts/glm_tts_corpus.py clone \
  --ref "$HOME/Library/Application Support/GabTwin/tts/glm-ref.wav" \
  --ref-text "收到那俄罗斯人给我的打款了，我就给了他支付宝账号，他支付宝里有钱。" \
  --name gabtwin

# 2) 先试 5 条
.venv-mlx/bin/python scripts/glm_tts_corpus.py gen --limit 5

# 3) 全量（断点续跑，已存在的 wav 会跳过）
.venv-mlx/bin/python scripts/glm_tts_corpus.py gen --workers 2

# 4) 统计
.venv-mlx/bin/python scripts/glm_tts_corpus.py status
```

## 语料

`data/glm_corpus.jsonl`（209 条，可继续加）：

| 类别 | 条数 | 用途 |
|---|---|---|
| daily | 40 | 日常口语句式 |
| emotion:* | 31 | 开心/难过/生气/惊讶/担心/疲惫/平静 |
| numbers | 20 | 金额、日期、电话、百分比 |
| mixed_en | 25 | 中英混读（API/GitHub 等） |
| polyphone | 60 | 多音字专句（银行/行长/重庆/音乐/便/得/着…） |
| style | 18 | 语气/情感/旁白 |
| articulation | 10 | 绕口令、声母韵母覆盖 |

按每条 4–8 秒估算，全量约 **15–25 分钟** 音频，够 GPT-SoVITS 的 few-shot 微调下限；
要更稳可以扩到 400–600 条（加对话、长句、更多数字组合）。

## 数据落盘

```text
$GABTWIN_HOME/dataset/glm/gabtwin/
  wavs/c0001.wav ...
  metadata.jsonl   # id/text/style/wav/duration/sample_rate/voice/model/speed/created_at
```

`metadata.jsonl` 可以直接转成 GPT-SoVITS / RVC / IndexTTS 训练清单（list 文件）。

## 注意

- 参考音质量决定上限：3 秒起，建议 5–10 秒、安静、单人、无混响；
- GLM 会自动匹配情绪，语料里特意放了不同语气的句子；
- API 有并发/配额限制，脚本默认 2 并发、429/5xx 退避重试、断点续跑；
- 商业 API 的使用条款与音色授权自行确认；生成的语料仅用于你自己的模型训练。
