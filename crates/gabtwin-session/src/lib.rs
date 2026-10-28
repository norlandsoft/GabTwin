//! 会话层：把「采集」与「识别」接成一条回路，对外只发事件。
//!
//! 这一层不依赖 Tauri、不依赖 UI，所以可以用 CLI（gabtwin-probe）单独验证，
//! 也可以被 Tauri 直接复用。事件协议见 docs/01-architecture.md。

use anyhow::{bail, Context, Result};
use gabtwin_asr::{Asr, AsrConfig, AsrUpdate};
use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear::{self, LinearPipeline};
use gabtwin_audio::{downmix, recv_chunk, rms, Capture, CaptureConfig};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use gabtwin_asr::{OfflineAsr, OfflineAsrConfig};
pub use gabtwin_audio::{input_devices, DeviceInfo};

pub const MODEL_DIR_NAME: &str = "streaming-paraformer-bilingual-zh-en";
/// 句末定稿用的离线模型（SenseVoice）。
pub const OFFLINE_MODEL_DIR_NAME: &str = "sense-voice-zh-en-ja-ko-yue-2024-07-17";

/// GabTwin 的根目录。`GABTWIN_HOME` 可覆盖（测试、离线部署）。
pub fn gabtwin_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("GABTWIN_HOME") {
        return PathBuf::from(dir);
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workspace");
    if repo.is_dir() {
        return repo;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Library")
        .join("Application Support")
        .join("GabTwin")
}

/// 模型根目录：GABTWIN_MODELS_DIR > 仓库 models/ > $GABTWIN_HOME/models。
pub fn models_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("GABTWIN_MODELS_DIR") {
        return PathBuf::from(dir);
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models");
    if repo.is_dir() {
        return repo;
    }
    gabtwin_home().join("models")
}

pub fn default_asr_model_dir() -> PathBuf {
    models_root().join("asr").join(MODEL_DIR_NAME)
}

pub fn default_offline_model_dir() -> PathBuf {
    models_root().join("asr").join(OFFLINE_MODEL_DIR_NAME)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    /// loading / listening / replaying / idle
    State {
        state: String,
    },
    Device {
        name: String,
        sample_rate: u32,
        channels: u16,
    },
    Level {
        rms: f32,
    },
    /// 麦克风是否被 TTS 播放屏蔽（诊断用，UI 可忽略）。
    Muted {
        muted: bool,
    },
    /// 当前句子的完整假设，会不断被改写。
    /// onset_ms = 从起始点算起的墙钟；audio_ms = 这一刻已经喂进去的音频位置。
    Partial {
        text: String,
        onset_ms: u64,
        audio_ms: u64,
    },
    /// 端点确认，这句话定稿。latency_ms = 最后一个有声帧到定稿之间的音频时长。
    Segment {
        text: String,
        onset_ms: u64,
        latency_ms: u64,
        audio_ms: u64,
    },
    Metrics {
        /// 起始点 → 第一段字幕的墙钟时间（麦克风 / 实时回放有意义）。
        first_partial_ms: Option<u64>,
        /// 起始点 → 第一段字幕之间"吃进去的音频"长度，与回放速度无关。
        first_partial_audio_ms: Option<u64>,
        rtf: f32,
        audio_ms: u64,
        wall_ms: u64,
    },
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub capture: CaptureConfig,
    pub asr: AsrConfig,
    /// 每次送进识别器的音频块长度（毫秒）。
    pub chunk_ms: u32,
    /// 起始点判定阈值（RMS）。低于它视为静音。
    pub onset_rms: f32,
    /// 会话开始时先喂多少毫秒静音给识别器做预热。
    /// 流式模型没有这段热身时，用户开口的第一个词容易被吃掉。
    pub warmup_ms: u32,
    /// 句末定稿用的离线识别器（可选）。设了就用它替换流式的句末文本。
    pub offline: Option<OfflineAsrConfig>,
    /// TTS 播放期间置位：不喂识别器，避免把自己的声音当成用户说话。
    pub mute: Option<Arc<AtomicBool>>,
    /// 播放端参考信号；Some 时启用 AEC3，麦克风可以边播边听。
    pub aec_render: Option<gabtwin_audio::RenderTap>,
    /// 一段话至少要有这么多 RMS，否则不送定稿（挡住噪声幻觉）。
    pub min_segment_rms: f32,
    /// 一段话至少这么长（毫秒）。
    pub min_segment_ms: u32,
    /// Push-to-talk：只有它为 true 时才把麦克风送进识别器。
    pub ptt: Option<Arc<AtomicBool>>,
}

impl SessionConfig {
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            capture: CaptureConfig::default(),
            asr: AsrConfig::new(model_dir),
            chunk_ms: 50,
            onset_rms: 0.01,
            warmup_ms: 0,
            offline: None,
            mute: None,
            aec_render: None,
            min_segment_rms: 0.008,
            min_segment_ms: 250,
            ptt: None,
        }
    }
}

