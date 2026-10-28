use chrono::Local;
use gabtwin_audio::{resample_linear, start_speaker_with_render, RenderTap};
use gabtwin_llm::{system_prompt_for, Dialog, LlmClient, LlmConfig};
use gabtwin_session::{
    default_asr_model_dir, default_offline_model_dir, gabtwin_home, input_devices, models_root,
    replay_wav, run_mic, DeviceInfo, OfflineAsrConfig, SessionConfig, SessionEvent,
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
    Flush { generation: u64 },
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
    /// 当前 TTS 引擎（窗口里显示）。
    label: Arc<Mutex<(String, String)>>,
    /// 最近一轮 TTS 的全文 + 结束时刻，用于过滤残余回声。
    last_reply: Arc<Mutex<(String, Option<Instant>)>>,
    /// 扬声器实际播出的参考信号，供 AEC3 使用。
    render: RenderTap,
    /// 最近一次麦克风 RMS，用于插话门限。
    level: Arc<Mutex<f32>>,
    /// Push-to-talk：按住空格才开麦。
    ptt: Arc<AtomicBool>,
}

impl TtsState {
    fn new(app: AppHandle) -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        let turn_started = Arc::new(Mutex::new(None));
        let mute = Arc::new(AtomicBool::new(false));
        let headphones = Arc::new(AtomicBool::new(false));
        let speaking = Arc::new(AtomicBool::new(false));
        let label = Arc::new(Mutex::new(("—".to_string(), String::new())));
        let last_reply = Arc::new(Mutex::new((String::new(), None)));
        let render = RenderTap::new();
        let level = Arc::new(Mutex::new(0.0));
        let ptt = Arc::new(AtomicBool::new(false));
        let tx = start_tts_worker(
            app,
            generation.clone(),
            turn_started.clone(),
            mute.clone(),
            headphones.clone(),
            speaking.clone(),
            label.clone(),
            last_reply.clone(),
            render.clone(),
        );
        Self {
            tx: Mutex::new(Some(tx)),
            generation,
            splitter: Mutex::new(ClauseSplitter::with_first_cap(
                std::env::var("GABTWIN_TTS_FIRST_CAP")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(20),
            )),
            turn_started,
            mute,
            headphones,
            speaking,
            label,
            last_reply,
            render,
            level,
            ptt,
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

    /// 按 LLM 的最终完整结果一次性送 TTS（默认路径）。
    fn speak(&self, text: &str) {
        let generation = self.generation.load(Ordering::SeqCst);
        self.splitter.lock().unwrap().reset();
        let clauses = self.splitter.lock().unwrap().push(text);
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            for clause in clauses {
                let _ = tx.send(TtsCommand::Clause {
                    generation,
                    text: clause,
                });
            }
            let last = self.splitter.lock().unwrap().flush();
            if let Some(text) = last {
                let _ = tx.send(TtsCommand::Clause { generation, text });
            }
            let _ = tx.send(TtsCommand::Flush { generation });
        }
    }

    fn flush(&self) {
        let generation = self.generation.load(Ordering::SeqCst);
        let last = self.splitter.lock().unwrap().flush();
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            if let Some(text) = last {
                let _ = tx.send(TtsCommand::Clause { generation, text });
            }
            let _ = tx.send(TtsCommand::Flush { generation });
        }
    }
}

