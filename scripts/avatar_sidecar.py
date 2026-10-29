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
import os
import struct
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import cv2
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "vendor" / "musetalk-mlx"))

from aiohttp import WSMsgType, web  # noqa: E402
from musetalk_mlx.pipeline_mlx import MuseTalkPipeline  # noqa: E402
import mlx.core as mx  # noqa: E402

FPS = 10
OUT_W, OUT_H = 432, 576
FACE = 256
# 统一清晰度模式：整张脸一起贴回，再对整帧做同样的柔化；默认开启。
UNIFORM = os.environ.get("GABTWIN_AVATAR_UNIFORM", "1").strip().lower() not in ("0", "off", "false", "no")
SOFTEN = float(os.environ.get("GABTWIN_AVATAR_SOFTEN", "1.35"))
RESTORE_MODE = os.environ.get("GABTWIN_AVATAR_RESTORE", "0").strip().lower()
RESTORE_PREVIEW = RESTORE_MODE in ("1", "all", "preview", "true", "yes")
RESTORE_ALL = RESTORE_MODE in ("1", "all", "true", "yes")
RESTORE_URL = os.environ.get("GABTWIN_GFPGAN_URL", "http://127.0.0.1:8812/restore")

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
    global FRAMES, LATENTS, BOXES_PER_FRAME, PIPE
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
        s = int(max(bw, bh) * 1.20)
        cx, cy = x + bw / 2, y + bh / 2
        x0 = max(0, min(w - s, int(round(cx - s / 2))))
        y0 = max(0, min(h - s, int(round(cy - s / 2))))
        crop = cv2.resize(fr[y0:y0 + s, x0:x0 + s], (FACE, FACE), interpolation=cv2.INTER_LANCZOS4)
        LATENTS.append(PIPE.get_latents_for_unet(crop))
        BOXES_PER_FRAME.append((x0, y0, s))
    print(f"precomputed {len(LATENTS)} latents in {time.time()-t0:.1f}s", flush=True)

    # 3:4 竖屏：源视频本身就是 3:4，直接缩放，不做模糊填充。
    print("portrait 3:4 ready", flush=True)


def paste(frame: np.ndarray, face: np.ndarray, box: tuple[int, int, int]) -> np.ndarray:
    x0, y0, s = box
    gen = cv2.resize(face, (s, s), interpolation=cv2.INTER_LANCZOS4)
    mask = np.zeros((s, s), np.float32)
    if UNIFORM:
        # 整张脸一起贴回：眼睛、鼻子、嘴、脸颊同源同清晰度，不在下半脸留拼接边。
        cv2.ellipse(
            mask,
            (int(s * 0.50), int(s * 0.47)),
            (int(s * 0.36), int(s * 0.47)),
            0, 0, 360, 1.0, -1,
        )
        mask = cv2.GaussianBlur(mask, (0, 0), max(8.0, s * 0.08))[..., None]
    else:
        # 质量模式：只替换下半脸，保留源视频更锐的眼睛、鼻子和脸颊。
        cv2.ellipse(
            mask,
            (int(s * 0.50), int(s * 0.68)),
            (int(s * 0.32), int(s * 0.21)),
            0, 0, 360, 1.0, -1,
        )
        mask = cv2.GaussianBlur(mask, (0, 0), max(6.0, s * 0.04))[..., None]
    region = frame[y0:y0 + s, x0:x0 + s].astype(np.float32)
    out = frame.copy()
    out[y0:y0 + s, x0:x0 + s] = np.clip(gen.astype(np.float32) * mask + region * (1 - mask), 0, 255).astype(np.uint8)
    return out


def compose_frame(src: np.ndarray, face: np.ndarray, box: tuple[int, int, int]) -> bytes | None:
    # 先把源帧缩到输出分辨率，再把 256² 生成脸直接贴到输出坐标：
    # 只重采样一次，避免 256 -> 386 -> 540 的双重柔化。
    scale = OUT_W / src.shape[1]
    portrait = cv2.resize(src, (OUT_W, OUT_H), interpolation=cv2.INTER_LANCZOS4)
    x0, y0, s = box
    x0 = max(0, min(OUT_W - 1, int(round(x0 * scale))))
    y0 = max(0, min(OUT_H - 1, int(round(y0 * scale))))
    s = max(8, min(min(OUT_W - x0, OUT_H - y0), int(round(s * scale))))
    portrait = paste(portrait, face, (x0, y0, s))
    # 输出分辨率下做一次轻度锐化，只动下半脸，避免全图发硬。
    roi = portrait[y0:y0 + s, x0:x0 + s]
    if roi.size:
        blur = cv2.GaussianBlur(roi, (0, 0), 1.0)
        sharp = cv2.addWeighted(roi, 1.35, blur, -0.35, 0)
        mask = np.zeros((s, s), np.float32)
        cv2.ellipse(mask, (int(s * 0.50), int(s * 0.68)), (int(s * 0.32), int(s * 0.21)), 0, 0, 360, 1.0, -1)
        mask = cv2.GaussianBlur(mask, (0, 0), max(6.0, s * 0.04))[..., None]
        portrait[y0:y0 + s, x0:x0 + s] = np.clip(sharp * mask + roi * (1 - mask), 0, 255).astype(np.uint8)
    if UNIFORM and SOFTEN > 0:
        # 整帧同一层柔化：源视频的清晰脸和生成的下半脸被拉到同一清晰度。
        portrait = cv2.GaussianBlur(portrait, (0, 0), SOFTEN)
    ok, buf = cv2.imencode(".jpg", portrait, [cv2.IMWRITE_JPEG_QUALITY, 92])
    return buf.tobytes() if ok else None


def restore_face(face: np.ndarray) -> np.ndarray:
    """把 256 生成脸交给 GFPGAN 小服务修成 512；失败就用原图，不阻塞出帧。"""
    if not (RESTORE_PREVIEW or RESTORE_ALL):
        return face
    try:
        ok, buf = cv2.imencode(".jpg", face, [cv2.IMWRITE_JPEG_QUALITY, 95])
        if not ok:
            return face
        req = urllib.request.Request(
            RESTORE_URL,
            data=buf.tobytes(),
            headers={"Content-Type": "image/jpeg"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = resp.read()
        out = cv2.imdecode(np.frombuffer(data, np.uint8), cv2.IMREAD_COLOR)
        return out if out is not None else face
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        print(f"GFPGAN restore 失败，使用原脸：{error}", flush=True)
        return face


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
        # preview 默认修复；实时 /speak 默认不修复，避免 10 fps 掉到 1 fps。
        use_restore = (not UNIFORM) and (RESTORE_ALL or (RESTORE_PREVIEW and audio_url is not None))
        if use_restore:
            # 先把整段 256 脸生成完，再统一走 GFPGAN；不要 MLX/Torch 交替抢显存。
            faces_all = []
            for start in range(0, n, 4):
                if STATE["cancel"]:
                    break
                end = min(n, start + 4)
                lat_stack = mx.concatenate([LATENTS[base_idx[i]] for i in range(start, end)], axis=0)
                faces_all.extend(PIPE.run_batched(lat_stack, chunks[start:end], batch_size=4))
            for j, face in enumerate(faces_all):
                if STATE["cancel"]:
                    break
                face = restore_face(face)
                jpg = compose_frame(FRAMES[base_idx[j]], face, BOXES_PER_FRAME[base_idx[j]])
                if jpg:
                    await broadcast_bytes(struct.pack(">I", int(round(j * 1000 / FPS))) + jpg)
                await asyncio.sleep(0)
        else:
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
