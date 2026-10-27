#!/usr/bin/env python3
"""GLM 音色克隆 + 批量语料生成。

  # 1) 上传参考音并克隆音色（保存 voice id）
  ZHIPU_API_KEY=... .venv-mlx/bin/python scripts/glm_tts_corpus.py clone \
      --ref /tmp/ref-pause.wav --ref-text "参考音逐字文字" --name gabtwin

  # 2) 批量合成语料（可断点续跑）
  .venv-mlx/bin/python scripts/glm_tts_corpus.py gen --limit 5      # 先试 5 条
  .venv-mlx/bin/python scripts/glm_tts_corpus.py gen                # 全量

  # 3) 统计
  .venv-mlx/bin/python scripts/glm_tts_corpus.py status
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime
from pathlib import Path

import requests
import soundfile as sf

ROOT = Path(__file__).resolve().parent.parent
BASE = "https://open.bigmodel.cn/api/paas/v4"
HOME = Path(os.environ.get("GABTWIN_HOME", Path.home() / "Library/Application Support/GabTwin"))
VOICES = HOME / "voices" / "glm"
DATASET = HOME / "dataset" / "glm"
CORPUS = ROOT / "data" / "glm_corpus.jsonl"


def api_key() -> str:
    for name in ("ZHIPU_API_KEY", "GLM_API_KEY", "BIGMODEL_API_KEY"):
        value = os.environ.get(name, "").strip()
        if value:
            return value
    env = ROOT / ".env"
    if env.is_file():
        for line in env.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            for name in ("ZHIPU_API_KEY", "GLM_API_KEY", "BIGMODEL_API_KEY"):
                if line.startswith(name + "="):
                    value = line.split("=", 1)[1].strip()
                    if value:
                        return value
    sys.exit("缺少 ZHIPU_API_KEY：写到 .env 或环境变量（bigmodel.cn 的 API Key）")


def headers(key: str) -> dict:
    return {"Authorization": f"Bearer {key}"}


def request(method: str, path: str, key: str, **kwargs):
    url = BASE + path
    last = None
    for attempt in range(4):
        resp = requests.request(method, url, headers=headers(key), timeout=180, **kwargs)
        if resp.status_code == 429 or resp.status_code >= 500:
            last = resp
            time.sleep(2 * (attempt + 1))
            continue
        if resp.status_code >= 400:
            sys.exit(f"{path} 失败 {resp.status_code}: {resp.text[:500]}")
        return resp
    sys.exit(f"{path} 重试后仍失败: {last.status_code} {last.text[:300]}")


def upload(key: str, ref: Path) -> str:
    with ref.open("rb") as fh:
        resp = request(
            "POST",
            "/files",
            key,
            files={"file": (ref.name, fh, "audio/wav")},
            data={"purpose": "voice-clone-input"},
        )
    body = resp.json()
    file_id = body.get("id")
    if not file_id:
        sys.exit(f"上传返回里没有 id: {body}")
    print(f"file_id: {file_id}")
    return file_id


def clone(key: str, file_id: str, ref_text: str, name: str, preview: str) -> str:
    payload = {
        "model": "glm-tts-clone",
        "voice_name": name,
        "text": ref_text,
        "input": preview,
        "file_id": file_id,
    }
    resp = request("POST", "/voice/clone", key, json=payload)
    body = resp.json()
    voice = body.get("voice")
    if not voice:
        sys.exit(f"克隆返回里没有 voice: {body}")
    VOICES.mkdir(parents=True, exist_ok=True)
    meta = {
        "name": name,
        "voice": voice,
        "file_id": file_id,
        "ref_file_id": file_id,
        "ref_text": ref_text,
        "preview_file_id": body.get("file_id"),
        "created_at": datetime.now().isoformat(timespec="seconds"),
    }
    path = VOICES / f"{name}.json"
    path.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"voice: {voice}\n已保存 {path}")
    return voice


def speech(key: str, text: str, voice: str, out: Path, speed: float) -> float:
    payload = {
        "model": "glm-tts",
        "input": text,
        "voice": voice,
        "response_format": "wav",
        "speed": speed,
    }
    resp = request("POST", "/audio/speech", key, json=payload)
    content_type = resp.headers.get("Content-Type", "")
    if "application/json" in content_type:
        sys.exit(f"/audio/speech 返回 JSON: {resp.text[:400]}")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(resp.content)
    info = sf.info(str(out))
    if info.duration < 0.3:
        out.unlink(missing_ok=True)
        raise RuntimeError(f"音频过短 {info.duration:.2f}s")
    return info.duration


def load_corpus(limit: int | None, style: str | None) -> list[dict]:
    rows = [json.loads(line) for line in CORPUS.read_text(encoding="utf-8").splitlines() if line.strip()]
    if style:
        rows = [r for r in rows if r["style"] == style]
    if limit:
        rows = rows[:limit]
    return rows


def gen(args) -> None:
    key = api_key()
    meta_path = VOICES / f"{args.name}.json"
    if not meta_path.is_file():
        sys.exit(f"先 clone：{meta_path} 不存在")
    voice = json.loads(meta_path.read_text(encoding="utf-8"))["voice"]
    out_dir = Path(args.out or (DATASET / args.name))
    wav_dir = out_dir / "wavs"
    wav_dir.mkdir(parents=True, exist_ok=True)
    metadata = out_dir / "metadata.jsonl"
    done = set()
    if metadata.is_file():
        for line in metadata.read_text(encoding="utf-8").splitlines():
            try:
                done.add(json.loads(line)["id"])
            except Exception:
                pass
    rows = load_corpus(args.limit, args.style)
    todo = [r for r in rows if r["id"] not in done or not (wav_dir / f"{r['id']}.wav").is_file()]
    print(f"语料 {len(rows)} 条，已完成 {len(rows)-len(todo)}，本次 {len(todo)} 条 → {out_dir}")

    def work(row):
        wav = wav_dir / f"{row['id']}.wav"
        if wav.is_file():
            return row, sf.info(str(wav)).duration
        duration = speech(key, row["text"], voice, wav, args.speed)
        return row, duration

    started = time.time()
    total_sec = 0.0
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {pool.submit(work, row): row for row in todo}
        for i, future in enumerate(as_completed(futures), 1):
            row, duration = future.result()
            total_sec += duration
            record = {
                "id": row["id"],
                "text": row["text"],
                "style": row["style"],
                "wav": str((wav_dir / f"{row['id']}.wav").relative_to(out_dir)),
                "duration": round(duration, 3),
                "sample_rate": sf.info(str(wav_dir / f"{row['id']}.wav")).samplerate,
                "voice": voice,
                "model": "glm-tts",
                "speed": args.speed,
                "created_at": datetime.now().isoformat(timespec="seconds"),
            }
            with metadata.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(record, ensure_ascii=False) + "\n")
            print(f"[{i}/{len(todo)}] {row['id']} {duration:5.2f}s  {row['text'][:24]}")

    elapsed = time.time() - started
    print(f"完成：{len(todo)} 条，新增音频 {total_sec/60:.1f} 分钟，用时 {elapsed/60:.1f} 分钟")


def status(args) -> None:
    out_dir = Path(args.out or (DATASET / args.name))
    metadata = out_dir / "metadata.jsonl"
    if not metadata.is_file():
        print(f"没有数据：{metadata}")
        return
    rows = [json.loads(line) for line in metadata.read_text(encoding="utf-8").splitlines() if line.strip()]
    total = sum(r.get("duration", 0) for r in rows)
    print(f"{len(rows)} 条，{total/60:.1f} 分钟，目录 {out_dir}")
    from collections import Counter
    print(Counter(r.get("style", "?") for r in rows))


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("clone")
    c.add_argument("--ref", type=Path, required=True)
    c.add_argument("--ref-text", required=True)
    c.add_argument("--name", default="gabtwin")
    c.add_argument("--preview", default="你好，我是你的数字伙伴，今天想聊点什么？")

    g = sub.add_parser("gen")
    g.add_argument("--name", default="gabtwin")
    g.add_argument("--corpus", type=Path, default=CORPUS)
    g.add_argument("--out", type=Path)
    g.add_argument("--limit", type=int)
    g.add_argument("--style")
    g.add_argument("--speed", type=float, default=1.0)
    g.add_argument("--workers", type=int, default=2)

    s = sub.add_parser("status")
    s.add_argument("--name", default="gabtwin")
    s.add_argument("--out", type=Path)

    args = ap.parse_args()
    if args.cmd == "clone":
        key = api_key()
        if not args.ref.is_file():
            sys.exit(f"参考音不存在：{args.ref}")
        file_id = upload(key, args.ref)
        clone(key, file_id, args.ref_text, args.name, args.preview)
    elif args.cmd == "gen":
        gen(args)
    else:
        status(args)


if __name__ == "__main__":
    main()