struct Stats {
    sample_rate: u32,
    audio_samples: u64,
    started: Instant,
    /// 当前这句话的起点（第一个有声帧）。
    speech_start: Option<Instant>,
    /// 起点对应的音频位置（毫秒）。
    onset_audio_ms: Option<u64>,
    /// 最后一个有声帧之后累计的静音时长（毫秒）。端点延迟按它算，
    /// 这样实时麦克风、实时回放、非实时回放是同一个口径。
    silence_ms: u64,
    first_partial_ms: Option<u64>,
    first_partial_audio_ms: Option<u64>,
    /// 真正花在识别上的时间（不含实时回放的等待、不含麦克风等待）。
    proc_time: Duration,
}

impl Stats {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            audio_samples: 0,
            started: Instant::now(),
            speech_start: None,
            onset_audio_ms: None,
            silence_ms: 0,
            first_partial_ms: None,
            first_partial_audio_ms: None,
            proc_time: Duration::ZERO,
        }
    }

    fn on_audio(&mut self, mono: &[f32], level: f32, onset_rms: f32) {
        self.audio_samples += mono.len() as u64;
        if level > onset_rms {
            if self.speech_start.is_none() {
                self.speech_start = Some(Instant::now());
                self.onset_audio_ms = Some(self.audio_ms());
            }
            self.silence_ms = 0;
        } else {
            self.silence_ms += mono.len() as u64 * 1000 / self.sample_rate.max(1) as u64;
        }
    }

    /// 静音填充（WAV 回放补尾用）：只累计静音，不计入 RTF 的音频时长。
    fn on_silence(&mut self, samples: usize) {
        self.silence_ms += samples as u64 * 1000 / self.sample_rate.max(1) as u64;
    }

    fn on_update<F: FnMut(SessionEvent)>(&mut self, update: AsrUpdate, sink: &mut F) {
        match update {
            AsrUpdate::Partial(text) => {
                let text = strip_trailing_filler(&strip_leading_filler(&text));
                let onset_ms = self
                    .speech_start
                    .map(|t| t.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                let audio_ms = self.audio_ms();
                if self.first_partial_ms.is_none() {
                    self.first_partial_ms = Some(onset_ms);
                }
                if self.first_partial_audio_ms.is_none() {
                    self.first_partial_audio_ms =
                        Some(audio_ms.saturating_sub(self.onset_audio_ms.unwrap_or(0)));
                }
                sink(SessionEvent::Partial {
                    text,
                    onset_ms,
                    audio_ms,
                });
            }
            AsrUpdate::Segment(text) => {
                let text = strip_trailing_filler(&strip_leading_filler(&text));
                let onset_ms = self
                    .speech_start
                    .map(|t| t.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                let latency_ms = self.silence_ms;
                let audio_ms = self.audio_ms();
                sink(SessionEvent::Segment {
                    text,
                    onset_ms,
                    latency_ms,
                    audio_ms,
                });
                self.speech_start = None;
                self.onset_audio_ms = None;
            }
        }
    }

    fn audio_ms(&self) -> u64 {
        self.audio_samples * 1000 / self.sample_rate.max(1) as u64
    }

    fn metrics(&self) -> SessionEvent {
        let audio_secs = self.audio_ms() as f64 / 1000.0;
        let wall_secs = self.started.elapsed().as_secs_f64();
        SessionEvent::Metrics {
            first_partial_ms: self.first_partial_ms,
            first_partial_audio_ms: self.first_partial_audio_ms,
            rtf: if audio_secs > 0.0 {
                (self.proc_time.as_secs_f64() / audio_secs) as f32
            } else {
                0.0
            },
            audio_ms: self.audio_ms(),
            wall_ms: (wall_secs * 1000.0) as u64,
        }
    }
}

/// 从麦克风跑一轮会话，直到 `stop` 置位。
fn aec_filter(
    pipeline: &mut LinearPipeline,
    render: &gabtwin_audio::RenderTap,
    mono: &[f32],
    sr: u32,
) -> Vec<f32> {
    let input = if sr == 16_000 {
        mono.to_vec()
    } else {
        gabtwin_audio::resample_linear(mono, sr, 16_000)
    };
    let mut output = Vec::with_capacity(input.len());
    for chunk in input.chunks(160) {
        let mut capture_frame = [0.0f32; 160];
        capture_frame[..chunk.len()].copy_from_slice(chunk);
        let render_frame = render.pop(160);
        let _ = pipeline.handle_render_frame(&render_frame);
        let mut processed = [0.0f32; 160];
        let _ = pipeline.process_capture_frame(&capture_frame, &mut processed);
        output.extend_from_slice(&processed[..chunk.len()]);
    }
    if sr == 16_000 {
        output
    } else {
        gabtwin_audio::resample_linear(&output, 16_000, sr)
    }
}

pub fn run_mic<F>(cfg: &SessionConfig, stop: Arc<AtomicBool>, mut sink: F) -> Result<()>
where
    F: FnMut(SessionEvent),
{
    sink(SessionEvent::State {
        state: "loading".into(),
    });

    let mut asr = Asr::new(&cfg.asr).map_err(|e| {
        let msg = format!("{e:#}");
        sink(SessionEvent::Error {
            message: msg.clone(),
        });
        anyhow::anyhow!(msg)
    })?;
    let offline = match &cfg.offline {
        Some(config) => match OfflineAsr::new(config) {
            Ok(asr) => {
                sink(SessionEvent::State {
                    state: "offline_ready".into(),
                });
                Some(asr)
            }
            Err(err) => {
                sink(SessionEvent::Warning {
                    message: format!("句末定稿模型不可用，退回流式文本：{err:#}"),
                });
                None
            }
        },
        None => None,
    };
    let mut segment_audio: Vec<f32> = Vec::new();

    let mut capture = match Capture::start(&cfg.capture) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("{e:#}");
            sink(SessionEvent::Error {
                message: msg.clone(),
            });
            bail!(msg);
        }
    };

    let sr = capture.sample_rate;
    let channels = capture.channels as usize;
    let mut aec_pipeline = if cfg.aec_render.is_some() {
        let format = AudioFormat::ten_ms(16_000, 1);
        let delay_ms = std::env::var("GABTWIN_AEC_DELAY_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(100);
        match linear::builder(format, format).initial_delay_ms(delay_ms).build() {
            Ok(pipeline) => {
                sink(SessionEvent::Warning {
                    message: "AEC3 已启用：播放时麦克风保持开启".into(),
                });
                Some(pipeline)
            }
            Err(err) => {
                sink(SessionEvent::Warning {
                    message: format!("AEC3 初始化失败，退回半双工：{err:#}"),
                });
                None
            }
        }
    } else {
        None
    };
    if let Some(render) = &cfg.aec_render {
        render.clear();
    }
    warmup(&mut asr, sr, cfg.warmup_ms);
    sink(SessionEvent::Device {
        name: capture.device_name.clone(),
        sample_rate: sr,
        channels: capture.channels,
    });
    sink(SessionEvent::State {
        state: "listening".into(),
    });

    let frames_per_chunk = (sr as u64 * cfg.chunk_ms as u64 / 1000).max(160) as usize;
    let chunk_samples = frames_per_chunk * channels;
    let mut scratch = vec![0f32; chunk_samples];
    let mut pending: Vec<f32> = Vec::with_capacity(chunk_samples * 4);
    let mut stats = Stats::new(sr);
    let mut level_at = Instant::now();
    let mut was_muted = false;
    let mut was_ptt_off = false;
    let mut ptt_was_on = false;
    let mut ptt_release_tail: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        let got = recv_chunk(
            &mut capture.consumer,
            &mut scratch,
            chunk_samples,
            Duration::from_millis(200),
        );
        if got == 0 {
            continue;
        }
        pending.extend_from_slice(&scratch[..got]);

        // Push-to-talk：没按住空格就丢弃；松开后再留 800 ms 尾巴让端点触发。
        let ptt_active = cfg
            .ptt
            .as_ref()
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(true);
        if ptt_active {
            ptt_was_on = true;
            if was_ptt_off || ptt_release_tail.is_some() {
                asr.reset();
                segment_audio.clear();
            }
            ptt_release_tail = None;
            was_ptt_off = false;
        } else if ptt_was_on {
            if ptt_release_tail.is_none() {
                ptt_release_tail = Some(Instant::now() + Duration::from_millis(800));
            }
            let tail = ptt_release_tail
                .map(|at| at > Instant::now())
                .unwrap_or(false);
            if !tail {
                ptt_was_on = false;
                ptt_release_tail = None;
                pending.clear();
                segment_audio.clear();
                asr.reset();
                was_ptt_off = true;
                sink(SessionEvent::Muted { muted: true });
                continue;
            }
        } else {
            pending.clear();
            segment_audio.clear();
            if !was_ptt_off {
                asr.reset();
                was_ptt_off = true;
                sink(SessionEvent::Muted { muted: true });
            }
            continue;
        }

        // TTS 正在播放：丢弃音频，并清掉识别器里的残留假设。
        let muted = cfg
            .mute
            .as_ref()
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(false);
        if muted {
            pending.clear();
            segment_audio.clear();
            if !was_muted {
                asr.reset();
                was_muted = true;
                sink(SessionEvent::Muted { muted: true });
            }
            continue;
        }
        if was_muted {
            asr.reset();
            segment_audio.clear();
            was_muted = false;
            sink(SessionEvent::Muted { muted: false });
        }

        while pending.len() >= chunk_samples {
            let frame: Vec<f32> = pending.drain(..chunk_samples).collect();
            let mono_raw = downmix(&frame, channels);
            let mono = match (&mut aec_pipeline, cfg.aec_render.as_ref()) {
                (Some(pipeline), Some(render)) => {
                    aec_filter(pipeline, render, &mono_raw, sr)
                }
                _ => mono_raw,
            };
            segment_audio.extend_from_slice(&mono);
            let level = rms(&mono);
            stats.on_audio(&mono, level, cfg.onset_rms);

            if level_at.elapsed() >= Duration::from_millis(100) {
                sink(SessionEvent::Level { rms: level });
                level_at = Instant::now();
            }

            let tick = Instant::now();
            let updates = asr.accept(sr, &mono);
            stats.proc_time += tick.elapsed();
            for update in updates {
                match update {
                    AsrUpdate::Segment(text) => {
                        let min_samples = (sr as usize * cfg.min_segment_ms as usize) / 1000;
                        if segment_audio.len() < min_samples
                            || rms(&segment_audio) < cfg.min_segment_rms
                        {
                            segment_audio.clear();
                            continue;
                        }
                        let text =
                            finalize_text(offline.as_ref(), sr, &segment_audio, text, &mut stats);
                        segment_audio.clear();
                        stats.on_update(AsrUpdate::Segment(text), &mut sink);
                    }
                    other => stats.on_update(other, &mut sink),
                }
            }
        }
    }

    for update in asr.finish() {
        match update {
            AsrUpdate::Segment(text) => {
                let min_samples = (sr as usize * cfg.min_segment_ms as usize) / 1000;
                if segment_audio.len() < min_samples
                    || rms(&segment_audio) < cfg.min_segment_rms
                {
                    segment_audio.clear();
                    continue;
                }
                let text = finalize_text(offline.as_ref(), sr, &segment_audio, text, &mut stats);
                segment_audio.clear();
                stats.on_update(AsrUpdate::Segment(text), &mut sink);
            }
            other => stats.on_update(other, &mut sink),
        }
    }

    if capture.overflowed.load(Ordering::Relaxed) {
        sink(SessionEvent::Warning {
            message: "采集环形缓冲曾经写满，可能有丢帧；识别线程跟不上时会出现。".into(),
        });
    }

    sink(stats.metrics());
    sink(SessionEvent::State {
        state: "idle".into(),
    });
    Ok(())
}

