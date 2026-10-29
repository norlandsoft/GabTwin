#!/usr/bin/env python3
"""GabTwin 照片级数字人 sidecar（MuseTalk 1.5 MLX，本地）。

这是 A 方案的 **预览/渲染 sidecar**：加载固定 base 视频的 75 帧与 latent，
收到 /preview 后用一段 WAV 生成 256² 全画质脸，贴回源帧，合成 1280×720
宽幅（模糊背景 + 居中竖屏），通过 WebSocket 以 JPEG 帧发给 UI。

实时 TTS 流式推理（边合成边出帧）是下一步；当前先把「照片级画面 +
宽幅 + 模糊填充」在应用里跑通。

启动：
  .venv-avatar/bin/python scripts/avatar_sidecar.py \
      --model models/avatar/musetalk-1.5-q8 \
      --frames workspace/avatar/frames3s \
      --audio workspace/avatar/preview.wav \
      --port 8811
"""
from __future__ import annotations

import argparse
import asyncio
import json
import struct
import sys
import time
from pathlib import Path

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "vendor" / "musetalk-mlx"))

from aiohttp import WSMsgType, web  # noqa: E402
from musetalk_mlx.pipeline_mlx import MuseTalkPipeline  # noqa: E402
import mlx.core as mx  # noqa: E402

FPS = 10
OUT_W, OUT_H = 1280, 720
FACE = 256

# Vision 在 9.0–12.0 s 每 0.5 s 检出的 (t, x, y, w, h)；与样片脚本一致。
BOXES = [
    (9.0, 280, 218, 317, 317),
    (9.5, 271, 221, 322, 322),
    (10.0, 275, 223, 309, 309),
    (10.5, 275, 222, 314, 314),
    (11.0, 272, 204, 335, 335),
    (11.5, 275, 216, 317, 317),
    (12.0, 273, 216, 329, 329),
]

STATE: dict = {"generating": False, "cancel": False}
CLIENTS: set[web.WebSocketResponse] = set()
PIPE: MuseTalkPipeline | None = None
FRAMES: list[np.ndarray] = []
LATENTS: list = []
BOXES_PER_FRAME: list[tuple[int, int, int]] = []
BG: np.ndarray | None = None
PREVIEW_WAV: Path | None = None


def box_at(t: float) -> tuple[float, float, float, float]:
    if t <= BOXES[0][0]:
        a = b = BOXES[0]
    elif t >= BOXES[-1][0]:
        a = b = BOXES[-1]
    else:
        a = b = BOXES[0]
        for j in range(len(BOXES) - 1):
            if BOXES[j][0] <= t <= BOXES[j + 1][0]:
                a, b = BOXES[j], BOXES[j + 1]
                break
    u = (t - a[0]) / (b[0] - a[0]) if b[0] != a[0] else 0.0
    return tuple(a[k] + (b[k] - a[k]) * u for k in range(1, 5))


def prepare(frames_dir: Path) -> None:
    global FRAMES, LATENTS, BOXES_PER_FRAME, BG, PIPE
    files = sorted(frames_dir.glob("*.png"))
    if not files:
        raise SystemExit(f"no frames in {frames_dir}")
    FRAMES = [cv2.imread(str(p)) for p in files]
    h, w = FRAMES[0].shape[:2]
    print(f"base frames: {len(FRAMES)} @ {w}x{h}", flush=True)

    t0 = time.time()
    LATENTS = []
    BOXES_PER_FRAME = []
    for i, fr in enumerate(FRAMES):
        t = 9.0 + i / 25.0
        x, y, bw, bh = box_at(t)
        s = int(max(bw, bh) * 1.42)
        cx, cy = x + bw / 2, y + bh / 2
        x0 = max(0, min(w - s, int(round(cx - s / 2))))
        y0 = max(0, min(h - s, int(round(cy - s / 2))))
        crop = cv2.resize(fr[y0:y0 + s, x0:x0 + s], (FACE, FACE), interpolation=cv2.INTER_LANCZOS4)
        LATENTS.append(PIPE.get_latents_for_unet(crop))
        BOXES_PER_FRAME.append((x0, y0, s))
    print(f"precomputed {len(LATENTS)} latents in {time.time()-t0:.1f}s", flush=True)

    # 静态模糊背景 + 前景位置：宽幅 + 模糊填充
    sc = max(OUT_W / w, OUT_H / h)
    rw, rh = int(w * sc + 0.5), int(h * sc + 0.5)
    bg = cv2.resize(FRAMES[0], (rw, rh))
    bx, by = (rw - OUT_W) // 2, (rh - OUT_H) // 2
    bg = bg[by:by + OUT_H, bx:bx + OUT_W]
    bg = cv2.GaussianBlur(bg, (0, 0), 26)
    BG = (bg * 0.55).astype(np.uint8)
    print("background ready", flush=True)


