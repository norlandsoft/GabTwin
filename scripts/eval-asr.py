#!/usr/bin/env python3
"""ASR 对照评测：用 macOS 的 say 合成已知文本，跑候选模型，算 CER 与首字延迟。

    python3 scripts/eval-asr.py
    python3 scripts/eval-asr.py --models zipformer-large,zipformer-small-ctc
    python3 scripts/eval-asr.py --json /tmp/asr-eval.json

注意：合成语音是"干净、朗读式"的，CER 会明显好于真实麦克风。它的用途是
**同一条件下比较两个模型**，不是估计产品里的真实字错率。
"""

import argparse
import json
import os
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
        Path.home() / "Library" / "Application Support" / "GabTwin",
    )
)

MODELS = {
    "zipformer-large": HOME / "models" / "streaming-zipformer-zh-int8-2025-06-30",
    "zipformer-small-ctc": HOME / "models" / "streaming-zipformer-small-ctc-zh-int8-2025-04-01",
}

SENTENCES = [
    "今天天气不错，我想出去走走。",
    "你最近在忙什么项目？",
    "我昨天看了一部很有意思的电影。",
    "这家店的面条很好吃，下次带你去。",
    "你能帮我记住明天下午三点开会吗？",
    "最近总是睡不好，有什么办法吗？",
    "周末要不要一起去爬山？",
    "我觉得这个想法还可以再想想。",
    "他说话很快，我有点跟不上。",
    "外面下雨了，记得带伞。",
    "这道题我算了半天也没算出来。",
    "谢谢你今天陪我聊了这么久。",
]

VOICE = os.environ.get("GABTWIN_TTS_VOICE", "Tingting")


def synth(text: str, wav: Path) -> None:
    wav.parent.mkdir(parents=True, exist_ok=True)
    if wav.is_file() and wav.stat().st_size > 0:
        return
    aiff = wav.with_suffix(".aiff")
    subprocess.run(["say", "-v", VOICE, "-o", str(aiff), text], check=True)
    subprocess.run(
        ["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(wav)],
        check=True,
    )
    aiff.unlink(missing_ok=True)


def normalize(text: str) -> str:
    text = unicodedata.normalize("NFKC", text)
    return "".join(ch for ch in text if ch.isalnum()).lower()


def edit_distance(a: str, b: str) -> int:
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def run_model(model_dir: Path, wav: Path, chunk_ms: int) -> dict:
    with tempfile.NamedTemporaryFile("r", suffix=".jsonl", delete=False) as tmp:
        jsonl = Path(tmp.name)
    try:
        proc = subprocess.run(
            [
                str(PROBE),
                "--model-dir",
                str(model_dir),
                "--wav",
                str(wav),
                "--chunk-ms",
                str(chunk_ms),
                "--jsonl",
                str(jsonl),
            ],
            capture_output=True,
            text=True,
        )
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
        return {
            "hyp": hyp,
            "segments": segments,
            "metrics": metrics,
        }
    finally:
        jsonl.unlink(missing_ok=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", default="zipformer-large,zipformer-small-ctc")
    ap.add_argument("--chunk-ms", type=int, default=50)
    ap.add_argument("--json", type=Path, default=None)
    args = ap.parse_args()

    if not PROBE.is_file():
        print(f"找不到 {PROBE}，先 cargo build -p gabtwin-probe", file=sys.stderr)
        return 1

    names = [n.strip() for n in args.models.split(",") if n.strip()]
    for name in names:
        if name not in MODELS:
            print(f"未知模型 {name}（可选：{', '.join(MODELS)}）", file=sys.stderr)
            return 2
        if not MODELS[name].is_dir():
            print(f"模型目录不存在：{MODELS[name]}（先 scripts/fetch-model.sh）", file=sys.stderr)
            return 2

    wav_dir = HOME / "eval" / "tts"
    pairs = []
    for i, text in enumerate(SENTENCES, 1):
        wav = wav_dir / f"{i:02d}.wav"
        synth(text, wav)
        pairs.append((text, wav))

    results = {name: [] for name in names}
    for name in names:
        for text, wav in pairs:
            r = run_model(MODELS[name], wav, args.chunk_ms)
            ref = normalize(text)
            hyp = normalize(r["hyp"])
            r.update(
                {
                    "ref": text,
                    "hyp_raw": r["hyp"],
                    "ref_norm": ref,
                    "hyp_norm": hyp,
                    "edits": edit_distance(ref, hyp),
                    "cer": edit_distance(ref, hyp) / max(1, len(ref)),
                }
            )
            results[name].append(r)

    print("## 结果\n")
    print("| 模型 | CER(微) | CER(宏) | 首字音频位 | RTF | 空结果 |")
    print("|---|---|---|---|---|---|")
    for name in names:
        rows = results[name]
        total_edits = sum(r["edits"] for r in rows)
        total_ref = sum(len(r["ref_norm"]) for r in rows)
        cer_micro = total_edits / max(1, total_ref)
        cer_macro = sum(r["cer"] for r in rows) / len(rows)
        audio_pos = [
            r["metrics"].get("first_partial_audio_ms")
            for r in rows
            if r["metrics"].get("first_partial_audio_ms") is not None
        ]
        rtfs = [r["metrics"].get("rtf", 0.0) for r in rows]
        empty = sum(1 for r in rows if not r["hyp_norm"])
        print(
            f"| {name} | {cer_micro:.1%} | {cer_macro:.1%} | "
            f"{sum(audio_pos) / len(audio_pos):.0f} ms | {sum(rtfs) / len(rtfs):.3f} | {empty} |"
        )

    print("\n## 逐句\n")
    for i, (text, _) in enumerate(pairs, 1):
        print(f"### {i:02d} {text}")
        for name in names:
            r = results[name][i - 1]
            print(f"- {name}: `{r['hyp_raw'] or '（空）'}`  CER {r['cer']:.0%}")

    if args.json:
        args.json.write_text(
            json.dumps(
                {
                    "voice": VOICE,
                    "chunk_ms": args.chunk_ms,
                    "models": names,
                    "results": {
                        name: [
                            {k: v for k, v in r.items() if k != "metrics"} | {"metrics": r["metrics"]}
                            for r in rows
                        ]
                        for name, rows in results.items()
                    },
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
