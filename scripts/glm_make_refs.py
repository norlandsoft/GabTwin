#!/usr/bin/env python3
"""用 GLM 克隆音色批量生成 15 秒参考语音。

输入：data/gyy_ref_texts.jsonl（id/text）
输出：workspace/dataset/glm/<name>/refs/<id>.wav + metadata.jsonl
"""
import argparse
import json
import os
import subprocess
import time
from pathlib import Path

import requests

ROOT = Path(__file__).resolve().parents[1]
URL = "https://open.bigmodel.cn/api/paas/v4/audio/speech"


def load_key() -> str:
    key = os.environ.get("ZHIPU_API_KEY")
    if key:
        return key
    for line in (ROOT / ".env").read_text().splitlines():
        if line.startswith("ZHIPU_API_KEY="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("缺少 ZHIPU_API_KEY")


def duration(path: Path) -> float:
    out = subprocess.check_output(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration",
         "-of", "default=nw=1:nk=1", str(path)],
        text=True,
    )
    return float(out.strip())


def synth(text: str, voice: str, key: str, out: Path) -> None:
    for attempt in range(5):
        response = requests.post(
            URL,
            headers={"Authorization": f"Bearer {key}"},
            json={
                "model": "glm-tts",
                "input": text,
                "voice": voice,
                "response_format": "wav",
                "speed": 1.0,
            },
            timeout=180,
        )
        if response.status_code == 200 and response.content:
            out.write_bytes(response.content)
            return
        print(f"  retry {attempt + 1}: HTTP {response.status_code} {response.text[:120]}")
        time.sleep(1.5 * (attempt + 1))
    raise RuntimeError("GLM TTS 多次失败")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--name", default="gyy")
    parser.add_argument("--texts", type=Path, default=ROOT / "data/gyy_ref_texts.jsonl")
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--target", type=float, default=15.0)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    key = load_key()
    voice_data = json.loads(
        (ROOT / "workspace" / "voices" / "glm" / f"{args.name}.json").read_text()
    )
    voice = voice_data["voice"]
    out_dir = args.out or (ROOT / "workspace" / "dataset" / "glm" / args.name / "refs")
    out_dir.mkdir(parents=True, exist_ok=True)
    raw_dir = Path("/tmp") / f"glm-refs-{args.name}"
    raw_dir.mkdir(parents=True, exist_ok=True)

    rows = [json.loads(line) for line in args.texts.read_text().splitlines() if line.strip()]
    if args.limit:
        rows = rows[: args.limit]
    meta_path = out_dir / "metadata.jsonl"
    done = 0
    for row in rows:
        out = out_dir / f"{row['id']}.wav"
        if out.exists() and not args.force:
            print(f"skip {row['id']}")
            continue
        raw = raw_dir / f"{row['id']}.wav"
        print(f"[{done + 1}/{len(rows)}] {row['id']} {row['chars']} chars")
        synth(row["text"], voice, key, raw)
        source = duration(raw)
        tempo = min(2.0, max(0.5, source / args.target))
        subprocess.run(
            [
                "ffmpeg", "-y", "-v", "error", "-i", str(raw),
                "-af", f"atempo={tempo:.4f},loudnorm=I=-18:TP=-2,apad",
                "-t", f"{args.target:.3f}", "-ar", "24000", "-ac", "1",
                "-c:a", "pcm_s16le", str(out),
            ],
            check=True,
        )
        final = duration(out)
        with meta_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps({
                "id": row["id"],
                "text": row["text"],
                "wav": str(out),
                "seconds": round(final, 3),
                "source_seconds": round(source, 3),
                "tempo": round(tempo, 3),
                "voice": voice,
                "model": "glm-tts",
            }, ensure_ascii=False) + "\n")
        print(f"  {source:.2f}s -> {final:.2f}s (tempo {tempo:.2f})")
        done += 1
    print(f"完成：{done} 条，目录 {out_dir}")


if __name__ == "__main__":
    main()
