#!/usr/bin/env python3
"""把 GLM 语料的 metadata.jsonl 转成训练清单（GPT-SoVITS / RVC 等）。"""
import argparse
import csv
import json
import os
from collections import Counter
from pathlib import Path

HOME = Path(os.environ.get("GABTWIN_HOME", Path(__file__).resolve().parents[1] / "workspace"))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", type=Path, default=HOME / "dataset" / "glm" / "gabtwin")
    ap.add_argument("--speaker", default="gabtwin")
    ap.add_argument("--lang", default="zh")
    args = ap.parse_args()

    meta = args.dataset / "metadata.jsonl"
    rows = [json.loads(line) for line in meta.read_text(encoding="utf-8").splitlines() if line.strip()]
    rows.sort(key=lambda r: r["id"])

    # GPT-SoVITS: wav_path|speaker|language|text
    with (args.dataset / "list.txt").open("w", encoding="utf-8") as fh:
        for row in rows:
            wav = (args.dataset / row["wav"]).resolve()
            fh.write(f"{wav}|{args.speaker}|{args.lang}|{row['text']}\n")

    with (args.dataset / "metadata.csv").open("w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(["id", "wav", "duration", "style", "text"])
        for row in rows:
            writer.writerow([row["id"], row["wav"], row["duration"], row["style"], row["text"]])

    total = sum(r["duration"] for r in rows)
    rates = Counter(r["sample_rate"] for r in rows)
    styles = Counter(r["style"] for r in rows)
    summary = [
        f"# GLM 语料 {args.speaker}",
        "",
        f"- 条数：{len(rows)}",
        f"- 总时长：{total/60:.1f} 分钟",
        f"- 采样率：{dict(rates)}",
        f"- 音色 voice：{rows[0]['voice'] if rows else '-'}",
        "",
        "## 类别",
        "",
    ]
    summary += [f"- {k}: {v}" for k, v in styles.most_common()]
    summary += [
        "",
        "## 训练清单",
        "",
        "- `list.txt`：GPT-SoVITS 格式，`绝对路径|说话人|语言|文字`",
        "- `metadata.csv`：id / wav / duration / style / text",
        "",
        "> 这类合成语料适合做 few-shot 微调；正式训练前建议抽听 20–30 条，",
        "> 剔除吞字、爆音、情绪不符的样本。",
    ]
    (args.dataset / "README.md").write_text("\n".join(summary) + "\n", encoding="utf-8")
    print(f"{len(rows)} 条，{total/60:.1f} 分钟，{args.dataset}")


if __name__ == "__main__":
    main()
