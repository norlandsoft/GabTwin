use chrono::Local;
use gabtwin_session::{
    default_asr_model_dir, default_offline_model_dir, gabtwin_home, input_devices, replay_wav,
    run_mic, DeviceInfo, OfflineAsrConfig, SessionConfig, SessionEvent,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
        let mut sink = move |event: SessionEvent| {
            if matches!(event, SessionEvent::Level { .. }) {
                let _ = emit_target.emit("session-event", &event);
                return;
            }
            log(format!("{event:?}"));
            if let Err(err) = emit_target.emit("session-event", &event) {
                log(format!("emit 失败：{err}"));
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
pub fn run() {
    install_signal_logging();
    log(format!("GabTwin 启动 pid={}", std::process::id()));

    tauri::Builder::default()
        .manage(SessionState::default())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            start_listening,
            stop_listening,
            replay_sample,
            app_info
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