/// 用一段 WAV 走同一条识别管线。`realtime` 为真时按音频时长节流，
/// 用来测「说话时字幕有多跟手」；为假时尽快跑完，用来测 RTF。
pub fn replay_wav<F>(
    path: &Path,
    cfg: &SessionConfig,
    realtime: bool,
    stop: Arc<AtomicBool>,
    mut sink: F,
) -> Result<()>
where
    F: FnMut(SessionEvent),
{
    let (mono, sr) = read_wav(path)?;

    sink(SessionEvent::State {
        state: "loading".into(),
    });
    let mut asr = Asr::new(&cfg.asr)?;
    let offline = match &cfg.offline {
        Some(config) => match OfflineAsr::new(config) {
            Ok(asr) => Some(asr),
            Err(err) => {
                sink(SessionEvent::Warning {
                    message: format!("句末定稿模型不可用，退回流式文本：{err:#}"),
                });
                None
            }
        },
        None => None,
    };
    let mut segment_audio: Vec<f32> = Vec::new();
    sink(SessionEvent::State {
        state: "replaying".into(),
    });

    let frames_per_chunk = (sr as u64 * cfg.chunk_ms as u64 / 1000).max(160) as usize;
    warmup(&mut asr, sr, cfg.warmup_ms);
    let mut stats = Stats::new(sr);
    let started = Instant::now();
    let mut fed: u64 = 0;

    for chunk in mono.chunks(frames_per_chunk) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if realtime {
            let target = Duration::from_secs_f64(fed as f64 / sr as f64);
            let now = started.elapsed();
            if target > now {
                std::thread::sleep(target - now);
            }
        }
        fed += chunk.len() as u64;
        let level = rms(chunk);
        stats.on_audio(chunk, level, cfg.onset_rms);
        segment_audio.extend_from_slice(chunk);
        sink(SessionEvent::Level { rms: level });
        let tick = Instant::now();
        let updates = asr.accept(sr, chunk);
        stats.proc_time += tick.elapsed();
        for update in updates {
            match update {
                AsrUpdate::Segment(text) => {
                    let text =
                        finalize_text(offline.as_ref(), sr, &segment_audio, text, &mut stats);
                    segment_audio.clear();
                    stats.on_update(AsrUpdate::Segment(text), &mut sink);
                }
                other => stats.on_update(other, &mut sink),
            }
        }
    }

    // 补 1.2 s 静音，让端点规则有机会把最后一句定稿。
    let pad = vec![0f32; (sr as f32 * 1.2) as usize];
    for chunk in pad.chunks(frames_per_chunk) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if realtime {
            std::thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / sr as f64));
        }
        stats.on_silence(chunk.len());
        let tick = Instant::now();
        let updates = asr.accept(sr, chunk);
        stats.proc_time += tick.elapsed();
        for update in updates {
            match update {
                AsrUpdate::Segment(text) => {
                    let text =
                        finalize_text(offline.as_ref(), sr, &segment_audio, text, &mut stats);
                    segment_audio.clear();
                    stats.on_update(AsrUpdate::Segment(text), &mut sink);
                }
                other => stats.on_update(other, &mut sink),
            }
        }
    }

    for update in asr.finish() {
        match update {
            AsrUpdate::Segment(text) => {
                let min_samples = (sr as usize * cfg.min_segment_ms as usize) / 1000;
                if segment_audio.len() < min_samples
                    || rms(&segment_audio) < cfg.min_segment_rms
                {
                    segment_audio.clear();
                    continue;
                }
                let text = finalize_text(offline.as_ref(), sr, &segment_audio, text, &mut stats);
                segment_audio.clear();
                stats.on_update(AsrUpdate::Segment(text), &mut sink);
            }
            other => stats.on_update(other, &mut sink),
        }
    }

    sink(stats.metrics());
    sink(SessionEvent::State {
        state: "idle".into(),
    });
    Ok(())
}

