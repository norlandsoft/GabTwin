#!/usr/bin/env python3
"""IndexTTS-2.5 MLX sidecar for GabTwin.

GET  /health -> ok
POST /tts   {"text": "...", "lang": "zh"} -> WAV (22.05 kHz, PCM16)
"""
import argparse
import io
import json
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import soundfile as sf

from index_tts_2_5_mlx import IndexTTS

lock = threading.Lock()
tts = None
spk = None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path != "/tts":
            self.send_response(404)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(length) or b"{}")
        text = (req.get("text") or "").strip()
        lang = req.get("lang") or "zh"
        if not text:
            self.send_response(400)
            self.end_headers()
            return
        started = time.time()
        try:
            with lock:
                pcm = tts.synthesize(text, lang=lang, spk=spk)
            buf = io.BytesIO()
            sf.write(buf, pcm.astype("int16"), tts.sample_rate, format="WAV", subtype="PCM_16")
            data = buf.getvalue()
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            print(f"[tts] {len(text)} chars, {len(data)/1024:.0f} KB, {time.time()-started:.2f}s: {text[:40]}", flush=True)
        except Exception as exc:  # noqa: BLE001
            print(f"[tts] error: {exc}", flush=True)
            self.send_response(500)
            self.end_headers()
            self.wfile.write(str(exc).encode())


def main():
    global tts, spk
    ap = argparse.ArgumentParser()
    ap.add_argument("--model-dir", required=True)
    ap.add_argument("--ref", required=True)
    ap.add_argument("--port", type=int, default=8798)
    ap.add_argument("--lang", default="zh")
    args = ap.parse_args()

    print("[tts] loading model...", flush=True)
    tts = IndexTTS(model_dir=args.model_dir)
    print("[tts] building speaker...", flush=True)
    spk = tts.build_speaker(args.ref)
    print(f"[tts] ready on :{args.port} ({tts.sample_rate} Hz)", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
