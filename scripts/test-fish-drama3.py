#!/usr/bin/env python3
"""Fish Audio Drama 3 冒烟测试：克隆参考音 → 合成中文 → 保存 WAV。

用法：
  FISH_API_KEY=... .venv-tts/bin/python scripts/test-fish-drama3.py \
    --ref-audio /tmp/ref-fish.wav \
    --ref-text "参考音的文字" \
    --text "要合成的话" --out /tmp/fish-drama3.wav
已有音色模型时用 --voice-id 跳过创建。
"""
import argparse
import os
import sys
import time
from pathlib import Path

import requests

ROOT = Path(__file__).resolve().parent.parent


def api_key() -> str:
    key = os.environ.get("FISH_API_KEY", "").strip()
    if key:
        return key
    env = ROOT / ".env"
    if env.is_file():
        for line in env.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line.startswith("FISH_API_KEY="):
                return line.split("=", 1)[1].strip()
    sys.exit("缺少 FISH_API_KEY（写在 .env 或环境变量里）")


def proxies() -> dict | None:
    proxy = os.environ.get("FISH_PROXY", "").strip()
    return {"http": proxy, "https": proxy} if proxy else None


def create_voice(key: str, audio: Path, text: str) -> str:
    with audio.open("rb") as fh:
        files = {"voices": (audio.name, fh, "audio/wav")}
        data = {
            "title": "gabtwin-test",
            "type": "tts",
            "train_mode": "fast",
            "visibility": "private",
            "texts": text,
        }
        r = requests.post(
            "https://api.fish.audio/model",
            headers={"Authorization": f"Bearer {key}"},
            files=files,
            data=data,
            timeout=180,
            proxies=proxies(),
        )
    if r.status_code >= 300:
        sys.exit(f"创建音色失败 {r.status_code}: {r.text[:300]}")
    body = r.json()
    voice_id = body.get("_id") or body.get("id")
    if not voice_id:
        sys.exit(f"创建音色返回里没有 id: {body}")
    print(f"音色 id: {voice_id}")
    return voice_id


def tts(key: str, text: str, voice_id: str, out: Path, model: str) -> None:
    payload = {
        "text": text,
        "format": "wav",
        "sample_rate": 44100,
        "reference_id": voice_id,
    }
    started = time.time()
    r = requests.post(
        "https://api.fish.audio/v1/tts",
        headers={
            "Authorization": f"Bearer {key}",
            "Content-Type": "application/json",
            "model": model,
        },
        json=payload,
        timeout=180,
        proxies=proxies(),
    )
    elapsed = time.time() - started
    if r.status_code >= 300:
        sys.exit(f"TTS 失败 {r.status_code}: {r.text[:500]}")
    out.write_bytes(r.content)
    print(f"已写入 {out}（{len(r.content)/1024:.0f} KB，{elapsed:.2f} s）")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--text", default="你好，我是你的数字伙伴，今天想聊点什么？")
    ap.add_argument("--ref-audio", type=Path, default=Path("/tmp/ref-fish.wav"))
    ap.add_argument("--ref-text", default="")
    ap.add_argument("--voice-id", default="")
    ap.add_argument("--out", type=Path, default=Path("/tmp/fish-drama3.wav"))
    ap.add_argument("--model", default="drama-3")
    args = ap.parse_args()

    key = api_key()
    voice_id = args.voice_id.strip()
    if not voice_id:
        if not args.ref_text.strip():
            sys.exit("没有 --voice-id 时必须给 --ref-text")
        if not args.ref_audio.is_file():
            sys.exit(f"参考音不存在：{args.ref_audio}")
        voice_id = create_voice(key, args.ref_audio, args.ref_text)
    tts(key, args.text, voice_id, args.out, args.model)


if __name__ == "__main__":
    main()