/// 句末用离线模型重解这一段音频；失败或结果为空时退回流式文本。
fn finalize_text(
    offline: Option<&OfflineAsr>,
    sample_rate: u32,
    audio: &[f32],
    fallback: String,
    stats: &mut Stats,
) -> String {
    let Some(asr) = offline else {
        return fallback;
    };
    if audio.len() < (sample_rate as usize / 5) {
        return fallback;
    }
    let tick = Instant::now();
    let text = asr.transcribe(sample_rate, audio);
    stats.proc_time += tick.elapsed();
    if text.trim().is_empty() {
        fallback
    } else {
        text
    }
}

/// 某些模型在长静音后会多吐一个语气词（"嗯"/"啊"）。它通常不是用户说的，
/// 在显示与落账前去掉，避免每条都带一个"嗯"。
fn strip_trailing_filler(text: &str) -> String {
    let mut out = text.trim().to_string();
    for filler in ["嗯", "啊", "哦"] {
        for punct in ["", "。", "！", "？", ".", "!", "?"] {
            let suffix = format!("{filler}{punct}");
            if out.ends_with(&suffix) {
                let cut = out.len() - suffix.len();
                let head = out[..cut].trim_end().to_string();
                if !head.is_empty() {
                    out = format!("{head}{punct}");
                }
                break;
            }
        }
    }
    out
}

