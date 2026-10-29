const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;
const listen = tauri?.event?.listen;

const $ = (id) => document.getElementById(id);
const transcript = $("transcript");
const conversation = $("conversation");
const live = $("live");
const hint = $("hint");

let lastPartial = "";
let current = null; // { id, el, text }
let llmConfigured = false;
let llmMissingShown = false;
let audioState = "idle";
let avatar = null;
let avatarMoodTimer = null;
let avatarErrorTimer = null;

const AVATAR_MOOD = {
  idle: "在这里",
  listening: "在听你说",
  thinking: "在想…",
  speaking: "在说",
  interrupted: "被逗到了",
  error: "有点慌",
};

function setAvatarState(state) {
  const mood = $("avatarMood");
  if (mood) mood.textContent = AVATAR_MOOD[state] || "";
  if (!avatar) return;
  avatar.setState(state);
  clearTimeout(avatarMoodTimer);
  clearTimeout(avatarErrorTimer);
  if (state === "interrupted") {
    avatarMoodTimer = setTimeout(() => {
      const next =
        audioState === "listening" || audioState === "replaying" ? "listening" : "idle";
      avatar?.setState(next);
      if (mood) mood.textContent = AVATAR_MOOD[next];
    }, 650);
  }
}

function recoverAvatarSoon(delay = 1800) {
  clearTimeout(avatarErrorTimer);
  avatarErrorTimer = setTimeout(() => {
    avatarErrorTimer = null;
    if (!avatar || avatar.isSpeaking()) return;
    const next =
      audioState === "listening" || audioState === "replaying" ? "listening" : "idle";
    setAvatarState(next);
  }, delay);
}

const STATE_TEXT = {
  idle: "待机",
  loading: "载入模型…",
  listening: "聆听中",
  replaying: "回放中",
  thinking: "思考中",
  replying: "回复中",
  speaking: "说话中",
  error: "错误",
};

const ACTIVE = new Set(["loading", "listening", "replaying", "thinking", "replying", "speaking"]);
let speakingTimer = null;

function clearSpeakingTimer() {
  if (speakingTimer) {
    clearTimeout(speakingTimer);
    speakingTimer = null;
  }
}

function setStatus(state, message) {
  const el = document.querySelector(".status");
  el.dataset.state = state;
  $("statusText").textContent = message || STATE_TEXT[state] || state;
  document.body.dataset.state = state;

  const running = ACTIVE.has(state);
  $("start").disabled = running;
  $("stop").disabled = !running;
  $("sample").disabled = running;

  if (state === "error") {
    hint.textContent = message || "出错了";
    hint.classList.add("error");
    hint.style.display = "";
  }
}

function syncHint() {
  if (conversation.children.length > 0) {
    hint.style.display = "none";
  }
}

function clearAll() {
  conversation.innerHTML = "";
  live.innerHTML = "";
  lastPartial = "";
  current = null;
  $("first").textContent = "—";
  $("endpoint").textContent = "—";
  $("ttft").textContent = "—";
  $("firstAudio").textContent = "—";
  $("rtf").textContent = "—";
  clearSpeakingTimer();
  hint.classList.remove("error");
  hint.textContent = "按「开始聆听」，然后说话。它会在你说完后接话。";
  hint.style.display = "";
}

// 模型对英文常输出全大写；显示层收一下，日志里保留原文。
const UPPER_KEEP = new Set([
  "I", "IM", "ILL", "IVE", "OK", "API", "JSON", "USB", "AI", "CPU", "GPU",
  "HTTP", "URL", "ID", "PC", "UI", "ASR", "TTS", "LLM", "SDK", "CLI",
]);

function tidy(text) {
  let out = text.replace(/\b[A-Z][A-Z'’]*\b/g, (m) => {
    const letters = m.replace(/['’]/g, "");
    if (letters === "I") return m;
    if (letters.length < 2) return m.toLowerCase();
    if (UPPER_KEEP.has(letters)) return m;
    return m.toLowerCase();
  });
  if (!/[\u4e00-\u9fff]/.test(out)) {
    out = out.replace(/^[a-z]/, (c) => c.toUpperCase());
  }
  return out;
}

function escapeHtml(text) {
  return text.replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[c]));
}

