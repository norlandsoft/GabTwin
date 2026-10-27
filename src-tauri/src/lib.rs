use chrono::Local;
use gabtwin_audio::{resample_linear, start_speaker};
use gabtwin_llm::{spoken_system_prompt, Dialog, LlmClient, LlmConfig};
use gabtwin_session::{
    default_asr_model_dir, default_offline_model_dir, gabtwin_home, input_devices, replay_wav,
    run_mic, DeviceInfo, OfflineAsrConfig, SessionConfig, SessionEvent,
};
use gabtwin_tts::{ClauseSplitter, GlmTts, RemoteTts, Tts, TtsConfig};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// 带时间戳的诊断日志。写到 stderr，启动脚本会把它落进 app.log。
fn log(message: impl AsRef<str>) {
    eprintln!("[{}] {}", Local::now().format("%H:%M:%S"), message.as_ref());
}

/// 被 kill / Ctrl+C 时留一行，避免下次再出现"它自己停了"的疑问。
fn install_signal_logging() {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let mut signals = match signal_hook::iterator::Signals::new([SIGINT, SIGTERM]) {
        Ok(signals) => signals,
        Err(err) => {
            log(format!("无法安装信号处理器：{err}"));
            return;
        }
    };
    std::thread::spawn(move || {
        for sig in signals.forever() {
            log(format!("收到终止信号 {sig}，退出"));
            std::process::exit(0);
        }
    });
}

enum TtsCommand {
    Clause { generation: u64, text: String },
    Stop,
    Flush,
}

struct TtsState {
    tx: Mutex<Option<mpsc::Sender<TtsCommand>>>,
    generation: Arc<AtomicU64>,
    splitter: Mutex<ClauseSplitter>,
    turn_started: Arc<Mutex<Option<Instant>>>,
    /// 播放期间置位，会话层据此屏蔽麦克风输入。
    mute: Arc<AtomicBool>,
    /// 输出是不是耳机（耳机回声小，可以边播边听、支持插话）。
    headphones: Arc<AtomicBool>,
    /// 当前是否正在播放，用于耳机下的插话检测。
    speaking: Arc<AtomicBool>,
}

impl TtsState {
    fn new(app: AppHandle) -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        let turn_started = Arc::new(Mutex::new(None));
        let mute = Arc::new(AtomicBool::new(false));
        let headphones = Arc::new(AtomicBool::new(false));
        let speaking = Arc::new(AtomicBool::new(false));
        let tx = start_tts_worker(
            app,
            generation.clone(),
            turn_started.clone(),
            mute.clone(),
            headphones.clone(),
            speaking.clone(),
        );
        Self {
            tx: Mutex::new(Some(tx)),
            generation,
            splitter: Mutex::new(ClauseSplitter::new()),
            turn_started,
            mute,
            headphones,
            speaking,
        }
    }

    fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(TtsCommand::Stop);
        }
    }

    fn begin_turn(&self) {
        self.stop();
        self.splitter.lock().unwrap().reset();
        *self.turn_started.lock().unwrap() = Some(Instant::now());
    }

    fn feed(&self, delta: &str) {
        let generation = self.generation.load(Ordering::SeqCst);
        let clauses = self.splitter.lock().unwrap().push(delta);
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            for text in clauses {
                let _ = tx.send(TtsCommand::Clause { generation, text });
            }
        }
    }

    fn flush(&self) {
        let generation = self.generation.load(Ordering::SeqCst);
        let last = self.splitter.lock().unwrap().flush();
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            if let Some(text) = last {
                let _ = tx.send(TtsCommand::Clause { generation, text });
            }
            let _ = tx.send(TtsCommand::Flush);
        }
    }
}

fn tts_model_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("GABTWIN_TTS_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    // 有参考音就优先用 ZipVoice 克隆音色，否则用 Matcha。
    let home = gabtwin_home();
    let zipvoice = home.join("models").join("zipvoice-zh-en");
    if zipvoice.is_dir() && home.join("tts/reference.wav").is_file() {
        zipvoice
    } else {
        home.join("models").join("matcha-zh-en")
    }
}

enum Synth {
    Local(Tts),
    Remote(RemoteTts),
    Glm(GlmTts),
}

impl Synth {
    fn sample_rate(&self) -> u32 {
        match self {
            Self::Local(tts) => tts.sample_rate(),
            Self::Remote(remote) => remote.sample_rate(),
            Self::Glm(glm) => glm.sample_rate(),
        }
    }

