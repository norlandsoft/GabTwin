#!/usr/bin/env python3
"""Breeze TTS 2（MLX）本地 sidecar：加载一次模型，HTTP 返回 24 kHz WAV。

POST /tts  {"text": "...", "lang": "zh"}  -> audio/wav
GET  /health                              -> ok
"""
import argparse
import io
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import soundfile as sf
from mlx_audio.tts import load_model

MODEL = None
REF = None
REF_TEXT = ""
SAMPLE_RATE = 24000
LOCK = threading.Lock()


def synth(text: str) -> bytes:
    with LOCK:
        chunks = []
        rate = SAMPLE_RATE
        for result in MODEL.generate(text=text, ref_audio=REF, ref_text=REF_TEXT):
            chunks.append(np.array(result.audio))
            rate = getattr(result, "sample_rate", rate)
        audio = np.concatenate(chunks) if chunks else np.zeros(0, dtype=np.float32)
    buffer = io.BytesIO()
    sf.write(buffer, audio, rate, format="WAV", subtype="PCM_16")
    return buffer.getvalue()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # 安静
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
            wav = synth(text)
            self._send(200, wav, "audio/wav")
        except Exception as error:  # noqa: BLE001
            self._send(500, str(error).encode("utf-8", "replace"), "text/plain")


def main():
    global MODEL, REF, REF_TEXT, SAMPLE_RATE
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--ref-text", required=True)
    parser.add_argument("--port", type=int, default=8799)
    parser.add_argument("--warmup", default="你好。")
    args = parser.parse_args()

    REF = args.ref
    REF_TEXT = args.ref_text
    print(f"[tts] loading {args.model}", flush=True)
    MODEL = load_model(args.model)
    print("[tts] warming up", flush=True)
    synth(args.warmup)
    print(f"[tts] ready on :{args.port} (24 kHz)", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