fn strip_leading_filler(text: &str) -> String {
    for filler in ["嗯", "啊", "哦", "呃", "唉"] {
        if let Some(rest) = text.strip_prefix(filler) {
            if !rest.trim().is_empty() {
                return rest.trim_start().to_string();
            }
        }
    }
    text.to_string()
}

/// 给识别器喂一段静音，让流式编码器先进入稳定状态。
fn warmup(asr: &mut Asr, sample_rate: u32, ms: u32) {
    if ms == 0 {
        return;
    }
    let samples = (sample_rate as u64 * ms as u64 / 1000) as usize;
    let silence = vec![0.0f32; samples];
    let _ = asr.accept(sample_rate, &silence);
}

pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("打开 WAV 失败：{}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => match spec.bits_per_sample {
            16 => reader
                .samples::<i16>()
                .map(|s| s.map(|v| v as f32 / 32768.0))
                .collect::<std::result::Result<Vec<_>, _>>()?,
            24 | 32 => reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / 2147483648.0))
                .collect::<std::result::Result<Vec<_>, _>>()?,
            bits => bail!("不支持的 WAV 位深：{bits}"),
        },
    };

    Ok((downmix(&samples, channels), spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_averages() {
        let mono = downmix(&[1.0, 0.0, 0.5, 0.5], 2);
        assert_eq!(mono, vec![0.5, 0.5]);
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0; 100]), 0.0);
    }

    #[test]
    fn home_can_be_overridden() {
        std::env::set_var("GABTWIN_HOME", "/tmp/gabtwin-test");
        assert_eq!(gabtwin_home(), PathBuf::from("/tmp/gabtwin-test"));
        std::env::remove_var("GABTWIN_HOME");
    }
}