    fn synthesize(&self, text: &str) -> anyhow::Result<(Vec<f32>, u32)> {
        match self {
            Self::Local(tts) => Ok((tts.synthesize(text)?, tts.sample_rate())),
            Self::Remote(remote) => remote.synthesize(text),
            Self::Glm(glm) => glm.synthesize(text),
        }
    }
}

fn build_local_synth(app: &AppHandle) -> Option<Synth> {
    let dir = tts_model_dir();
    let mut tts_config = TtsConfig::new(dir.clone());
    tts_config.reference_wav = std::env::var_os("GABTWIN_TTS_REF_WAV")
        .map(PathBuf::from)
        .or_else(|| {
            let path = gabtwin_home().join("tts/reference.wav");
            path.is_file().then_some(path)
        });
    tts_config.reference_text = std::env::var("GABTWIN_TTS_REF_TEXT").ok().or_else(|| {
        std::fs::read_to_string(gabtwin_home().join("tts/reference.txt"))
            .ok()
            .map(|text| text.trim().to_string())
    });
    tts_config.num_steps = std::env::var("GABTWIN_TTS_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    match Tts::new(&tts_config) {
        Ok(tts) => Some(Synth::Local(tts)),
        Err(err) => {
            log(format!("本地 TTS 不可用：{err:#}"));
            let _ = app.emit(
                "tts-error",
                serde_json::json!({ "message": format!("本地 TTS 不可用：{err:#}") }),
            );
            None
        }
    }
}

fn start_tts_worker(
    app: AppHandle,
    generation: Arc<AtomicU64>,
    turn_started: Arc<Mutex<Option<Instant>>>,
    mute: Arc<AtomicBool>,
    headphones: Arc<AtomicBool>,
    speaking: Arc<AtomicBool>,
) -> mpsc::Sender<TtsCommand> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mode = std::env::var("GABTWIN_TTS").unwrap_or_else(|_| "glm".to_string());
        let mut synth = match mode.as_str() {
            "glm" => {
                let key = std::env::var("ZHIPU_API_KEY")
                    .ok()
                    .filter(|v| !v.is_empty());
                let voice = std::env::var("GABTWIN_GLM_VOICE").ok().or_else(|| {
                    let path = gabtwin_home().join("voices/glm/gabtwin.json");
                    let text = std::fs::read_to_string(path).ok()?;
                    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
                    value.get("voice")?.as_str().map(str::to_string)
                });
                match (key, voice) {
                    (Some(key), Some(voice)) => match GlmTts::new(key, voice) {
                        Ok(glm) => {
                            log("TTS：GLM 云端克隆音色");
                            Some(Synth::Glm(glm))
                        }
                        Err(err) => {
                            log(format!("GLM TTS 初始化失败：{err:#}"));
                            None
                        }
                    },
                    _ => {
                        log("TTS：缺少 ZHIPU_API_KEY 或音色 id，回退本地");
                        None
                    }
                }
            }
            "indextts" => std::env::var("GABTWIN_TTS_URL")
                .ok()
                .and_then(|url| RemoteTts::new(url).ok().map(Synth::Remote)),
            _ => None,
        };
        if synth.is_none() {
            synth = build_local_synth(&app);
        }
        let Some(synth) = synth else {
            return;
        };
        let warm = Instant::now();
        let _ = synth.synthesize("你好");
        log(format!(
            "TTS 就绪：{} Hz（预热 {} ms）",
            synth.sample_rate(),
            warm.elapsed().as_millis()
        ));
        let speaker = match start_speaker() {
            Ok(speaker) => speaker,
            Err(err) => {
                log(format!("音频输出不可用：{err:#}"));
                let _ = app.emit(
                    "tts-error",
                    serde_json::json!({ "message": format!("音频输出不可用：{err:#}") }),
                );
                return;
            }
        };
        headphones.store(speaker.headphones, Ordering::SeqCst);
        log(format!(
            "音频输出：{} Hz（TTS {} Hz，线性重采样），{}",
            speaker.sample_rate,
            synth.sample_rate(),
            if speaker.headphones {
                "耳机：可直接插话"
            } else {
                "外放：播放时半双工"
            }
        ));

        let mut started = false;
        let mut queued_ms: u64 = 0;
        let mut first_push_at: Option<Instant> = None;
        while let Ok(command) = rx.recv() {
            match command {
                TtsCommand::Stop => {
                    speaker.clear();
                    started = false;
                    queued_ms = 0;
                    first_push_at = None;
                    mute.store(false, Ordering::SeqCst);
                    speaking.store(false, Ordering::SeqCst);
                }
                TtsCommand::Flush => {
                    // 合成期间已经在播的部分要扣掉，否则声音结束后还会多静音一段。
                    let played = first_push_at
                        .map(|at| at.elapsed().as_millis() as u64)
                        .unwrap_or(0);
                    let remaining = queued_ms.saturating_sub(played);
                    log(format!("TTS 排队 {queued_ms} ms（剩余约 {remaining} ms）"));
                    let _ = app.emit(
                        "tts-queued",
                        serde_json::json!({ "duration_ms": remaining }),
                    );
                    let flag = mute.clone();
                    let playing = speaking.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(remaining + 250));
                        flag.store(false, Ordering::SeqCst);
                        playing.store(false, Ordering::SeqCst);
                        log("TTS 结束：恢复麦克风");
                    });
                    queued_ms = 0;
                    first_push_at = None;
                }
                TtsCommand::Clause {
                    generation: g,
                    text,
                } => {
                    if g != generation.load(Ordering::SeqCst) {
                        continue;
                    }
                    log(format!("TTS 句：{text}"));
                    let tick = Instant::now();
                    let (samples, tts_rate) = match synth.synthesize(&text) {
                        Ok(pair) => pair,
                        Err(err) => {
                            log(format!("TTS 合成失败：{err:#}"));
                            let _ = app.emit(
                                "tts-error",
                                serde_json::json!({ "message": format!("{err:#}") }),
                            );
                            continue;
                        }
                    };
                    if g != generation.load(Ordering::SeqCst) {
                        continue;
                    }
                    let ms = samples.len() as u64 * 1000 / tts_rate.max(1) as u64;
                    let audio = resample_linear(&samples, tts_rate, speaker.sample_rate);
                    queued_ms += ms;
                    let first = !started;
                    started = true;
                    if first_push_at.is_none() {
                        first_push_at = Some(Instant::now());
                    }
                    speaking.store(true, Ordering::SeqCst);
                    if !mute.swap(true, Ordering::SeqCst) {
                        log("TTS 播放：屏蔽麦克风");
                    }
                    speaker.push(audio);
                    if first {
                        let first_audio_ms = turn_started
                            .lock()
                            .ok()
                            .and_then(|value| *value)
                            .map(|at| at.elapsed().as_millis() as u64);
                        log(format!(
                            "TTS 首音 {:?} ms（首句合成 {} ms）",
                            first_audio_ms,
                            tick.elapsed().as_millis()
                        ));
                        let _ = app.emit(
                            "tts-start",
                            serde_json::json!({
                                "first_audio_ms": first_audio_ms,
                                "clause_ms": tick.elapsed().as_millis() as u64,
                            }),
                        );
                    }
                }
            }
        }
    });
    tx
}

