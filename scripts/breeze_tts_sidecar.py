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
# 新请求到来时递增；正在生成旧请求的线程在下一个 chunk 处退出。
REQUEST_ID = 0


def synth(text: str) -> bytes:
    global REQUEST_ID
    REQUEST_ID += 1
    my_id = REQUEST_ID
    with LOCK:
        chunks = []
        rate = SAMPLE_RATE
        # stream=True 让旧请求能在 chunk 之间被新请求打断，而不是一直占着模型。
        for result in MODEL.generate(
            text=text,
            ref_audio=REF,
            ref_text=REF_TEXT,
            stream=True,
            streaming_interval=0.5,
        ):
            if my_id != REQUEST_ID:
                print("[tts] cancelled by newer request", flush=True)
                break
            chunks.append(np.array(result.audio))
            rate = getattr(result, "sample_rate", rate)
        audio = np.concatenate(chunks) if chunks else np.zeros(0, dtype=np.float32)
    buffer = io.BytesIO()
    sf.write(buffer, audio, rate, format="WAV", subtype="PCM_16")
    return buffer.getvalue()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # 安静
        pass

    def _send(self, code: int, body: bytes, content_type: str):
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _stream_tts(self):
        global REQUEST_ID
        try:
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length) or b"{}")
            text = (payload.get("text") or "").strip()
            if not text:
                self._send(400, b"empty text", "text/plain")
                return
            REQUEST_ID += 1
            my_id = REQUEST_ID
            self.send_response(200)
            self.send_header("Content-Type", "audio/L16; rate=24000")
            self.send_header("X-Sample-Rate", "24000")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            with LOCK:
                for result in MODEL.generate(
                    text=text,
                    ref_audio=REF,
                    ref_text=REF_TEXT,
                    stream=True,
                    streaming_interval=0.25,
                ):
                    if my_id != REQUEST_ID:
                        print("[tts] stream cancelled by newer request", flush=True)
                        break
                    audio = np.array(result.audio, dtype=np.float32)
                    pcm = (np.clip(audio * 2.2, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()
                    if pcm:
                        self.wfile.write(f"{len(pcm):X}\r\n".encode("ascii"))
                        self.wfile.write(pcm)
                        self.wfile.write(b"\r\n")
                        self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:  # noqa: BLE001
            try:
                self._send(500, str(error).encode("utf-8", "replace"), "text/plain")
            except Exception:
                pass

    def do_GET(self):
        if self.path == "/health":
            self._send(200, b"ok", "text/plain")
        else:
            self._send(404, b"not found", "text/plain")

    def do_POST(self):
        if self.path == "/tts_stream":
            self._stream_tts()
            return
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