def paste(frame: np.ndarray, face: np.ndarray, box: tuple[int, int, int]) -> np.ndarray:
    x0, y0, s = box
    gen = cv2.resize(face, (s, s), interpolation=cv2.INTER_LANCZOS4)
    border = max(16, min(40, s // 10))
    mask = np.zeros((s, s), np.float32)
    mask[border:s - border, border:s - border] = 1.0
    mask = cv2.GaussianBlur(mask, (0, 0), border * 0.55)[..., None]
    region = frame[y0:y0 + s, x0:x0 + s].astype(np.float32)
    out = frame.copy()
    out[y0:y0 + s, x0:x0 + s] = np.clip(gen.astype(np.float32) * mask + region * (1 - mask), 0, 255).astype(np.uint8)
    return out


def compose_frame(src: np.ndarray, face: np.ndarray, box: tuple[int, int, int]) -> bytes | None:
    assert BG is not None
    src = paste(src, face, box)
    wide = BG.copy()
    fh = OUT_H
    fw = int(round(src.shape[1] * fh / src.shape[0]))
    fx = (OUT_W - fw) // 2
    fg = cv2.resize(src, (fw, fh), interpolation=cv2.INTER_LANCZOS4)
    wide[:, fx:fx + fw] = fg
    ok, buf = cv2.imencode(".jpg", wide, [cv2.IMWRITE_JPEG_QUALITY, 86])
    return buf.tobytes() if ok else None


async def run_clause(wav: Path, clause_id: str, audio_url: str | None = None) -> None:
    """生成一个 clause 的帧，边生成边通过 WebSocket 发出去。"""
    assert PIPE is not None
    STATE["generating"] = True
    STATE["cancel"] = False
    try:
        chunks = PIPE.encode_audio_from_wav(str(wav), fps=FPS)
        mx.eval(chunks)
        n = chunks.shape[0]
        print(f"clause {clause_id}: {n} chunks @ {FPS} fps", flush=True)
        await broadcast_json({
            "type": "clause",
            "clause_id": clause_id,
            "fps": FPS,
            "count": n,
            "audio": audio_url,
        })
        # 10 fps 输出：每帧取源视频 25 fps 的对应帧（循环）
        base_idx = [min(len(FRAMES) - 1, int(round(i * 25 / FPS))) % len(FRAMES) for i in range(n)]
        for start in range(0, n, 4):
            if STATE["cancel"]:
                break
            end = min(n, start + 4)
            lat_stack = mx.concatenate([LATENTS[base_idx[i]] for i in range(start, end)], axis=0)
            faces = PIPE.run_batched(lat_stack, chunks[start:end], batch_size=4)
            for j in range(start, end):
                jpg = compose_frame(FRAMES[base_idx[j]], faces[j - start], BOXES_PER_FRAME[base_idx[j]])
                if jpg:
                    await broadcast_bytes(struct.pack(">I", int(round(j * 1000 / FPS))) + jpg)
            await asyncio.sleep(0)
        if STATE["cancel"]:
            await broadcast_json({"type": "stopped", "clause_id": clause_id})
        else:
            await broadcast_json({"type": "done", "clause_id": clause_id, "count": n})
    except Exception as error:  # noqa: BLE001
        print(f"clause failed: {error}", flush=True)
        await broadcast_json({"type": "error", "message": str(error)})
    finally:
        STATE["generating"] = False


async def broadcast_json(payload: dict) -> None:
    for ws in list(CLIENTS):
        try:
            await ws.send_json(payload)
        except Exception:
            CLIENTS.discard(ws)


async def broadcast_bytes(payload: bytes) -> None:
    for ws in list(CLIENTS):
        try:
            await ws.send_bytes(payload)
        except Exception:
            CLIENTS.discard(ws)


async def run_preview() -> None:
    assert PREVIEW_WAV is not None
    await run_clause(PREVIEW_WAV, "preview", "/audio/preview.wav")


async def ws_handler(request: web.Request) -> web.WebSocketResponse:
    ws = web.WebSocketResponse(heartbeat=30)
    await ws.prepare(request)
    CLIENTS.add(ws)
    await ws.send_json({"type": "ready", "fps": FPS, "frames": len(FRAMES)})
    try:
        async for msg in ws:
            if msg.type == WSMsgType.TEXT and msg.data == "ping":
                await ws.send_json({"type": "pong"})
    finally:
        CLIENTS.discard(ws)
    return ws


async def preview_handler(request: web.Request) -> web.Response:
    if STATE["generating"]:
        return web.json_response({"status": "busy"}, status=409)
    asyncio.create_task(run_preview())
    return web.json_response({"status": "started", "fps": FPS})


async def speak_handler(request: web.Request) -> web.Response:
    if STATE["generating"]:
        return web.json_response({"status": "busy"}, status=409)
    try:
        data = await request.json()
    except Exception:
        return web.json_response({"status": "bad-json"}, status=400)
    wav = Path(str(data.get("wav", "")))
    if not wav.is_file():
        return web.json_response({"status": "missing-wav"}, status=400)
    clause_id = str(data.get("clause_id") or f"clause-{time.time():.3f}")
    asyncio.create_task(run_clause(wav, clause_id))
    return web.json_response({"status": "started", "clause_id": clause_id, "fps": FPS})


async def stop_handler(request: web.Request) -> web.Response:
    STATE["cancel"] = True
    return web.json_response({"status": "stopping"})


async def health_handler(request: web.Request) -> web.Response:
    return web.json_response({
        "ok": True,
        "fps": FPS,
        "frames": len(FRAMES),
        "generating": STATE["generating"],
    })


async def audio_handler(request: web.Request) -> web.StreamResponse:
    assert PREVIEW_WAV is not None
    return web.FileResponse(PREVIEW_WAV)


@web.middleware
async def cors(request: web.Request, handler):
    if request.method == "OPTIONS":
        resp = web.Response()
    else:
        resp = await handler(request)
    resp.headers["Access-Control-Allow-Origin"] = "*"
    resp.headers["Access-Control-Allow-Methods"] = "GET,POST,OPTIONS"
    resp.headers["Access-Control-Allow-Headers"] = "Content-Type"
    return resp


def main() -> None:
    global PIPE, PREVIEW_WAV
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default=str(ROOT / "models/avatar/musetalk-1.5-q8"))
    ap.add_argument("--frames", default=str(ROOT / "workspace/avatar/frames3s"))
    ap.add_argument("--audio", default=str(ROOT / "workspace/avatar/preview.wav"))
    ap.add_argument("--port", type=int, default=8811)
    args = ap.parse_args()

    mx.set_default_device(mx.gpu)
    t0 = time.time()
    PIPE = MuseTalkPipeline.from_pretrained_mlx(args.model)
    print(f"model loaded in {time.time()-t0:.1f}s", flush=True)
    prepare(Path(args.frames))
    PREVIEW_WAV = Path(args.audio)
    print(f"audio: {PREVIEW_WAV} ({PREVIEW_WAV.stat().st_size/1024:.0f} KB)", flush=True)

    app = web.Application(middlewares=[cors])
    app.add_routes([
        web.get("/health", health_handler),
        web.get("/ws", ws_handler),
        web.get("/audio/preview.wav", audio_handler),
        web.post("/preview", preview_handler),
        web.post("/speak", speak_handler),
        web.post("/stop", stop_handler),
    ])
    print(f"avatar sidecar on http://127.0.0.1:{args.port} (ws /ws)", flush=True)
    web.run_app(app, host="127.0.0.1", port=args.port, print=None)


if __name__ == "__main__":
    main()
