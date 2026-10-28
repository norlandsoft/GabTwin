#!/usr/bin/env python3
"""ASR 对照评测：合成已知文本，跑候选模型，按类别算错误率与首字延迟。

    python3 scripts/eval-asr.py
    python3 scripts/eval-asr.py --models zipformer-zh,bilingual
    python3 scripts/eval-asr.py --categories en,mixed
    python3 scripts/eval-asr.py --beam          # 用 modified_beam_search
    python3 scripts/eval-asr.py --json /tmp/asr-eval.json

类别：
  zh     中文（Tingting 朗读）
  en     英文（Samantha 朗读）
  mixed  中英混说（Tingting，英文词按中文口音读）

错误率按 token 算：中文一个字一个 token，英文一个词一个 token。
合成语音是干净朗读，错误率会明显好于真实麦克风；用途是**同条件横向比较模型**。
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROBE = ROOT / "target" / "debug" / "gabtwin-probe"
HOME = Path(
    os.environ.get(
        "GABTWIN_HOME",
        ROOT / "workspace",
    )
)

MODELS = {
    "zipformer-zh": ROOT / "models" / "asr" / "streaming-zipformer-zh-int8-2025-06-30",
    "bilingual": ROOT / "models" / "asr" / "streaming-zipformer-bilingual-zh-en-2023-02-20",
    "multilingual": HOME
    / "models"
    / "streaming-zipformer-ar_en_id_ja_ru_th_vi_zh-2025-02-10",
    "small-ctc": ROOT / "models" / "asr" / "streaming-zipformer-small-ctc-zh-int8-2025-04-01",
    "paraformer": ROOT / "models" / "asr" / "streaming-paraformer-bilingual-zh-en",
}

TEST_SET = [
    # 中文
    ("zh", "Tingting", "今天天气不错，我想出去走走。"),
    ("zh", "Tingting", "你最近在忙什么项目？"),
    ("zh", "Tingting", "我昨天看了一部很有意思的电影。"),
    ("zh", "Tingting", "这家店的面条很好吃，下次带你去。"),
    ("zh", "Tingting", "你能帮我记住明天下午三点开会吗？"),
    ("zh", "Tingting", "最近总是睡不好，有什么办法吗？"),
    ("zh", "Tingting", "周末要不要一起去爬山？"),
    ("zh", "Tingting", "我觉得这个想法还可以再想想。"),
    ("zh", "Tingting", "他说话很快，我有点跟不上。"),
    ("zh", "Tingting", "外面下雨了，记得带伞。"),
    ("zh", "Tingting", "这道题我算了半天也没算出来。"),
    ("zh", "Tingting", "谢谢你今天陪我聊了这么久。"),
    # 英文
    ("en", "Samantha", "Can you hear me now?"),
    ("en", "Samantha", "Let's schedule a meeting tomorrow at three."),
    ("en", "Samantha", "I think this product is really interesting."),
    ("en", "Samantha", "Please open the settings panel."),
    ("en", "Samantha", "The weather is nice today."),
    ("en", "Samantha", "I would like a cup of coffee."),
    ("en", "Samantha", "What time is it now?"),
    ("en", "Samantha", "Turn on the lights, please."),
    # 中英混说
    ("mixed", "Tingting", "帮我查一下 deadline 是哪天。"),
    ("mixed", "Tingting", "这个 bug 我明天再修。"),
    ("mixed", "Tingting", "我们用 GitHub 管理代码。"),
    ("mixed", "Tingting", "我需要一个 USB-C 转接头。"),
    ("mixed", "Tingting", "这个 API 的文档在哪里？"),
    ("mixed", "Tingting", "把这段话转成 JSON。"),
]

TOKEN_RE = re.compile(r"[a-z0-9]+|[\u3400-\u4dbf\u4e00-\u9fff]")

# 合成音频开头补 500 ms 静音：流式模型需要一点"热身"，
# 否则第一个词会被吃掉（真实麦克风前通常本来就有环境静音）。
PAD_MS = 500


def prepend_silence(wav: Path, ms: int) -> None:
    import wave

    with wave.open(str(wav), "rb") as src:
        params = src.getparams()
        frames = src.readframes(src.getnframes())
    silence = b"\x00\x00" * int(params.framerate * ms / 1000) * params.nchannels
    with wave.open(str(wav), "wb") as dst:
        dst.setparams(params)
        dst.writeframes(silence + frames)


def synth(text: str, voice: str, wav: Path) -> None:
    wav.parent.mkdir(parents=True, exist_ok=True)
    if wav.is_file() and wav.stat().st_size > 0:
        return
    aiff = wav.with_suffix(".aiff")
    subprocess.run(["say", "-v", voice, "-o", str(aiff), text], check=True)
    subprocess.run(
        ["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(wav)],
        check=True,
    )
    aiff.unlink(missing_ok=True)
    prepend_silence(wav, PAD_MS)


def tokenize(text: str) -> list[str]:
    text = unicodedata.normalize("NFKC", text).lower()
    # 撇号不参与比较：let's / lets 视为同一个词
    text = text.replace("'", "").replace("’", "")
    return TOKEN_RE.findall(text)


def edit_distance(a: list[str], b: list[str]) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def run_model(model_dir: Path, wav: Path, chunk_ms: int, beam: bool, warmup_ms: int) -> dict:
    with tempfile.NamedTemporaryFile("r", suffix=".jsonl", delete=False) as tmp:
        jsonl = Path(tmp.name)
    try:
        cmd = [
            str(PROBE),
            "--model-dir",
            str(model_dir),
            "--wav",
            str(wav),
            "--chunk-ms",
            str(chunk_ms),
            "--warmup-ms",
            str(warmup_ms),
            "--jsonl",
            str(jsonl),
        ]
        if beam:
            cmd.append("--beam")
        proc = subprocess.run(cmd, capture_output=True, text=True)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())

        segments, partials, metrics = [], [], {}
        for line in jsonl.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            ev = json.loads(line)["ev"]
            if ev["kind"] == "segment":
                segments.append(ev["text"])
            elif ev["kind"] == "partial":
                partials.append(ev["text"])
            elif ev["kind"] == "metrics":
                metrics = ev
        hyp = "".join(segments) if segments else (partials[-1] if partials else "")
        return {"hyp": hyp, "metrics": metrics}
    finally:
        jsonl.unlink(missing_ok=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", default="zipformer-zh,bilingual,multilingual")
    ap.add_argument("--categories", default="zh,en,mixed")
    ap.add_argument("--chunk-ms", type=int, default=50)
    ap.add_argument("--beam", action="store_true")
    ap.add_argument("--warmup-ms", type=int, default=150)
    ap.add_argument("--json", type=Path, default=None)
    args = ap.parse_args()

    if not PROBE.is_file():
        print(f"找不到 {PROBE}，先 cargo build -p gabtwin-probe", file=sys.stderr)
        return 1

    names = [n.strip() for n in args.models.split(",") if n.strip()]
    cats = {c.strip() for c in args.categories.split(",") if c.strip()}
    for name in names:
        if name not in MODELS:
            print(f"未知模型 {name}（可选：{', '.join(MODELS)}）", file=sys.stderr)
            return 2
        if not MODELS[name].is_dir():
            print(f"模型目录不存在：{MODELS[name]}（先 scripts/fetch-model.sh）", file=sys.stderr)
            return 2

    wav_dir = HOME / "eval" / "tts"
    cases = []
    for i, (cat, voice, text) in enumerate(TEST_SET):
        if cat not in cats:
            continue
        digest = hashlib.sha1(text.encode()).hexdigest()[:8]
        wav = wav_dir / f"{cat}-{i:02d}-pad{PAD_MS}-{digest}.wav"
        synth(text, voice, wav)
        cases.append({"cat": cat, "voice": voice, "text": text, "wav": wav})

    results = {name: [] for name in names}
    for name in names:
        print(f"[{name}] {len(cases)} 句…", file=sys.stderr)
        for idx, case in enumerate(cases, 1):
            r = run_model(MODELS[name], case["wav"], args.chunk_ms, args.beam, args.warmup_ms)
            ref = tokenize(case["text"])
            hyp = tokenize(r["hyp"])
            r.update(
                {
                    "cat": case["cat"],
                    "ref": case["text"],
                    "hyp_raw": r["hyp"],
                    "edits": edit_distance(ref, hyp),
                    "ref_len": max(1, len(ref)),
                }
            )
            r["rate"] = r["edits"] / r["ref_len"]
            results[name].append(r)
            print(f"  {idx}/{len(cases)} {case['cat']:<5} {r['rate']:.0%}", file=sys.stderr)

    cat_names = {"zh": "中文", "en": "英文", "mixed": "中英混"}
    print("## 结果" + ("（beam search）" if args.beam else "") + "\n")
    print(
        "| 模型 | "
        + " | ".join(cat_names.get(c, c) for c in sorted(cats))
        + " | 首字音频位 | RTF |"
    )
    print("|---" * (len(cats) + 3) + "|")
    for name in names:
        rows = results[name]
        cells = []
        for cat in sorted(cats):
            sub = [r for r in rows if r["cat"] == cat]
            if not sub:
                cells.append("—")
                continue
            edits = sum(r["edits"] for r in sub)
            total = sum(r["ref_len"] for r in sub)
            cells.append(f"{edits / max(1, total):.1%}")
        pos = [
            r["metrics"].get("first_partial_audio_ms")
            for r in rows
            if r["metrics"].get("first_partial_audio_ms") is not None
        ]
        rtfs = [r["metrics"].get("rtf", 0.0) for r in rows]
        print(
            f"| {name} | "
            + " | ".join(cells)
            + f" | {sum(pos) / max(1, len(pos)):.0f} ms | {sum(rtfs) / max(1, len(rtfs)):.3f} |"
        )

    print("\n## 逐句\n")
    for cat in sorted(cats):
        for ci, case in enumerate(cases):
            if case["cat"] != cat:
                continue
            print(f"### [{cat}] {case['text']}")
            for name in names:
                r = results[name][ci]
                print(f"- {name}: `{r['hyp_raw'] or '（空）'}`  {r['rate']:.0%}")

    if args.json:
        args.json.write_text(
            json.dumps(
                {
                    "models": names,
                    "beam": args.beam,
                    "chunk_ms": args.chunk_ms,
                    "results": {n: rows for n, rows in results.items()},
                },
                ensure_ascii=False,
                indent=2,
            ),
            encoding="utf-8",
        )
        print(f"\n明细已写入 {args.json}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