function commonPrefix(a, b) {
  const n = Math.min(a.length, b.length);
  let i = 0;
  while (i < n && a[i] === b[i]) i += 1;
  return b.slice(0, i);
}

function scrollToBottom() {
  transcript.scrollTop = transcript.scrollHeight;
}

function appendUser(text) {
  const p = document.createElement("p");
  p.className = "turn user";
  p.textContent = tidy(text);
  conversation.appendChild(p);
  syncHint();
  scrollToBottom();
}

function startAssistant(turnId) {
  const p = document.createElement("p");
  p.className = "turn assistant streaming";
  conversation.appendChild(p);
  current = { id: turnId, el: p, text: "" };
  syncHint();
  scrollToBottom();
}

function finishCurrent() {
  if (current) {
    current.el.classList.remove("streaming");
    current = null;
  }
}

function renderLive(raw, onsetMs) {
  const text = tidy(raw);
  const stable = commonPrefix(lastPartial, text);
  live.innerHTML =
    `<span>${escapeHtml(stable)}</span>` +
    `<span class="tail">${escapeHtml(text.slice(stable.length))}</span>`;
  lastPartial = text;
  if ($("first").textContent === "—" && onsetMs != null) {
    $("first").textContent = `${onsetMs} ms`;
  }
  scrollToBottom();
}

function onSessionEvent(payload) {
  switch (payload.kind) {
    case "state": {
      const state = payload.state;
      if (state === "offline_ready") break;
      if (state === "loading") {
        clearAll();
        audioState = "loading";
        setAvatarState("idle");
        setStatus("loading");
      } else if (state === "listening") {
        audioState = "listening";
        setAvatarState("listening");
        setStatus("listening");
      } else if (state === "replaying") {
        audioState = "replaying";
        setAvatarState("listening");
        setStatus("replaying");
      } else if (state === "idle") {
        audioState = "idle";
        if (!avatar?.isSpeaking()) setAvatarState("idle");
        setStatus(current ? "replying" : "idle");
      }
      break;
    }
    case "device":
      $("device").textContent = `${payload.name} · ${(payload.sample_rate / 1000).toFixed(1)} kHz · ${payload.channels} ch`;
      break;
    case "level":
      $("level").style.width = `${Math.min(100, payload.rms * 380)}%`;
      avatar?.setMicLevel(payload.rms);
      break;
    case "partial":
      renderLive(payload.text, payload.onset_ms);
      break;
    case "segment":
      clearSpeakingTimer();
      appendUser(payload.text);
      live.innerHTML = "";
      lastPartial = "";
      $("endpoint").textContent = `${payload.latency_ms} ms`;
      if (llmConfigured) {
        setAvatarState("thinking");
        setStatus("thinking");
      } else {
        setAvatarState("listening");
        setStatus(audioState === "replaying" ? "replaying" : "listening");
      }
      break;
    case "metrics":
      if (payload.first_partial_ms != null) {
        $("first").textContent = `${payload.first_partial_ms} ms`;
      }
      $("rtf").textContent = payload.rtf.toFixed(2);
      break;
    case "warning":
      hint.style.display = "";
      hint.classList.remove("error");
      hint.textContent = payload.message;
      break;
    case "error":
      setAvatarState("error");
      recoverAvatarSoon(2400);
      setStatus("error", payload.message);
      break;
  }
}

function onTtsStart(payload) {
  clearSpeakingTimer();
  if (payload.first_audio_ms != null) {
    $("firstAudio").textContent = `${payload.first_audio_ms} ms`;
  }
  setAvatarState("speaking");
  setStatus("speaking");
}

function onTtsQueued(payload) {
  clearSpeakingTimer();
  const wait = Math.max(300, (payload.duration_ms || 0) + 150);
  speakingTimer = setTimeout(() => {
    speakingTimer = null;
    const next = audioState === "listening" || audioState === "replaying" ? "listening" : "idle";
    setAvatarState(next);
    setStatus(audioState === "idle" ? "idle" : audioState);
  }, wait);
}

function onLlmThinking(payload) {
  if (current) {
    current.el.remove();
    current = null;
  }
  startAssistant(payload.turn_id);
  setAvatarState("thinking");
  setStatus("thinking");
}