#[cfg(test)]
mod aec_tests {
    use aec3::nodes::audio::AudioFormat;
    use aec3::pipelines::linear;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn aec3_suppresses_delayed_echo() {
        let rate = 16_000u32;
        let frame = 160usize;
        let frames = 400usize; // 4 s
        let format = AudioFormat::ten_ms(rate, 1);
        let mut pipeline = linear::builder(format, format)
            .initial_delay_ms(20)
            .build()
            .expect("pipeline");

        let mut seed = 0x1234_5678u32;
        let mut render = vec![0.0f32; frame * frames];
        for sample in &mut render {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *sample = ((seed >> 8) as f32 / 16_777_216.0 - 0.5) * 0.5;
        }
        let delay = frame * 2; // 20 ms
        let mut capture = vec![0.0f32; render.len()];
        for i in delay..capture.len() {
            capture[i] = render[i - delay] * 0.4;
        }
        let mut output = vec![0.0f32; capture.len()];
        for f in 0..frames {
            let r = &render[f * frame..(f + 1) * frame];
            let c = &capture[f * frame..(f + 1) * frame];
            pipeline.handle_render_frame(r).expect("render");
            let mut processed = [0.0f32; 160];
            pipeline
                .process_capture_frame(c, &mut processed)
                .expect("capture");
            output[f * frame..(f + 1) * frame].copy_from_slice(&processed);
        }

        let start = frame * 200; // 收敛 2 s 后
        let before = rms(&capture[start..]);
        let after = rms(&output[start..]);
        let erle = 20.0 * (before / after.max(1e-9)).log10();
        println!("AEC ERLE = {erle:.1} dB ({before:.5} -> {after:.5})");
        assert!(erle > 6.0, "ERLE too low: {erle:.1} dB");
    }
}