fn tts_model_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("GABTWIN_TTS_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    // 有参考音就优先用 ZipVoice 克隆音色，否则用 Matcha。
    let home = gabtwin_home();
    let root = models_root();
    let zipvoice = root.join("tts").join("zipvoice-zh-en");
    if zipvoice.is_dir() && home.join("tts/reference.wav").is_file() {
        zipvoice
    } else {
        root.join("tts").join("matcha-zh-en")
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

    fn stream<F>(&self, text: &str, on_chunk: &mut F) -> anyhow::Result<()>
    where
        F: FnMut(&[f32], u32),
    {
        if let Self::Remote(remote) = self {
            if std::env::var("GABTWIN_TTS_STREAM")
                .map(|value| value != "0")
                .unwrap_or(true)
            {
                match remote.stream_pcm(text, on_chunk) {
                    Ok(()) => return Ok(()),
                    Err(err) => log(format!("TTS 流式失败，回退整句：{err:#}")),
                }
            }
            let (samples, rate) = remote.synthesize(text)?;
            on_chunk(&samples, rate);
            return Ok(());
        }
        let (samples, rate) = self.synthesize(text)?;
        on_chunk(&samples, rate);
        Ok(())
    }

    fn label(&self) -> (String, String) {
        match self {
            Self::Local(tts) => (
                tts.engine_name().to_string(),
                tts_model_dir().display().to_string(),
            ),
            Self::Remote(_) => {
                let is_breeze = std::env::var("GABTWIN_TTS")
                    .map(|value| value == "breeze")
                    .unwrap_or(true);
                (
                    if is_breeze {
                        "Breeze（本地）"
                    } else {
                        "IndexTTS（本地）"
                    }
                    .to_string(),
                    std::env::var("GABTWIN_TTS_URL")
                        .unwrap_or_else(|_| "127.0.0.1:8799".to_string()),
                )
            }
            Self::Glm(_) => ("GLM 云端克隆".to_string(), "bigmodel.cn".to_string()),
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
    label: Arc<Mutex<(String, String)>>,
    last_reply: Arc<Mutex<(String, Option<Instant>)>>,
    render: RenderTap,
) -> mpsc::Sender<TtsCommand> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mode = std::env::var("GABTWIN_TTS").unwrap_or_else(|_| "breeze".to_string());
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
            "indextts" | "breeze" => std::env::var("GABTWIN_TTS_URL")
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
        let (engine, detail) = synth.label();
        *label.lock().unwrap() = (engine.clone(), detail.clone());
        let _ = app.emit(
            "tts-engine",
            serde_json::json!({ "engine": engine, "detail": detail }),
        );
        log(format!("TTS 引擎：{engine}（{detail}）"));
        let warm = Instant::now();
        let _ = synth.synthesize("你好");
        log(format!(
            "TTS 就绪：{} Hz（预热 {} ms）",
            synth.sample_rate(),
            warm.elapsed().as_millis()
        ));
        let speaker = match start_speaker_with_render(render) {
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
        let barge_in = std::env::var("GABTWIN_BARGE_IN")
            .map(|value| value == "1")
            .unwrap_or(false);
        log(format!(
            "音频输出：{} Hz（TTS {} Hz，线性重采样），{}",
            speaker.sample_rate,
            synth.sample_rate(),
            if speaker.headphones && barge_in {
                "耳机：可直接插话（GABTWIN_BARGE_IN=1）"
            } else if speaker.headphones {
                "耳机：播放时闭麦（默认，GABTWIN_BARGE_IN=1 可插话）"
            } else {
                "外放：播放时闭麦（半双工）"
            }
        ));

        let save_tts = std::env::var("GABTWIN_TTS_SAVE").unwrap_or_else(|_| "1".to_string()) != "0";
        let engine = engine.clone();
        let mut tts_seq: u64 = 0;
        let mut reply_text = String::new();
        let mut started = false;
        let mut queued_ms: u64 = 0;
        let mut first_push_at: Option<Instant> = None;
        while let Ok(command) = rx.recv() {
            match command {
                TtsCommand::Stop => {
                    *last_reply.lock().unwrap() = (reply_text.clone(), Some(Instant::now()));
                    reply_text.clear();
                    speaker.clear();
                    started = false;
                    queued_ms = 0;
                    first_push_at = None;
                    mute.store(false, Ordering::SeqCst);
                    speaking.store(false, Ordering::SeqCst);
                }
                TtsCommand::Flush { generation: g } => {
                    if g != generation.load(Ordering::SeqCst) {
                        continue;
                    }
                    *last_reply.lock().unwrap() = (reply_text.clone(), Some(Instant::now()));
                    reply_text.clear();
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
                        std::thread::sleep(Duration::from_millis(remaining + 800));
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
                    if !started {
                        reply_text.clear();
                    }
                    reply_text.push_str(&text);
                    *last_reply.lock().unwrap() = (reply_text.clone(), Some(Instant::now()));
                    let first_clause = !started;
                    let mut first_chunk = true;
                    let mut chunk_rate = 0u32;
                    let mut samples_all: Vec<f32> = Vec::new();
                    let result = synth.stream(&text, &mut |samples, rate| {
                        if g != generation.load(Ordering::SeqCst) {
                            return;
                        }
                        chunk_rate = rate;
                        if save_tts {
                            samples_all.extend_from_slice(samples);
                        }
                        let ms = samples.len() as u64 * 1000 / rate.max(1) as u64;
                        let audio = resample_linear(samples, rate, speaker.sample_rate);
                        queued_ms += ms;
                        if first_push_at.is_none() {
                            first_push_at = Some(Instant::now());
                        }
                        speaking.store(true, Ordering::SeqCst);
                        if !mute.swap(true, Ordering::SeqCst) {
                            log("TTS 播放：屏蔽麦克风");
                        }
                        speaker.push(audio);
                        if first_chunk {
                            first_chunk = false;
                            started = true;
                            if first_clause {
                                let first_audio_ms = turn_started
                                    .lock()
                                    .ok()
                                    .and_then(|value| *value)
                                    .map(|at| at.elapsed().as_millis() as u64);
                                log(format!(
                                    "TTS 首音 {:?} ms（首句首块 {} ms）",
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
                    });
                    if let Err(err) = result {
                        log(format!("TTS 合成失败：{err:#}"));
                        let _ = app.emit(
                            "tts-error",
                            serde_json::json!({ "message": format!("{err:#}") }),
                        );
                        continue;
                    }
                    if g != generation.load(Ordering::SeqCst) {
                        continue;
                    }
                    if save_tts && !samples_all.is_empty() && chunk_rate > 0 {
                        tts_seq += 1;
                        let dir = gabtwin_home()
                            .join("recordings")
                            .join(chrono::Local::now().format("%Y-%m-%d").to_string());
                        let _ = std::fs::create_dir_all(&dir);
                        let stamp = chrono::Local::now().format("%H%M%S").to_string();
                        let path = dir.join(format!("{stamp}-{tts_seq:03}.wav"));
                        match gabtwin_tts::save_wav(&path, &samples_all, chunk_rate) {
                            Ok(()) => {
                                let record = serde_json::json!({
                                    "ts": chrono::Local::now().to_rfc3339(),
                                    "engine": engine,
                                    "text": text,
                                    "wav": path.display().to_string(),
                                    "seconds": samples_all.len() as f64 / chunk_rate as f64,
                                });
                                if let Ok(mut fh) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(dir.join("metadata.jsonl"))
                                {
                                    use std::io::Write;
                                    let _ = writeln!(fh, "{record}");
                                }
                            }
                            Err(err) => log(format!("保存 TTS 音频失败：{err:#}")),
                        }
                    }
                }
            }
        }
    });
    tx
}

/// 低能量噪声/短英文填充词导致的 ASR 幻觉。
fn is_hallucination(text: &str) -> bool {
    let normalized: String = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    if normalized.is_empty() {
        return true;
    }
    let has_cjk = text
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    if !has_cjk {
        const FILLERS: &[&str] = &[
            "the", "i", "yeah", "what", "you", "thankyou", "hmm", "mm", "ah", "oh", "okay",
            "ok", "yes", "no",
        ];
        if FILLERS.contains(&normalized.as_str()) {
            return true;
        }
        if normalized.chars().count() <= 2 {
            return true;
        }
    } else if normalized.chars().count() <= 1 {
        return true;
    }
    false
}

/// 判断识别结果是否只是刚播完的 TTS 残余回声（字符 bigram Jaccard）。
fn is_tts_echo(text: &str, state: &TtsState) -> bool {
    let (last, at) = state.last_reply.lock().unwrap().clone();
    let Some(at) = at else { return false };
    if at.elapsed() > Duration::from_secs(5) {
        return false;
    }
    let norm = |value: &str| -> String {
        value
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let said = norm(text);
    let last = norm(&last);
    if said.chars().count() < 4 || last.chars().count() < 4 {
        return !said.is_empty() && last.contains(&said);
    }
    let grams = |value: &String| -> std::collections::HashSet<(char, char)> {
        value.chars().collect::<Vec<_>>().windows(2).map(|w| (w[0], w[1])).collect()
    };
    let a = grams(&said);
    let b = grams(&last);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let inter = a.intersection(&b).count() as f32;
    let union = a.union(&b).count() as f32;
    inter / union > 0.45
}

struct LlmState {
    client: Option<LlmClient>,
    system: String,
    dialog: Mutex<Dialog>,
    turn: AtomicU64,
    cancel: Mutex<Arc<AtomicBool>>,
    /// 小停顿被 ASR 切成多段时，先把文本攒在这里再合并成一轮。
    pending: Mutex<Option<String>>,
    segment_seq: AtomicU64,
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
        let model_hint = std::env::var("GABTWIN_LLM_MODEL").unwrap_or_default();
        let system = system_prompt_for(persona, &model_hint);
        if let Some(client) = client.clone() {
            let system = system.clone();
            std::thread::spawn(move || {
                let _ = client.prewarm(&system);
            });
        }
        Self {
            client,
            system: system.clone(),
            dialog: Mutex::new(Dialog::new(system)),
            turn: AtomicU64::new(0),
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            pending: Mutex::new(None),
            segment_seq: AtomicU64::new(0),
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
    let aec_enabled = std::env::var("GABTWIN_AEC")
        .map(|value| value != "0")
        .unwrap_or(true);
    let ptt_enabled = std::env::var("GABTWIN_PTT")
        .map(|value| value != "0")
        .unwrap_or(true);
    if let Some(tts) = app.try_state::<TtsState>() {
        if ptt_enabled {
            cfg.ptt = Some(tts.ptt.clone());
            log("麦克风策略：Push-to-Talk（按住空格说话）");
        }
        if aec_enabled {
            tts.render.clear();
            cfg.aec_render = Some(tts.render.clone());
        }
        if !ptt_enabled && !aec_enabled {
            let barge_in = std::env::var("GABTWIN_BARGE_IN")
                .map(|value| value == "1")
                .unwrap_or(false);
            let allow_barge = barge_in && tts.headphones.load(Ordering::SeqCst);
            if !allow_barge {
                cfg.mute = Some(tts.mute.clone());
            }
            log(format!(
                "麦克风策略：{}",
                if allow_barge {
                    "耳机 + 插话（GABTWIN_BARGE_IN=1）"
                } else {
                    "TTS 播放时闭麦（GABTWIN_AEC=0）"
                }
            ));
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
            if let SessionEvent::Level { rms, .. } = &event {
                if let Some(tts) = llm_app.try_state::<TtsState>() {
                    *tts.level.lock().unwrap() = *rms;
                }
                let _ = emit_target.emit("session-event", &event);
                return;
            }
            log(format!("{event:?}"));
            if let Err(err) = emit_target.emit("session-event", &event) {
                log(format!("emit 失败：{err}"));
            }
            if let SessionEvent::Partial { text, .. } = &event {
                if let Some(tts) = llm_app.try_state::<TtsState>() {
                    let barge_in = std::env::var("GABTWIN_BARGE_IN")
                        .map(|value| value == "1")
                        .unwrap_or(false);
                    let barge_rms = std::env::var("GABTWIN_BARGE_RMS")
                        .ok()
                        .and_then(|value| value.parse::<f32>().ok())
                        .unwrap_or(0.03);
                    let level = *tts.level.lock().unwrap();
                    if (aec_enabled || (barge_in && tts.headphones.load(Ordering::SeqCst)))
                        && tts.speaking.load(Ordering::SeqCst)
                        && !is_hallucination(text)
                        && !is_tts_echo(text, &tts)
                        && level >= barge_rms
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
                if !text.chars().any(|c| c.is_alphanumeric()) {
                    return;
                }
                if is_hallucination(text) {
                    log(format!("忽略短/噪声幻觉：{text}"));
                    return;
                }
                if let Some(tts) = llm_app.try_state::<TtsState>() {
                    if is_tts_echo(text, &tts) {
                        log("忽略疑似 TTS 回声，不进入 LLM");
                        return;
                    }
                }
                queue_segment(&llm_app, text.clone());
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
fn queue_segment(app: &AppHandle, text: String) {
    let Some(state) = app.try_state::<LlmState>() else {
        start_llm_turn(app, text);
        return;
    };
    let seq = state.segment_seq.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut pending = state.pending.lock().unwrap();
        match pending.as_mut() {
            Some(buffer) => buffer.push_str(&text),
            None => *pending = Some(text),
        }
    }
    let delay_ms = std::env::var("GABTWIN_SEGMENT_MERGE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let Some(state) = app.try_state::<LlmState>() else {
            return;
        };
        if state.segment_seq.load(Ordering::SeqCst) != seq {
            return;
        }
        let text = state.pending.lock().unwrap().take();
        if let Some(text) = text {
            if !text.trim().is_empty() {
                start_llm_turn(&app, text);
            }
        }
    });
}

fn start_llm_turn(app: &AppHandle, text: String) {
    let final_tts = std::env::var("GABTWIN_TTS_FINAL")
        .map(|value| value != "0")
        .unwrap_or(true);
    // 新一轮开始：先取消上一轮，否则旧回复的 delta 会继续喂给 TTS，
    // 两条回答在 splitter 里交错，听起来就语无伦次。
    if let Some(previous) = app.try_state::<LlmState>() {
        previous.cancel.lock().unwrap().store(true, Ordering::SeqCst);
    }
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
        let cancel_for_feed = cancel.clone();
        let result = client.stream_chat(&messages, &cancel, |delta| {
            let _ = emit_target.emit(
                "llm-delta",
                serde_json::json!({ "turn_id": turn_id, "text": delta }),
            );
            if !final_tts && !cancel_for_feed.load(Ordering::SeqCst) {
                if let Some(tts) = emit_target.try_state::<TtsState>() {
                    tts.feed(delta);
                }
            }
        });

        // 如果这轮已经被新的一轮取代，结果直接丢弃，不再写对话/喂 TTS。
        let current_turn = app_handle
            .try_state::<LlmState>()
            .map(|state| state.turn.load(Ordering::SeqCst))
            .unwrap_or(0);
        if current_turn != turn_id {
            return;
        }

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
                    if final_tts {
                        tts.speak(&r.text);
                    } else {
                        tts.flush();
                    }
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
fn ptt_press(app: AppHandle) {
    if let Some(tts) = app.try_state::<TtsState>() {
        tts.ptt.store(true, Ordering::SeqCst);
        tts.stop();
        if let Some(llm) = app.try_state::<LlmState>() {
            llm.cancel.lock().unwrap().store(true, Ordering::SeqCst);
            llm.segment_seq.fetch_add(1, Ordering::SeqCst);
            *llm.pending.lock().unwrap() = None;
        }
        log("PTT 按下：开麦并打断");
    }
}

#[tauri::command]
fn ptt_release(app: AppHandle) {
    if let Some(tts) = app.try_state::<TtsState>() {
        tts.ptt.store(false, Ordering::SeqCst);
        log("PTT 松开");
    }
}

#[tauri::command]
fn clear_memory(app: AppHandle) {
    if let Some(llm) = app.try_state::<LlmState>() {
        llm.cancel.lock().unwrap().store(true, Ordering::SeqCst);
        llm.segment_seq.fetch_add(1, Ordering::SeqCst);
        *llm.pending.lock().unwrap() = None;
        *llm.dialog.lock().unwrap() = Dialog::new(llm.system.clone());
    }
    if let Some(tts) = app.try_state::<TtsState>() {
        tts.stop();
    }
    log("历史记忆已清空");
}

#[tauri::command]
fn interrupt(app: AppHandle) {
    if let Some(tts) = app.try_state::<TtsState>() {
        tts.stop();
    }
    if let Some(llm) = app.try_state::<LlmState>() {
        llm.cancel.lock().unwrap().store(true, Ordering::SeqCst);
        llm.segment_seq.fetch_add(1, Ordering::SeqCst);
        *llm.pending.lock().unwrap() = None;
    }
    log("手动打断：停止 TTS 并取消回复");
}

#[tauri::command]
fn tts_info(state: tauri::State<TtsState>) -> serde_json::Value {
    let (engine, detail) = state.label.lock().unwrap().clone();
    serde_json::json!({ "engine": engine, "detail": detail })
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
            tts_info,
            interrupt,
            clear_memory,
            ptt_press,
            ptt_release,
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