function onLlmDelta(payload) {
  if (!current || payload.turn_id !== current.id) return;
  current.text += payload.text;
  current.el.textContent = current.text;
  if (!avatar?.isSpeaking()) setAvatarState("thinking");
  setStatus("replying");
  scrollToBottom();
}

function onLlmDone(payload) {
  if (!current || payload.turn_id !== current.id) return;
  current.el.classList.remove("streaming");
  current.el.textContent = payload.text;
  if (payload.first_token_ms != null) {
    $("ttft").textContent = `${payload.first_token_ms} ms`;
  }
  current = null;
  if (!avatar?.isSpeaking()) {
    setAvatarState(audioState === "listening" ? "listening" : "idle");
  }
  setStatus(audioState === "idle" ? "idle" : audioState);
  scrollToBottom();
}

function onLlmError(payload) {
  clearSpeakingTimer();
  setAvatarState("error");
  recoverAvatarSoon();
  if (payload.turn_id === 0) {
    if (!llmMissingShown) {
      llmMissingShown = true;
      $("llm").textContent = "未配置 LLM key";
      hint.style.display = "";
      hint.classList.add("error");
      hint.textContent = payload.message;
    }
    return;
  }
  if (current && payload.turn_id === current.id) {
    current.el.classList.remove("streaming");
    current.el.classList.add("error");
    current.el.textContent = payload.message;
    current = null;
    setStatus(audioState === "idle" ? "idle" : audioState);
  }
}

function onLlmCancelled(payload) {
  if (current && payload.turn_id === current.id) {
    current.el.remove();
    current = null;
  }
  if (avatar?.isSpeaking()) {
    setAvatarState("interrupted");
  } else {
    setAvatarState(audioState === "listening" ? "listening" : "idle");
  }
  setStatus(audioState === "idle" ? "idle" : audioState);
}

async function guard(promise) {
  try {
    await promise;
  } catch (err) {
    setStatus("error", String(err));
  }
}

function initAvatar() {
  if (!window.GabTwinAvatar) {
    console.warn("[avatar] vendor/avatar.js 未加载");
    return;
  }
  avatar = window.GabTwinAvatar.init({ canvas: $("avatarCanvas") });
  if (avatar) {
    window.__gabtwinAvatar = avatar;
    setAvatarState("idle");
  }
}

initAvatar();
window.GabTwinPhoto?.init();

