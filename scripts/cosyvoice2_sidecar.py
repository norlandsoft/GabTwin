#!/usr/bin/env python3
"""CosyVoice2-0.5B（MLX）sidecar：一次请求生成整段，返回 24 kHz WAV。

依赖 `mlx-audio-plus`（提供 `mlx_audio.tts.models.cosyvoice2`）。
模型目录：models/tts/cosyvoice2-0.5b-8bit

POST /tts {"text": "...", "lang": "zh"} -> audio/wav
GET  /health -> ok
"""
from __future__ import annotations

import argparse
import io
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import soundfile as sf
from mlx_audio.tts.utils import load_model

MODEL = None
REF = None
REF_TEXT = ""
SAMPLE_RATE = 24000
SPEED = 1.0
INSTRUCT = ""
LOCK = threading.Lock()


def synth(text: str) -> bytes:
    with LOCK:
        started = time.time()
        result = None
        for item in MODEL.generate(
            text=text,
            ref_audio=REF,
            ref_text=REF_TEXT,
            instruct_text=INSTRUCT or None,
            speed=SPEED,
            stream=False,
            verbose=False,
        ):
            result = item
        if result is None:
            raise RuntimeError("CosyVoice2 没有返回音频")
        audio = np.asarray(result.audio, dtype=np.float32).reshape(-1)
        rate = int(getattr(result, "sample_rate", SAMPLE_RATE))
        elapsed = time.time() - started
        print(
            f"[tts] {len(text)} chars -> {len(audio)/rate:.2f}s audio / {elapsed:.2f}s "
            f"(RTF {elapsed/max(len(audio)/rate, 1e-6):.2f})",
            flush=True,
        )
        buffer = io.BytesIO()
        sf.write(buffer, np.clip(audio, -1.0, 1.0), rate, format="WAV", subtype="PCM_16")
        return buffer.getvalue()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _send(self, code: int, body: bytes, content_type: str):
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._send(200, b"ok", "text/plain")
        else:
            self._send(404, b"not found", "text/plain")

    def do_POST(self):
        if self.path != "/tts":
            self._send(404, b"not found", "text/plain")
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length) or b"{}")
            text = (payload.get("text") or "").strip()
            if not text:
                self._send(400, b"empty text", "text/plain")
                return
            self._send(200, synth(text), "audio/wav")
        except Exception as error:  # noqa: BLE001
            self._send(500, str(error).encode("utf-8", "replace"), "text/plain")


def main():
    global MODEL, REF, REF_TEXT, SAMPLE_RATE, SPEED, INSTRUCT
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--ref-text", required=True)
    parser.add_argument("--instruct", default="")
    parser.add_argument("--speed", type=float, default=1.0)
    parser.add_argument("--port", type=int, default=8797)
    parser.add_argument("--warmup", default="你好。")
    args = parser.parse_args()

    REF_TEXT = args.ref_text.strip()
    INSTRUCT = args.instruct.strip()
    SPEED = args.speed
    ref, sr = sf.read(args.ref, dtype="float32")
    if ref.ndim > 1:
        ref = ref.mean(axis=1)
    if sr != 24000:
        import librosa

        ref = librosa.resample(ref, orig_sr=sr, target_sr=24000)
    REF = ref.astype(np.float32)

    print(f"[tts] loading {args.model}", flush=True)
    MODEL = load_model(args.model)
    SAMPLE_RATE = int(getattr(MODEL, "sample_rate", 24000))
    print(f"[tts] warming up ({SAMPLE_RATE} Hz)", flush=True)
    synth(args.warmup)
    print(f"[tts] ready on :{args.port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
