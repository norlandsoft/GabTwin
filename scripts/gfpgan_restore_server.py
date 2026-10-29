#!/usr/bin/env python3
"""GFPGAN 人脸修复小服务：sidecar 每帧把生成的 256 脸 POST 过来，返回 512 修复图。

单独进程运行（.venv-cosy，torch + MPS），避免把 torch 塞进 MuseTalk 的 mlx 环境。
"""
from __future__ import annotations

import io
import os
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import cv2
import numpy as np
import torch
from gfpgan import GFPGANer

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MODEL = os.environ.get(
    "GABTWIN_GFPGAN_MODEL",
    os.path.join(ROOT, "models/avatar/gfpgan/GFPGANv1.4.pth"),
)
WEIGHT = float(os.environ.get("GABTWIN_GFPGAN_WEIGHT", "0.7"))
PORT = int(os.environ.get("GABTWIN_GFPGAN_PORT", "8812"))

print(f"loading GFPGAN: {MODEL}", flush=True)
t0 = time.time()
restorer = GFPGANer(
    model_path=MODEL,
    upscale=2,
    arch="clean",
    channel_multiplier=2,
    bg_upsampler=None,
    device=torch.device("mps"),
)
print(f"GFPGAN ready in {time.time() - t0:.1f}s", flush=True)


def restore(bgr: np.ndarray) -> tuple[np.ndarray, float]:
    t = time.time()
    _, _, output = restorer.enhance(
        bgr,
        has_aligned=False,
        only_center_face=True,
        paste_back=True,
        weight=WEIGHT,
    )
    if torch.backends.mps.is_available():
        torch.mps.synchronize()
    if output is None:
        return bgr, (time.time() - t) * 1000
    return output, (time.time() - t) * 1000


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path == "/health":
            body = b'{"ok":true}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path != "/restore":
            self.send_error(404)
            return
        n = int(self.headers.get("Content-Length", "0"))
        data = self.rfile.read(n) if n else b""
        img = cv2.imdecode(np.frombuffer(data, np.uint8), cv2.IMREAD_COLOR) if data else None
        if img is None:
            self.send_error(400, "bad image")
            return
        out, ms = restore(img)
        ok, buf = cv2.imencode(".jpg", out, [cv2.IMWRITE_JPEG_QUALITY, 95])
        if not ok:
            self.send_error(500, "encode failed")
            return
        payload = buf.tobytes()
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("X-Restore-Ms", str(round(ms)))
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    print(f"GFPGAN restore server on http://127.0.0.1:{PORT} (weight={WEIGHT})", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
