#!/usr/bin/env python3
"""Qwen3-TTS 1.7B Base（MLX）sidecar：/tts 整句 WAV + /tts_stream 流式 PCM。

克隆用 --ref 与 --ref-text；模型为本地目录（Base 模型支持 ref_audio/ref_text）。
"""
import argparse
import io
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import soundfile as sf
from mlx_audio.tts.utils import load_model

MODEL = None
REF = None
REF_TEXT = ""
SAMPLE_RATE = 24000
SPEED = 0.85
PAUSE = True
PAUSE_MIN_MS = 300
PAUSE_EXTRA_MS = 300
LOCK = threading.Lock()
SESSION_REF_PATH = "/tmp/gabtwin-session-ref.wav"
SESSION_REF = None
SESSION_TEXT = ""
SESSION_AUDIO: list[np.ndarray] = []
SESSION_TEXTS: list[str] = []
REQUEST_ID = 0



def current_ref():
    """同一对话内，后续段落沿用第一段生成音频做音色参考。"""
    if SESSION_REF is not None:
        return SESSION_REF, SESSION_TEXT
    return REF, REF_TEXT


def remember_session(audio: np.ndarray, rate: int, text: str) -> None:
    global SESSION_REF, SESSION_TEXT
    if SESSION_REF is not None:
        return
    SESSION_AUDIO.append(np.asarray(audio, dtype=np.float32))
    SESSION_TEXTS.append(text)
    total = np.concatenate(SESSION_AUDIO)
    if len(total) >= 3.0 * rate:
        sf.write(SESSION_REF_PATH, total, rate, subtype="PCM_16")
        SESSION_REF = SESSION_REF_PATH
        SESSION_TEXT = "".join(SESSION_TEXTS)
        print(f"[tts] session ref locked: {len(total) / rate:.2f}s", flush=True)


def reset_session() -> None:
    global SESSION_REF, SESSION_TEXT, SESSION_AUDIO, SESSION_TEXTS
    SESSION_REF = None
    SESSION_TEXT = ""
    SESSION_AUDIO = []
    SESSION_TEXTS = []


