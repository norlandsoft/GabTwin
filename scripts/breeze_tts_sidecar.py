#!/usr/bin/env python3
"""Breeze TTS 2（MLX）本地 sidecar：加载一次模型，HTTP 返回 24 kHz WAV。

POST /tts  {"text": "...", "lang": "zh"}  -> audio/wav
GET  /health                              -> ok
"""
import argparse
import io
import json
import math
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import librosa
import numpy as np

try:
    import psola
except Exception:  # noqa: BLE001
    psola = None
import soundfile as sf
from mlx_audio.tts import load_model

MODEL = None
REF = None
REF_TEXT = ""
SAMPLE_RATE = 24000
LOCK = threading.Lock()
# 新请求到来时递增；正在生成旧请求的线程在下一个 chunk 处退出。
REQUEST_ID = 0

# 情绪稳定模式：默认关闭；由 .env 的 GABTWIN_BREEZE_* 打开。
INSTRUCT = os.environ.get("GABTWIN_BREEZE_INSTRUCT", "").strip()
TEMPERATURE = float(os.environ.get("GABTWIN_BREEZE_TEMPERATURE", "0.9"))
TOP_P = float(os.environ.get("GABTWIN_BREEZE_TOP_P", "1.0"))
TOP_K = int(os.environ.get("GABTWIN_BREEZE_TOP_K", "50"))
_cfg = os.environ.get("GABTWIN_BREEZE_CFG_SCALE", "").strip()
CFG_SCALE = float(_cfg) if _cfg else None
_seed = os.environ.get("GABTWIN_BREEZE_SEED", "").strip()
SEED = int(_seed) if _seed else None
AGC_ON = os.environ.get("GABTWIN_BREEZE_AGC", "0") != "0"
# 音调区间限制：把每句的中位 F0 拉回目标附近，避免突然高涨/低落。
PITCH_LIMIT = os.environ.get("GABTWIN_BREEZE_PITCH", "1") != "0"
PITCH_TARGET = float(os.environ.get("GABTWIN_BREEZE_PITCH_TARGET", "220"))
PITCH_MAX_SHIFT = float(os.environ.get("GABTWIN_BREEZE_PITCH_MAX_SHIFT", "4.0"))
PITCH_RANGE = float(os.environ.get("GABTWIN_BREEZE_PITCH_RANGE", "3.0"))
PITCH_COMPRESS = float(os.environ.get("GABTWIN_BREEZE_PITCH_COMPRESS", "0.65"))


class AGC:
    """慢速自动增益：把每段语音拉到同一目标响度，压住情绪带来的音量起伏。"""

    def __init__(self, target_dbfs: float = -20.0):
        self.target = target_dbfs
        self.gain = 1.0

    def process(self, audio: np.ndarray) -> np.ndarray:
        if audio.size == 0:
            return audio
        rms = float(np.sqrt(np.mean(audio * audio)))
        if rms < 1e-4:
            return (audio * self.gain).astype(np.float32)
        desired = 10 ** ((self.target - 20 * math.log10(rms)) / 20)
        desired = min(max(desired, 0.25), 4.0)
        # 变响时快速压，变轻时稍慢放；目标是把不同情绪的段拉到同一响度。
        # 0.7/0.3 实测三句“高兴/中性”的整段 RMS 落在 0.6 dB 内。
        alpha = 0.7 if desired < self.gain else 0.3
        self.gain = self.gain * (1 - alpha) + desired * alpha
        out = audio * self.gain
        peak = float(np.max(np.abs(out)))
        if peak > 0.95:
            out = out * (0.95 / peak)
        return out.astype(np.float32)


