// 照片级数字人：连接本地 sidecar（scripts/avatar_sidecar.py），
// 边生成边接收 10 fps JPEG 帧。预览用 /audio/preview.wav 与帧一起播放；
// 实时 TTS 流式接入时，音频由 Rust 播放，UI 只按 avatar-speech-start 的时钟取帧。
(() => {
  const state = {
    ws: null,
    connected: false,
    generating: false,
    playing: false,
    fps: 10,
    frames: [],
    clause: null,
    audioUrl: null,
    audio: null,
    raf: 0,
    lastIndex: -1,
    clockMode: false,
    clockStart: 0,
    started: false,
    done: false,
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

  function stopPlayback({ restore = false } = {}) {
    state.playing = false;
    state.started = false;
    state.clockMode = false;
    if (state.raf) cancelAnimationFrame(state.raf);
    state.raf = 0;
    if (state.audio) {
      state.audio.pause();
      state.audio.currentTime = 0;
      state.audio = null;
    }
    if (restore) setMode(false);
  }

  function showFrame(idx) {
    if (!state.frames.length) return;
    const i = Math.max(0, Math.min(state.frames.length - 1, idx));
    if (i === state.lastIndex) return;
    state.lastIndex = i;
    const el = img();
    if (el) el.src = state.frames[i].url;
  }

  function tick() {
    if (!state.playing) return;
    const pos = state.clockMode
      ? (performance.now() - state.clockStart) / 1000
      : (state.audio ? state.audio.currentTime : 0);
    const raw = Math.floor(pos * state.fps);
    showFrame(raw);
    const total = ((state.clause && state.clause.count) || state.frames.length) / state.fps;
    if (pos >= total || (!state.clockMode && state.audio && state.audio.ended)) {
      state.playing = false;
      status("预览结束");
      return;
    }
    state.raf = requestAnimationFrame(tick);
  }

  async function startAudio() {
    if (!state.audioUrl || state.playing || !state.frames.length) return;
    stopPlayback();
    setMode(true);
    showFrame(0);
    // 优先用 Rust 的 afplay：WKWebView 对 HTTP 音频/自动播放限制多。
    const invoke = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
    if (invoke) {
      try {
        await invoke("avatar_play_preview");
        state.audio = null;
        state.clockMode = true;
        state.clockStart = performance.now();
        state.started = true;
        state.playing = true;
        state.lastIndex = -1;
        showFrame(0);
        status("播放样片");
        state.raf = requestAnimationFrame(tick);
        return;
      } catch (err) {
        console.warn("[photo] afplay failed, fallback to <audio>", err);
      }
    }
    const audio = new Audio(state.audioUrl);
    audio.preload = "auto";
    state.audio = audio;
    state.clockMode = false;
    state.started = true;
    state.playing = true;
    state.lastIndex = -1;
    status("播放样片");
    try {
      await audio.play();
      state.raf = requestAnimationFrame(tick);
    } catch (err) {
      state.playing = false;
      state.started = true; // 不要每来一帧就重试；等用户再点预览
      status("点一下播放（浏览器需要用户手势）");
      console.warn("[photo] audio play failed", err);
    }
  }

  function maybeStart() {
    // 预览：等 sidecar 把整段帧生成完再播，保证嘴动是连续的；
    // 实时 TTS 才需要边到边播。
    if (state.audioUrl && state.done && !state.started && state.frames.length >= 3) {
      startAudio();
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
        // sidecar 重启后，旧帧/旧音频已经过期，清掉缓存，避免重播上一版画面。
        stopPlayback();
        clearFrames();
        state.clause = null;
        state.audioUrl = null;
        state.done = false;
        state.started = false;
        const btn = button();
        if (btn) {
          btn.hidden = false;
          btn.title = `本地 sidecar · ${msg.fps} fps · ${msg.frames} 帧 base`;
        }
        return;
      }
      if (msg.type === "clause") {
        stopPlayback();
        clearFrames();
        state.clause = msg;
        state.generating = true;
        state.done = false;
        state.audioUrl = msg.audio || null;
        state.fps = msg.fps || 10;
        state.started = false;
        status(`生成中… ${msg.count} 帧`);
        return;
      }
      if (msg.type === "done") {
        state.generating = false;
        state.done = true;
        maybeStart();
        if (!state.audioUrl && state.frames.length) showFrame(0);
        return;
      }
      if (msg.type === "stopped") {
        state.generating = false;
        stopPlayback();
        status("已停止");
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
      if (event.data.byteLength < 5) return;
      const view = new DataView(event.data);
      const blob = new Blob([event.data.slice(4)], { type: "image/jpeg" });
      state.frames.push({ pts: view.getUint32(0), url: URL.createObjectURL(blob) });
      maybeStart();
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
    // 已经生成过：直接重播缓存，不重新跑推理。
    if (state.frames.length && state.audioUrl) {
      unlockAudio();
      startAudio();
      return;
    }
    unlockAudio();
    state.generating = true;
    const btn = button();
    if (btn) btn.disabled = true;
    status("生成中…（本地 256²，10 fps）");
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
    window.addEventListener("keydown", (e) => {
      const el = document.activeElement;
      const typing =
        el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
      if (typing || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.code === "KeyP") {
        e.preventDefault();
        preview();
      }
    });
    connect();
  }

  window.GabTwinPhoto = {
    init,
    preview,
    stop: stopPlayback,
    startAudio,
    isConnected: () => state.connected,
    get frames() { return state.frames.length; },
  };
})();
