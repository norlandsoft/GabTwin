// 照片级数字人预览：连接本地 sidecar（scripts/avatar_sidecar.py），
// 接收 10 fps JPEG 帧，和 /audio/preview.wav 一起播放。
// 这是 A 方案的渲染/传输验证；实时 TTS 流式推理下一步接。
(() => {
  const state = {
    ws: null,
    connected: false,
    generating: false,
    playing: false,
    fps: 10,
    frames: [],
    audio: null,
    raf: 0,
    lastIndex: -1,
    retry: 0,
  };

  const img = () => document.getElementById("avatarPhoto");
  const button = () => document.getElementById("photoPreview");
  const status = (text) => {
    const el = document.getElementById("avatarMood");
    if (el && text) el.textContent = text;
  };

  function clearFrames() {
    for (const f of state.frames) URL.revokeObjectURL(f.url);
    state.frames = [];
    state.lastIndex = -1;
  }

  function setMode(on) {
    const stage = document.getElementById("avatarStage");
    if (!stage) return;
    stage.classList.toggle("photo-mode", on);
    const avatar = window.__gabtwinAvatar;
    if (avatar) avatar.visible = !on;
  }

  function stopPlayback() {
    state.playing = false;
    if (state.raf) cancelAnimationFrame(state.raf);
    state.raf = 0;
    if (state.audio) {
      state.audio.pause();
      state.audio.currentTime = 0;
      state.audio = null;
    }
  }

  function tick() {
    if (!state.playing || !state.audio) return;
    const idx = Math.min(
      state.frames.length - 1,
      Math.floor(state.audio.currentTime * state.fps)
    );
    if (idx !== state.lastIndex && state.frames[idx]) {
      state.lastIndex = idx;
      const el = img();
      if (el) el.src = state.frames[idx].url;
    }
    if (state.audio.ended || idx >= state.frames.length - 1) {
      state.playing = false;
      status("预览结束");
      return;
    }
    state.raf = requestAnimationFrame(tick);
  }

  async function playClip(clip) {
    if (!state.frames.length) {
      status("没有收到画面");
      return;
    }
    setMode(true);
    state.fps = clip.fps || 10;
    const first = img();
    if (first) first.src = state.frames[0].url;
    const audio = new Audio(clip.audio || "http://127.0.0.1:8811/audio/preview.wav");
    audio.preload = "auto";
    state.audio = audio;
    state.playing = true;
    state.lastIndex = -1;
    status("播放样片");
    try {
      await audio.play();
      state.raf = requestAnimationFrame(tick);
    } catch (err) {
      state.playing = false;
      status("浏览器拦截了音频，请再点一次");
      console.warn("[photo] audio play failed", err);
    }
  }

  function onMessage(event) {
    if (typeof event.data === "string") {
      let msg;
      try {
        msg = JSON.parse(event.data);
      } catch {
        return;
      }
      if (msg.type === "ready") {
        state.connected = true;
        const btn = button();
        if (btn) {
          btn.hidden = false;
          btn.title = `本地 sidecar · ${msg.fps} fps · ${msg.frames} 帧 base`;
        }
        return;
      }
      if (msg.type === "clip") {
        clearFrames();
        state.generating = true;
        state.clip = msg;
        status(`生成中… ${msg.count} 帧`);
        return;
      }
      if (msg.type === "done") {
        state.generating = false;
        playClip(state.clip || { fps: 10 });
        return;
      }
      if (msg.type === "error") {
        state.generating = false;
        status(`数字人出错：${msg.message || "未知"}`);
        return;
      }
      return;
    }
    if (event.data instanceof ArrayBuffer) {
      const view = new DataView(event.data);
      if (event.data.byteLength < 5) return;
      const pts = view.getUint32(0);
      const blob = new Blob([event.data.slice(4)], { type: "image/jpeg" });
      const url = URL.createObjectURL(blob);
      state.frames.push({ pts, url });
    }
  }

  function connect() {
    if (state.ws && state.ws.readyState <= 1) return;
    let ws;
    try {
      ws = new WebSocket("ws://127.0.0.1:8811/ws");
    } catch {
      return;
    }
    state.ws = ws;
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      state.retry = 0;
    };
    ws.onmessage = onMessage;
    ws.onclose = () => {
      state.connected = false;
      const btn = button();
      if (btn) btn.hidden = true;
      state.ws = null;
      state.retry = Math.min(10, state.retry + 1);
      setTimeout(connect, 3000 * state.retry);
    };
    ws.onerror = () => {
      try {
        ws.close();
      } catch {}
    };
  }

  function unlockAudio() {
    // 用户点击时先放一个静音片段，避免 15 秒生成后被自动播放策略拦截。
    try {
      const a = new Audio(
        "data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAESsAABErAAABAAgAZGF0YQAAAAA="
      );
      a.volume = 0;
      a.play().catch(() => {});
    } catch {}
  }

  async function preview() {
    if (!state.connected) {
      status("照片级 sidecar 未启动");
      return;
    }
    if (state.generating || state.playing) return;
    // 已经生成过：直接重播缓存，不重新跑 15 秒推理。
    if (state.frames.length && state.clip) {
      unlockAudio();
      playClip(state.clip);
      return;
    }
    unlockAudio();
    state.generating = true;
    const btn = button();
    if (btn) btn.disabled = true;
    status("生成中…（首次约 10–20 秒）");
    try {
      const resp = await fetch("http://127.0.0.1:8811/preview", { method: "POST" });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    } catch (err) {
      state.generating = false;
      status(`sidecar 请求失败：${err.message}`);
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  function init() {
    const btn = button();
    if (btn) btn.addEventListener("click", preview);
    connect();
  }

  window.GabTwinPhoto = { init, preview, stop: stopPlayback, isConnected: () => state.connected };
})();
