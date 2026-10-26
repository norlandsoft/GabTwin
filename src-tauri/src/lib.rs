use chrono::Local;
use gabtwin_llm::{spoken_system_prompt, Dialog, LlmClient, LlmConfig};
use gabtwin_session::{
    default_asr_model_dir, default_offline_model_dir, gabtwin_home, input_devices, replay_wav,
    run_mic, DeviceInfo, OfflineAsrConfig, SessionConfig, SessionEvent,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
            if let SessionEvent::Segment { text, .. } = &event {
                start_llm_turn(&llm_app, text.clone());
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
        });

        match result {
            Ok(r) if r.cancelled => {
                let _ = app_handle.emit("llm-cancelled", serde_json::json!({ "turn_id": turn_id }));
            }
            Ok(r) if !r.text.trim().is_empty() => {
                if let Some(state) = app_handle.try_state::<LlmState>() {
                    state.dialog.lock().unwrap().push_assistant(r.text.clone());
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