def pcm_bytes(audio: np.ndarray) -> bytes:
    return (np.clip(audio, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()


class PauseExtender:
    """只在真正的标点停顿（>=300ms 静音）后追加额外静音。"""

    def __init__(self, sample_rate: int, extra_ms: int = 250, min_pause_ms: int = 300):
        self.frame = max(1, int(sample_rate * 0.02))
        self.min_samples = int(sample_rate * min_pause_ms / 1000)
        self.extra = np.zeros(int(sample_rate * extra_ms / 1000), dtype=np.float32)
        self.buf = np.zeros(0, dtype=np.float32)
        self.silence: list[np.ndarray] = []
        self.started = False

    def push(self, audio: np.ndarray) -> np.ndarray:
        self.buf = np.concatenate([self.buf, np.asarray(audio, dtype=np.float32)])
        out: list[np.ndarray] = []
        while len(self.buf) >= self.frame:
            frame = self.buf[: self.frame]
            self.buf = self.buf[self.frame :]
            rms = float(np.sqrt(np.mean(frame * frame)))
            if rms < 0.006:
                if self.started:
                    self.silence.append(frame)
                else:
                    out.append(frame)
            else:
                if self.silence:
                    run = np.concatenate(self.silence)
                    self.silence = []
                    out.append(run)
                    if len(run) >= self.min_samples:
                        out.append(self.extra)
                self.started = True
                out.append(frame)
        return np.concatenate(out) if out else np.zeros(0, dtype=np.float32)

    def flush(self) -> np.ndarray:
        out: list[np.ndarray] = []
        if self.silence:
            out.append(np.concatenate(self.silence))
            self.silence = []
        if len(self.buf):
            out.append(self.buf)
            self.buf = np.zeros(0, dtype=np.float32)
        return np.concatenate(out) if out else np.zeros(0, dtype=np.float32)


def extend_pauses(audio: np.ndarray, sample_rate: int) -> np.ndarray:
    if not PAUSE:
        return audio
    extender = PauseExtender(sample_rate, PAUSE_EXTRA_MS, PAUSE_MIN_MS)
    parts = [extender.push(audio), extender.flush()]
    parts = [part for part in parts if len(part)]
    return np.concatenate(parts) if parts else np.zeros(0, dtype=np.float32)


def generate(text: str):
    global REQUEST_ID
    REQUEST_ID += 1
    my_id = REQUEST_ID
    ref_audio, ref_text = current_ref()
    with LOCK:
        chunks = []
        rate = SAMPLE_RATE
        for result in MODEL.generate(
            text=text,
            ref_audio=ref_audio,
            ref_text=ref_text,
            stream=True,
            streaming_interval=0.25,
            speed=SPEED,
            temperature=0.7,
        ):
            if my_id != REQUEST_ID:
                print("[tts] cancelled by newer request", flush=True)
                break
            chunks.append(np.array(result.audio))
            rate = getattr(result, "sample_rate", rate)
        if not chunks:
            return np.zeros(0, dtype=np.float32), rate
        audio = np.concatenate(chunks)
        remember_session(audio, rate, text)
        return audio, rate


def wav_bytes(text: str) -> bytes:
    audio, rate = generate(text)
    audio = extend_pauses(audio, rate)
    buffer = io.BytesIO()
    sf.write(buffer, audio, rate, format="WAV", subtype="PCM_16")
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
        if self.path == "/tts":
            self._full_tts()
        elif self.path == "/tts_stream":
            self._stream_tts()
        elif self.path == "/voice_reset":
            reset_session()
            self._send(200, b"ok", "text/plain")
        else:
            self._send(404, b"not found", "text/plain")

    def _read_text(self):
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        return (payload.get("text") or "").strip()

    def _full_tts(self):
        try:
            text = self._read_text()
            if not text:
                self._send(400, b"empty text", "text/plain")
                return
            self._send(200, wav_bytes(text), "audio/wav")
        except Exception as error:  # noqa: BLE001
            self._send(500, str(error).encode("utf-8", "replace"), "text/plain")

    def _stream_tts(self):
        global REQUEST_ID
        try:
            text = self._read_text()
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
            extender = PauseExtender(24000, PAUSE_EXTRA_MS, PAUSE_MIN_MS) if PAUSE else None
            ref_audio, ref_text = current_ref()
            session_audio = []
            with LOCK:
                for result in MODEL.generate(
                    text=text,
                    ref_audio=ref_audio,
                    ref_text=ref_text,
                    stream=True,
                    streaming_interval=0.25,
                    speed=SPEED,
                    temperature=0.7,
                ):
                    if my_id != REQUEST_ID:
                        print("[tts] stream cancelled by newer request", flush=True)
                        break
                    audio = np.array(result.audio, dtype=np.float32)
                    session_audio.append(audio)
                    if extender is not None:
                        audio = extender.push(audio)
                    pcm = pcm_bytes(audio)
                    if pcm:
                        self.wfile.write(f"{len(pcm):X}\r\n".encode("ascii"))
                        self.wfile.write(pcm)
                        self.wfile.write(b"\r\n")
                        self.wfile.flush()
                if extender is not None:
                    pcm = pcm_bytes(extender.flush())
                    if pcm:
                        self.wfile.write(f"{len(pcm):X}\r\n".encode("ascii"))
                        self.wfile.write(pcm)
                        self.wfile.write(b"\r\n")
                        self.wfile.flush()
                remember_session(np.concatenate(session_audio), 24000, text)
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:  # noqa: BLE001
            try:
                self._send(500, str(error).encode("utf-8", "replace"), "text/plain")
            except Exception:
                pass


def main():
    global MODEL, REF, REF_TEXT
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--ref-text", required=True)
    parser.add_argument("--port", type=int, default=8800)
    parser.add_argument("--speed", type=float, default=0.9)
    parser.add_argument("--pause", type=int, default=1)
    parser.add_argument("--pause-min", type=int, default=300)
    parser.add_argument("--pause-extra", type=int, default=250)
    parser.add_argument("--warmup", default="你好。")
    args = parser.parse_args()

    global SPEED, PAUSE, PAUSE_MIN_MS, PAUSE_EXTRA_MS
    SPEED = args.speed
    PAUSE = args.pause != 0
    PAUSE_MIN_MS = args.pause_min
    PAUSE_EXTRA_MS = args.pause_extra
    REF = args.ref
    REF_TEXT = args.ref_text
    print(f"[tts] loading {args.model}", flush=True)
    MODEL = load_model(args.model)
    print("[tts] warming up", flush=True)
    generate(args.warmup)
    print(f"[tts] ready on :{args.port} (24 kHz)", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
