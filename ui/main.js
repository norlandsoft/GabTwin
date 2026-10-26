const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;
const listen = tauri?.event?.listen;

const $ = (id) => document.getElementById(id);
const captions = $("captions");
const hint = $("hint");

let lastPartial = "";
let running = false;

const STATE_TEXT = {
  idle: "待机",
  loading: "载入模型…",
  listening: "聆听中",
  replaying: "回放中",
  error: "错误",
};

const HINT_TEXT = {
  idle: "按「开始聆听」，然后说话。文字会随着你说，边出现。",
  loading: "正在载入识别模型…",
  listening: "说吧，我在听。",
  replaying: "回放示例音频（不需要麦克风）…",
  error: "",
};

function setStatus(state, message) {
  const el = document.querySelector(".status");
  el.dataset.state = state;
  $("statusText").textContent = message || STATE_TEXT[state] || state;

  hint.textContent = message && state === "error" ? message : HINT_TEXT[state] || "";
  hint.classList.toggle("error", state === "error");
  hint.style.display = hint.textContent ? "" : "none";
  if (state === "idle" && captions.children.length > 0) {
    hint.style.display = "none";
  }

  running = state === "loading" || state === "listening" || state === "replaying";
  $("start").disabled = running;
  $("stop").disabled = !running;
  $("sample").disabled = running;
}

function clearCaptions() {
  captions.innerHTML = "";
  lastPartial = "";
  $("first").textContent = "—";
  $("endpoint").textContent = "—";
  $("rtf").textContent = "—";
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
  const stage = document.querySelector(".stage");
  stage.scrollTop = stage.scrollHeight;
}

let currentOnsetMs = 0;

function payloadOnsetMs() {
  return currentOnsetMs;
}

function renderPartial(text) {
  let node = captions.querySelector(".partial");
  if (!node) {
    node = document.createElement("p");
    node.className = "partial";
    captions.appendChild(node);
  }
  const stable = commonPrefix(lastPartial, text);
  const tail = text.slice(stable.length);
  node.innerHTML =
    `<span class="stable">${escapeHtml(stable)}</span>` +
    `<span class="tail">${escapeHtml(tail)}</span>`;
  lastPartial = text;
  hint.style.display = "none";
  if ($("first").textContent === "—") {
    $("first").textContent = `${payloadOnsetMs()} ms`;
  }
  scrollToBottom();
}

function commitSegment(text) {
  const node = captions.querySelector(".partial");
  if (node) node.remove();
  const p = document.createElement("p");
  p.className = "final";
  p.textContent = text;
  captions.appendChild(p);
  lastPartial = "";
  scrollToBottom();
}

function handle(event) {
  const payload = event.payload || {};
  switch (payload.kind) {
    case "state": {
      const state = payload.state;
      if (state === "loading") clearCaptions();
      setStatus(state);
      break;
    }
    case "device":
      $("device").textContent = `${payload.name} · ${(payload.sample_rate / 1000).toFixed(1)} kHz · ${payload.channels} ch`;
      break;
    case "level":
      $("level").style.width = `${Math.min(100, payload.rms * 380)}%`;
      break;
    case "partial":
      currentOnsetMs = payload.onset_ms || 0;
      renderPartial(payload.text);
      break;
    case "segment":
      commitSegment(payload.text);
      $("endpoint").textContent = `${payload.latency_ms} ms`;
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
      setStatus("error", payload.message);
      break;
  }
}

async function guard(promise) {
  try {
    await promise;
  } catch (err) {
    setStatus("error", String(err));
  }
}

if (!invoke) {
  hint.textContent = "Tauri API 未注入：请用 GabTwin 应用打开，而不是浏览器。";
} else {
  listen("session-event", handle).catch(() => {});

  $("start").addEventListener("click", () => {
    clearCaptions();
    running = true;
    $("start").disabled = true;
    $("stop").disabled = false;
    guard(invoke("start_listening"));
  });

  $("stop").addEventListener("click", () => {
    $("stop").disabled = true;
    guard(invoke("stop_listening"));
  });

  $("sample").addEventListener("click", () => {
    clearCaptions();
    running = true;
    $("start").disabled = true;
    $("stop").disabled = false;
    guard(invoke("replay_sample"));
  });

  invoke("app_info")
    .then((info) => {
      $("model").textContent = info.model_dir;
      $("model").title = info.model_dir;
    })
    .catch(() => {});

  setStatus("idle");
}