def limit_pitch(audio: np.ndarray, rate: int) -> np.ndarray:
    """限制一句话的 F0 中位和上下区间，不改时长。

    用 librosa 估 F0 轮廓，再用 Praat PSOLA（psola.vocode）按目标轮廓重合成：
    中位拉向目标，偏离按 PITCH_COMPRESS 压窄，最后夹在 ±PITCH_RANGE 个半音内。
    保留共振峰，避免“卡通音”。
    """
    if not PITCH_LIMIT or audio.size < int(rate * 0.35):
        return audio
    if psola is None:
        print("[pitch] psola 未安装，跳过", flush=True)
        return audio
    try:
        hop = 240  # 10 ms
        f0, _, _ = librosa.pyin(
            audio.astype(np.float32),
            fmin=70,
            fmax=400,
            sr=rate,
            frame_length=1024,
            hop_length=hop,
        )
        voiced = f0[np.isfinite(f0) & (f0 > 0)]
        if voiced.size < 8:
            return audio
        median = float(np.median(voiced))
        shift = 12.0 * math.log2(PITCH_TARGET / median)
        shift = max(-PITCH_MAX_SHIFT, min(PITCH_MAX_SHIFT, shift))
        target = median * (2.0 ** (shift / 12.0))
        # 句内偏离按比例压缩，再限制在目标 ±PITCH_RANGE 半音。
        contour = np.where(
            np.isfinite(f0) & (f0 > 0),
            target * np.power(f0 / median, PITCH_COMPRESS),
            np.nan,
        )
        lo = target * (2.0 ** (-PITCH_RANGE / 12.0))
        hi = target * (2.0 ** (PITCH_RANGE / 12.0))
        contour = np.clip(contour, lo, hi)
        # 把 10 ms 的 F0 帧插值到与音频等长的 100 Hz 目标轮廓。
        duration = audio.size / rate
        n = max(2, int(round(duration * 100)) + 1)
        src_t = np.arange(contour.size) * hop / rate
        dst_t = np.linspace(0.0, duration, n)
        mapped = np.interp(dst_t, src_t, np.nan_to_num(contour, nan=0.0))
        mapped[mapped < 1.0] = np.nan
        out = psola.vocode(
            audio.astype(np.float32),
            rate,
            target_pitch=mapped,
            fmin=70,
            fmax=400,
        )
        shifted = np.asarray(out, dtype=np.float32).reshape(-1)
        if shifted.size < audio.size:
            shifted = np.pad(shifted, (0, audio.size - shifted.size))
        elif shifted.size > audio.size:
            shifted = shifted[: audio.size]
        print(
            f"[pitch] median {median:.1f} -> {target:.1f} Hz "
            f"({shift:+.2f} st), range ±{PITCH_RANGE:.1f} st",
            flush=True,
        )
        return shifted
    except Exception as error:  # noqa: BLE001
        print(f"[pitch] PSOLA skipped: {error}", flush=True)
    # 兜底：整句常量移调（至少把中位拉回；不压区间）。
    try:
        y16 = librosa.resample(audio.astype(np.float32), orig_sr=rate, target_sr=16000)
        f0 = librosa.yin(y16, fmin=70, fmax=400, sr=16000, frame_length=1024, hop_length=160)
        voiced = f0[np.isfinite(f0) & (f0 > 0)]
        if voiced.size < 8:
            return audio
        median = float(np.median(voiced))
        semitones = 12.0 * math.log2(PITCH_TARGET / median)
        semitones = max(-PITCH_MAX_SHIFT, min(PITCH_MAX_SHIFT, semitones))
        if abs(semitones) < 0.15:
            return audio
        shifted = librosa.effects.pitch_shift(
            audio.astype(np.float32), sr=rate, n_steps=semitones, res_type="soxr_hq"
        )
        if shifted.size < audio.size:
            shifted = np.pad(shifted, (0, audio.size - shifted.size))
        elif shifted.size > audio.size:
            shifted = shifted[: audio.size]
        print(f"[pitch] fallback median {median:.1f} Hz {semitones:+.2f} st", flush=True)
        return shifted.astype(np.float32)
    except Exception as error:  # noqa: BLE001
        print(f"[pitch] skipped: {error}", flush=True)
        return audio