if (!invoke) {
  hint.textContent = "Tauri API 未注入：请用 GabTwin 应用打开，而不是浏览器。";
} else {
  listen("session-event", (e) => onSessionEvent(e.payload || {}));
  listen("avatar-event", (e) => {
    const payload = e.payload || {};
    if (!avatar) return;
    if (payload.kind === "frame") {
      avatar.setLevel(payload.rms || 0);
      avatar.setPlayedMs(payload.played_ms || 0);
    } else if (payload.kind === "stop") {
      if (avatar.isSpeaking()) setAvatarState("interrupted");
    }
  });
  $("clearMemory").addEventListener("click", () => {
    if (invoke) invoke("clear_memory").catch(() => {});
    conversation.innerHTML = "";
    live.innerHTML = "";
    hint.textContent = "记忆已清空，重新开始。";
    setAvatarState("idle");
  });
  const settingsModal = $("settingsModal");
  const personaInput = $("personaInput");
  const personaStatus = $("personaStatus");
  const setPersonaStatus = (text, isError = false) => {
    if (!personaStatus) return;
    personaStatus.textContent = text;
    personaStatus.style.color = isError ? "#e07a7a" : "var(--muted)";
  };
  const closeSettings = () => settingsModal.classList.add("hidden");
  const openSettings = () => {
    if (!invoke) return;
    setPersonaStatus("");
    invoke("get_persona")
      .then((text) => {
        personaInput.value = text || "";
        settingsModal.classList.remove("hidden");
        personaInput.focus();
      })
      .catch((err) => setPersonaStatus(String(err), true));
  };
  $("settings").addEventListener("click", openSettings);
  $("settingsClose").addEventListener("click", closeSettings);
  settingsModal.addEventListener("click", (event) => {
    if (event.target === settingsModal) closeSettings();
  });
  $("personaSave").addEventListener("click", async () => {
    const text = personaInput.value.trim();
    if (!text) {
      setPersonaStatus("人设不能为空", true);
      return;
    }
    try {
      const saved = await invoke("set_persona", { text });
      personaInput.value = saved;
      setPersonaStatus("已保存，下一轮生效");
    } catch (err) {
      setPersonaStatus(String(err), true);
    }
  });
  $("personaReset").addEventListener("click", async () => {
    try {
      const text = await invoke("reset_persona");
      personaInput.value = text;
      setPersonaStatus("已恢复默认，下一轮生效");
    } catch (err) {
      setPersonaStatus(String(err), true);
    }
  });
  let pttHeld = false;
  const isTyping = (el) =>
    !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
  const setPttLabel = (text) => {
    const el = $("ptt");
    if (el) el.textContent = text;
  };
  window.addEventListener("keydown", (e) => {
    if (e.code !== "Space" || e.repeat || isTyping(document.activeElement)) return;
    e.preventDefault();
    if (!invoke || pttHeld) return;
    pttHeld = true;
    setPttLabel("正在听…松开空格发送");
    window.GabTwinPhoto?.stop();
    setAvatarState("listening");
    invoke("ptt_press").catch(() => {});
  });
  window.addEventListener("keyup", (e) => {
    if (e.code !== "Space" || isTyping(document.activeElement)) return;
    e.preventDefault();
    if (!invoke || !pttHeld) return;
    pttHeld = false;
    setPttLabel("按住空格说话");
    invoke("ptt_release").catch(() => {});
  });
  window.addEventListener("blur", () => {
    if (!pttHeld || !invoke) return;
    pttHeld = false;
    setPttLabel("按住空格说话");
    invoke("ptt_release").catch(() => {});
  });
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && invoke) {
      if (!settingsModal.classList.contains("hidden")) {
        closeSettings();
        return;
      }
      setAvatarState("interrupted");
      invoke("interrupt").catch(() => {});
    }
  });
  listen("tts-engine", (e) => {
    const payload = e.payload || {};
    const el = $("ttsEngine");
    if (el) {
      el.textContent = `TTS ${payload.engine || "—"}`;
      el.title = payload.detail || "";
    }
  });
  if (invoke) {
    invoke("tts_info")
      .then((info) => {
        const el = $("ttsEngine");
        if (el && info) {
          el.textContent = `TTS ${info.engine || "—"}`;
          el.title = info.detail || "";
        }
      })
      .catch(() => {});
  }
  listen("llm-thinking", (e) => onLlmThinking(e.payload || {}));
  listen("llm-delta", (e) => onLlmDelta(e.payload || {}));
  listen("llm-done", (e) => onLlmDone(e.payload || {}));
  listen("llm-error", (e) => onLlmError(e.payload || {}));
  listen("llm-cancelled", (e) => onLlmCancelled(e.payload || {}));
  listen("tts-start", (e) => onTtsStart(e.payload || {}));
  listen("tts-queued", (e) => onTtsQueued(e.payload || {}));
  listen("tts-error", (e) => {
    const message = (e.payload || {}).message || "TTS 不可用";
    setAvatarState("error");
    recoverAvatarSoon(2400);
    hint.style.display = "";
    hint.classList.add("error");
    hint.textContent = message;
  });

  $("start").addEventListener("click", () => {
    window.GabTwinPhoto?.stop();
    clearAll();
    setStatus("loading");
    guard(invoke("start_listening"));
  });

  $("stop").addEventListener("click", () => {
    $("stop").disabled = true;
    setAvatarState("idle");
    guard(invoke("stop_listening"));
  });

  $("sample").addEventListener("click", () => {
    clearAll();
    setStatus("loading");
    guard(invoke("replay_sample"));
  });

  invoke("llm_status")
    .then((info) => {
      llmConfigured = !!info.configured;
      $("llm").textContent = info.configured ? `LLM ${String(info.model || "").split("/").pop()}` : "未配置 LLM key";
      $("llm").title = info.configured
        ? "每次定稿后自动发起一轮流式回复"
        : "设置 DEEPSEEK_API_KEY 后重启应用";
    })
    .catch(() => {});

  invoke("app_info")
    .then((info) => {
      $("device").title = info.model_dir;
    })
    .catch(() => {});

  setStatus("idle");
  setAvatarState("idle");
}