struct LlmState {
    client: Option<LlmClient>,
    dialog: Mutex<Dialog>,
    turn: AtomicU64,
    cancel: Mutex<Arc<AtomicBool>>,
}

impl LlmState {
    fn new() -> Self {
        let client = match LlmConfig::from_env() {
            Some(cfg) => {
                let model = cfg.model.clone();
                match LlmClient::new(cfg) {
                    Ok(client) => {
                        log(format!("LLM 就绪：{model}"));
                        Some(client)
                    }
                    Err(err) => {
                        log(format!("LLM 初始化失败：{err:#}"));
                        None
                    }
                }
            }
            None => {
                log("未配置 DEEPSEEK_API_KEY，只显示识别结果");
                None
            }
        };
        let persona = "你是 GabTwin，一个温和、好奇、话不多的中文闲聊伙伴。";
        Self {
            client,
            dialog: Mutex::new(Dialog::new(spoken_system_prompt(persona))),
            turn: AtomicU64::new(0),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
        }
    }
}

#[derive(Default)]
struct SessionState {
    /// 当前会话的停止旗标。停止 = 置位，线程自己退出。
    stop: Mutex<Arc<AtomicBool>>,
    running: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Mic,
    Sample,
}

fn model_dir() -> PathBuf {
    std::env::var_os("GABTWIN_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_asr_model_dir)
}

fn offline_model_dir() -> PathBuf {
    std::env::var_os("GABTWIN_OFFLINE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_offline_model_dir)
}

fn sample_wav_path() -> PathBuf {
    if let Some(path) = std::env::var_os("GABTWIN_SAMPLE_WAV") {
        return PathBuf::from(path);
    }
    let dir = model_dir().join("test_wavs");
    for name in ["0.wav", "zh.wav"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    let shared = gabtwin_home().join("samples").join("0.wav");
    if shared.is_file() {
        return shared;
    }
    dir.join("0.wav")
}

/// 起一轮会话。会话线程自己持有 cpal 的 Stream（它在 macOS 上不是 Send）。
fn begin(app: &AppHandle, mode: Mode) -> Result<(), String> {
    let state = app.state::<SessionState>();
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("正在聆听，先停止再开始".into());
    }

    log(format!("开始会话（{mode:?}）"));
    let stop = Arc::new(AtomicBool::new(false));
    *state.stop.lock().unwrap() = stop.clone();

    let mut cfg = SessionConfig::new(model_dir());
    if let Some(tts) = app.try_state::<TtsState>() {
        // 耳机：不闭麦，靠插话检测；外放：播放期间半双工。
        if !tts.headphones.load(Ordering::SeqCst) {
            cfg.mute = Some(tts.mute.clone());
        }
    }
    if let Ok(device) = std::env::var("GABTWIN_DEVICE") {
        if !device.is_empty() {
            cfg.capture.device_name = Some(device);
        }
    }
    if let Ok(v) = std::env::var("GABTWIN_ENDPOINT") {
        if let Ok(v) = v.parse::<f32>() {
            cfg.asr.endpoint_silence_s = v;
        }
    }

    // 句末用 SenseVoice 定稿；GABTWIN_ENGINE=streaming 可关掉。
    let engine_enabled = std::env::var("GABTWIN_ENGINE")
        .map(|v| v != "streaming")
        .unwrap_or(true);
    if engine_enabled {
        let dir = offline_model_dir();
        if dir.is_dir() {
            cfg.offline = Some(OfflineAsrConfig::new(dir.clone()));
            log(format!("句末定稿：SenseVoice（{}）", dir.display()));
        } else {
            log("未找到 SenseVoice 定稿模型，句末退回流式文本");
        }
    }

    let app_handle = app.clone();
    let running = state.running.clone();
    std::thread::spawn(move || {
        let emit_target = app_handle.clone();
        let llm_app = app_handle.clone();
        let mut sink = move |event: SessionEvent| {
            if matches!(event, SessionEvent::Level { .. }) {
                let _ = emit_target.emit("session-event", &event);
                return;
            }
            log(format!("{event:?}"));
            if let Err(err) = emit_target.emit("session-event", &event) {
                log(format!("emit 失败：{err}"));
            }
            if let SessionEvent::Partial { .. } = &event {
                if let Some(tts) = llm_app.try_state::<TtsState>() {
                    if tts.speaking.load(Ordering::SeqCst) && tts.headphones.load(Ordering::SeqCst)
                    {
                        tts.stop();
                        if let Some(llm) = llm_app.try_state::<LlmState>() {
                            llm.cancel.lock().unwrap().store(true, Ordering::SeqCst);
                        }
                        log("检测到插话：停止播放并取消回复");
                    }
                }
            }
            if let SessionEvent::Segment { text, .. } = &event {
                // 只有标点/空白的结果不要触发一轮对话。
                if text.chars().any(|c| c.is_alphanumeric()) {
                    start_llm_turn(&llm_app, text.clone());
                }
            }
        };

        let result = match mode {
            Mode::Mic => run_mic(&cfg, stop.clone(), &mut sink),
            Mode::Sample => replay_wav(&sample_wav_path(), &cfg, true, stop.clone(), &mut sink),
        };

        if let Err(err) = result {
            let _ = app_handle.emit(
                "session-event",
                SessionEvent::Error {
                    message: format!("{err:#}"),
                },
            );
        }
        running.store(false, Ordering::SeqCst);
        log("会话结束");
        let _ = app_handle.emit(
            "session-event",
            SessionEvent::State {
                state: "idle".into(),
            },
        );
    });

    Ok(())
}

/// 一句用户话定稿后，发起一轮流式回复。新一轮会取消上一轮。
fn start_llm_turn(app: &AppHandle, text: String) {
    if let Some(tts) = app.try_state::<TtsState>() {
        tts.begin_turn();
    }
    let state = app.state::<LlmState>();
    let Some(client) = state.client.clone() else {
        let _ = app.emit(
            "llm-error",
            serde_json::json!({
                "turn_id": 0,
                "message": "未配置 DEEPSEEK_API_KEY（或 GABTWIN_LLM_KEY），只显示识别结果",
            }),
        );
        return;
    };

    let cancel = Arc::new(AtomicBool::new(false));
    *state.cancel.lock().unwrap() = cancel.clone();
    let turn_id = state.turn.fetch_add(1, Ordering::SeqCst) + 1;

    let messages = {
        let mut dialog = state.dialog.lock().unwrap();
        dialog.push_user(&text);
        dialog.messages()
    };

    let _ = app.emit("llm-thinking", serde_json::json!({ "turn_id": turn_id }));
    let app_handle = app.clone();
    std::thread::spawn(move || {
        let emit_target = app_handle.clone();
        let result = client.stream_chat(&messages, &cancel, |delta| {
            let _ = emit_target.emit(
                "llm-delta",
                serde_json::json!({ "turn_id": turn_id, "text": delta }),
            );
            if let Some(tts) = emit_target.try_state::<TtsState>() {
                tts.feed(delta);
            }
        });

        match result {
            Ok(r) if r.cancelled => {
                if let Some(tts) = app_handle.try_state::<TtsState>() {
                    tts.stop();
                }
                let _ = app_handle.emit("llm-cancelled", serde_json::json!({ "turn_id": turn_id }));
            }
            Ok(r) if !r.text.trim().is_empty() => {
                if let Some(state) = app_handle.try_state::<LlmState>() {
                    state.dialog.lock().unwrap().push_assistant(r.text.clone());
                }
                if let Some(tts) = app_handle.try_state::<TtsState>() {
                    tts.flush();
                }
                let _ = app_handle.emit(
                    "llm-done",
                    serde_json::json!({
                        "turn_id": turn_id,
                        "text": r.text,
                        "first_token_ms": r.first_token_ms,
                        "total_ms": r.total_ms,
                    }),
                );
            }
            Ok(_) => {
                let _ = app_handle.emit(
                    "llm-error",
                    serde_json::json!({ "turn_id": turn_id, "message": "模型没有返回内容" }),
                );
            }
            Err(err) => {
                if let Some(tts) = app_handle.try_state::<TtsState>() {
                    tts.stop();
                }
                log(format!("LLM 失败：{err:#}"));
                let _ = app_handle.emit(
                    "llm-error",
                    serde_json::json!({ "turn_id": turn_id, "message": format!("{err:#}") }),
                );
            }
        }
    });
}

#[tauri::command]
fn llm_status(app: AppHandle) -> serde_json::Value {
    let state = app.state::<LlmState>();
    serde_json::json!({
        "configured": state.client.is_some(),
        "model": state.client.as_ref().map(|c| c.model().to_string()),
        "turns": state.dialog.lock().unwrap().len() / 2,
    })
}

#[tauri::command]
fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    input_devices().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn start_listening(app: AppHandle) -> Result<(), String> {
    begin(&app, Mode::Mic)
}

#[tauri::command]
fn stop_listening(app: AppHandle) {
    if let Some(state) = app.try_state::<SessionState>() {
        state.stop.lock().unwrap().store(true, Ordering::SeqCst);
    }
    if let Some(llm) = app.try_state::<LlmState>() {
        llm.cancel.lock().unwrap().store(true, Ordering::SeqCst);
    }
    if let Some(tts) = app.try_state::<TtsState>() {
        tts.stop();
    }
}

#[tauri::command]
fn replay_sample(app: AppHandle) -> Result<(), String> {
    begin(&app, Mode::Sample)
}

#[tauri::command]
fn app_info() -> serde_json::Value {
    serde_json::json!({
        "home": gabtwin_home().display().to_string(),
        "model_dir": model_dir().display().to_string(),
        "sample_wav": sample_wav_path().display().to_string(),
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// 先读 .env，再读用户目录下的 .env；已有环境变量优先（不会被覆盖）。
fn load_env() {
    let _ = dotenvy::dotenv();
    let user_env = gabtwin_home().join(".env");
    if user_env.is_file() {
        let _ = dotenvy::from_path(&user_env);
    }
}

pub fn run() {
    load_env();
    install_signal_logging();
    log(format!("GabTwin 启动 pid={}", std::process::id()));

    tauri::Builder::default()
        .manage(SessionState::default())
        .manage(LlmState::new())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            start_listening,
            stop_listening,
            replay_sample,
            app_info,
            llm_status
        ])
        .setup(|app| {
            app.manage(TtsState::new(app.handle().clone()));

            // 验证用：不点按钮也能自动回放示例音频（截图 / 回归用）
            if std::env::var_os("GABTWIN_AUTOSTART").is_some() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    if let Err(err) = begin(&handle, Mode::Mic) {
                        log(format!("自动聆听失败：{err}"));
                    }
                });
            }
            if std::env::var_os("GABTWIN_AUTOREPLAY").is_some() {
                log("GABTWIN_AUTOREPLAY 已启用");
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    if let Err(err) = begin(&handle, Mode::Sample) {
                        log(format!("自动回放失败：{err}"));
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("GabTwin 启动失败");
}