def generate_kwargs(text: str, interval: float) -> dict:
    # 很短的话（如开场的“你好”）不套稳定 instruct：greedy 在极短文本上
    # 容易不收敛，反而生成几十秒。短句用普通采样即可。
    stable = bool(INSTRUCT) and len(text) >= 8
    return dict(
        text=text,
        ref_audio=REF,
        ref_text=REF_TEXT,
        stream=True,
        streaming_interval=interval,
        max_tokens=token_budget(text),
        instruct=INSTRUCT if stable else None,
        temperature=TEMPERATURE if stable else 0.9,
        top_p=TOP_P,
        top_k=TOP_K,
        cfg_scale=CFG_SCALE if stable else None,
        seed=SEED if stable else None,
    )


def warmup(text: str) -> None:
    """只编译/预热，不套稳定模式，避免 greedy 在“你好”上跑满 token。"""
    if not text.strip():
        return
    for _ in MODEL.generate(
        text=text,
        ref_audio=REF,
        ref_text=REF_TEXT,
        stream=False,
        max_tokens=250,
        temperature=0.9,
        instruct=None,
    ):
        break


def token_budget(text: str) -> int:
    """Breeze 的 codec 是 25 Hz；默认 max_tokens=750 会在 60.0 s 处硬截断。

    按 8 tokens/字给余量（约 0.32 s/字），上限 6000（约 240 s）。
    """
    return max(750, min(6000, len(text) * 8))


def synth(text: str) -> bytes:
    global REQUEST_ID
    REQUEST_ID += 1
    my_id = REQUEST_ID
    with LOCK:
        chunks = []
        rate = SAMPLE_RATE
        # stream=True 让旧请求能在 chunk 之间被新请求打断，而不是一直占着模型。
        for result in MODEL.generate(**generate_kwargs(text, 0.5)):
            if my_id != REQUEST_ID:
                print("[tts] cancelled by newer request", flush=True)
                break
            chunks.append(np.array(result.audio))
            rate = getattr(result, "sample_rate", rate)
        audio = np.concatenate(chunks) if chunks else np.zeros(0, dtype=np.float32)
        if AGC_ON and audio.size:
            agc = AGC()
            block = max(1, int(rate * 0.25))
            audio = np.concatenate([
                agc.process(audio[i:i + block]) for i in range(0, len(audio), block)
            ])
        audio = limit_pitch(audio, rate)
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
            # 要限制音调，就得先拿到整句；先合成完，再分块回放。
            chunks = []
            rate = SAMPLE_RATE
            with LOCK:
                for result in MODEL.generate(**generate_kwargs(text, 0.25)):
                    if my_id != REQUEST_ID:
                        print("[tts] stream cancelled by newer request", flush=True)
                        break
                    chunks.append(np.array(result.audio, dtype=np.float32))
                    rate = getattr(result, "sample_rate", rate)
            audio = np.concatenate(chunks) if chunks else np.zeros(0, dtype=np.float32)
            if AGC_ON and audio.size:
                agc = AGC()
                block = max(1, int(rate * 0.25))
                audio = np.concatenate([
                    agc.process(audio[i:i + block]) for i in range(0, len(audio), block)
                ])
            audio = limit_pitch(audio, rate)
            audio = np.clip(audio * 2.2, -1.0, 1.0)
            pcm_all = (audio * 32767.0).astype("<i2").tobytes()
            self.send_response(200)
            self.send_header("Content-Type", "audio/L16; rate=24000")
            self.send_header("X-Sample-Rate", "24000")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            chunk_bytes = max(2, int(rate * 0.25) * 2)
            for start in range(0, len(pcm_all), chunk_bytes):
                pcm = pcm_all[start:start + chunk_bytes]
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
    warmup(args.warmup)
    print(f"[tts] ready on :{args.port} (24 kHz)", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
