//! 无头验证工具。
//!
//! ```bash
//! # 列出输入设备
//! gabtwin-probe --list-devices
//!
//! # 用模型自带的 WAV 验证识别（尽快跑完，测 RTF）
//! gabtwin-probe --wav "$MODEL/test_wavs/0.wav"
//!
//! # 按真实语速回放，观察字幕跟着说话出现的节奏
//! gabtwin-probe --wav "$MODEL/test_wavs/0.wav" --realtime
//!
//! # 真实麦克风，20 秒
//! gabtwin-probe --mic --seconds 20
//! ```

use anyhow::{bail, Result};
use clap::Parser;
use gabtwin_session::{
    default_asr_model_dir, input_devices, read_wav, replay_wav, run_mic, OfflineAsr,
    OfflineAsrConfig, SessionConfig, SessionEvent,
};
use gabtwin_tts::{ClauseSplitter, Tts, TtsConfig};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "gabtwin-probe", about = "GabTwin 语音输入验证工具")]
struct Args {
    /// 用一段 WAV 走识别管线
    #[arg(long)]
    wav: Option<PathBuf>,

    /// 用麦克风实时识别
    #[arg(long)]
    mic: bool,

    /// 麦克风模式跑多少秒
    #[arg(long, default_value_t = 20.0)]
    seconds: f64,

    /// 按真实语速回放（默认尽快跑完）
    #[arg(long)]
    realtime: bool,

    /// 模型目录
    #[arg(long)]
    model_dir: Option<PathBuf>,

    /// 指定输入设备名（默认系统默认设备）
    #[arg(long)]
    device: Option<String>,

    /// 句尾静音多少秒算一句话结束
    #[arg(long, default_value_t = 0.8)]
    endpoint: f32,

    /// 起始点判定阈值（RMS）
    #[arg(long, default_value_t = 0.01)]
    onset: f32,

    /// 每次送进识别器的音频块长度（毫秒）
    #[arg(long, default_value_t = 50)]
    chunk_ms: u32,

    /// 用 modified_beam_search 解码（略准，略慢）
    #[arg(long)]
    beam: bool,

    /// blank penalty（负值抑制重复 token）
    #[arg(long, default_value_t = 0.0)]
    blank_penalty: f32,

    /// 会话开始前喂多少毫秒静音做预热（默认 0）
    #[arg(long, default_value_t = 0)]
    warmup_ms: u32,

    /// 把事件写成 JSONL
    #[arg(long)]
    jsonl: Option<PathBuf>,

    /// 打印电平
    #[arg(long)]
    verbose: bool,

    /// 用离线模型（SenseVoice）整段识别一个 WAV，然后退出
    #[arg(long)]
    offline: Option<PathBuf>,

    /// 句末用离线模型（SenseVoice）定稿
    #[arg(long)]
    offline_final: Option<PathBuf>,

    /// 只做 TTS：合成 --text 到 --out（WAV）
    #[arg(long)]
    tts: Option<PathBuf>,

    /// --tts 要合成的文本
    #[arg(long)]
    text: Option<String>,

    /// --tts 的输出 WAV
    #[arg(long)]
    out: Option<PathBuf>,

    /// --tts 的音色编号（Kokoro：中文女声 3–57，男声 58–102）
    #[arg(long, default_value_t = 0)]
    sid: i32,

    /// --tts 的 onnxruntime provider（cpu / coreml）
    #[arg(long, default_value = "cpu")]
    provider: String,

    /// --tts 时按应用里的分句器切分，逐句合成后拼接（验证断句听感）
    #[arg(long)]
    split: bool,

    /// 列出输入设备后退出
    #[arg(long)]
    list_devices: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.list_devices {
        for d in input_devices()? {
            println!(
                "{} {:<34} {} Hz / {} ch",
                if d.is_default { "*" } else { " " },
                d.name,
                d.sample_rate,
                d.channels
            );
        }
        return Ok(());
    }

    if let Some(tts_dir) = &args.tts {
        let text = args
            .text
            .clone()
            .ok_or_else(|| anyhow::anyhow!("--tts 需要同时给 --text"))?;
        let out = args
            .out
            .clone()
            .unwrap_or_else(|| PathBuf::from("/tmp/gabtwin-tts.wav"));
        let tts =
            Tts::new(&TtsConfig::new(tts_dir.clone())).map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let started = Instant::now();
        let samples = if args.split {
            let mut splitter = ClauseSplitter::new();
            let mut pieces = splitter.push(&text);
            if let Some(last) = splitter.flush() {
                pieces.push(last);
            }
            let mut all = Vec::new();
            for piece in pieces {
                println!("句：{piece}");
                all.extend(tts.synthesize(&piece)?);
            }
            all
        } else {
            tts.synthesize(&text)?
        };
        let elapsed = started.elapsed().as_secs_f64();
        let sr = tts.sample_rate();
        let seconds = samples.len() as f64 / sr as f64;
        write_wav(&out, &samples, sr)?;
        println!("{text}");
        println!(
            "— {:.2} s 音频 / {:.0} ms，RTF {:.3}，已写入 {}",
            seconds,
            elapsed * 1000.0,
            elapsed / seconds.max(0.001),
            out.display()
        );
        return Ok(());
    }

    if let Some(offline_dir) = &args.offline {
        let wav = args
            .wav
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--offline 需要同时给 --wav"))?;
        let (mono, sr) = read_wav(wav)?;
        let asr = OfflineAsr::new(&OfflineAsrConfig::new(offline_dir.clone()))
            .map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let started = Instant::now();
        let text = asr.transcribe(sr, &mono);
        let elapsed = started.elapsed().as_secs_f64();
        println!("{text}");
        println!(
            "— 离线识别 {:.2} s 音频 / {:.0} ms，RTF {:.3}",
            mono.len() as f64 / sr as f64,
            elapsed * 1000.0,
            elapsed / (mono.len() as f64 / sr as f64)
        );
        return Ok(());
    }

    let model_dir = args.model_dir.unwrap_or_else(default_asr_model_dir);
    if !model_dir.is_dir() {
        bail!(
            "模型目录不存在：{}\n先按 docs/dev/01-voice-input.md 下载模型，或传 --model-dir",
            model_dir.display()
        );
    }

    let mut cfg = SessionConfig::new(&model_dir);
    cfg.asr.endpoint_silence_s = args.endpoint;
    cfg.onset_rms = args.onset;
    cfg.chunk_ms = args.chunk_ms;
    cfg.asr.beam_search = args.beam;
    cfg.asr.blank_penalty = args.blank_penalty;
    if let Some(dir) = &args.offline_final {
        cfg.offline = Some(OfflineAsrConfig::new(dir.clone()));
    }
    cfg.warmup_ms = args.warmup_ms;
    if let Some(device) = &args.device {
        cfg.capture.device_name = Some(device.clone());
    }

    let started = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let mut out: Box<dyn Write> = match &args.jsonl {
        Some(path) => Box::new(std::fs::File::create(path)?),
        None => Box::new(std::io::sink()),
    };

    let timer_started = Arc::new(AtomicBool::new(false));
    let stop_for_timer = stop.clone();
    let mut last_level = Instant::now();
    let mut sink = |ev: SessionEvent| {
        if let SessionEvent::State { state } = &ev {
            if state == "listening" && !timer_started.swap(true, Ordering::SeqCst) {
                let stop_after = stop_for_timer.clone();
                let seconds = args.seconds;
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs_f64(seconds));
                    stop_after.store(true, Ordering::Relaxed);
                });
            }
        }
        let t = started.elapsed().as_millis() as u64;
        let line = serde_json::json!({ "t_ms": t, "ev": &ev });
        let _ = writeln!(out, "{line}");

        match &ev {
            SessionEvent::State { state } => {
                println!("[{:>7.2}s] 状态     {}", t as f64 / 1000.0, state)
            }
            SessionEvent::Device {
                name,
                sample_rate,
                channels,
            } => println!(
                "[{:>7.2}s] 设备     {name}  ({sample_rate} Hz, {channels} ch)",
                t as f64 / 1000.0
            ),
            SessionEvent::Muted { muted } => {
                if args.verbose {
                    println!(
                        "[{:>7.2}s] 麦克风   {}",
                        t as f64 / 1000.0,
                        if *muted {
                            "暂停（TTS 播放）"
                        } else {
                            "恢复"
                        }
                    );
                }
            }
            SessionEvent::Level { rms } => {
                if args.verbose && last_level.elapsed() >= Duration::from_millis(500) {
                    println!(
                        "[{:>7.2}s] 电平     {:<24} {:.4}",
                        t as f64 / 1000.0,
                        bar(*rms),
                        rms
                    );
                    last_level = Instant::now();
                }
            }
            SessionEvent::Partial { text, onset_ms, .. } => println!(
                "[{:>7.2}s] 字幕     {text}   (+{onset_ms} ms)",
                t as f64 / 1000.0
            ),
            SessionEvent::Segment {
                text,
                latency_ms,
                onset_ms,
                ..
            } => println!(
                "[{:>7.2}s] 定稿     {text}   (句长 {onset_ms} ms, 静音 {latency_ms} ms)",
                t as f64 / 1000.0
            ),
            SessionEvent::Metrics {
                first_partial_ms,
                first_partial_audio_ms,
                rtf,
                audio_ms,
                wall_ms,
            } => println!(
                "\n— 首字 {} ms / 音频位 {} ms | 音频 {:.2} s | 处理 {:.2} s | RTF {:.3}",
                first_partial_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".into()),
                first_partial_audio_ms
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "n/a".into()),
                *audio_ms as f64 / 1000.0,
                *wall_ms as f64 / 1000.0,
                rtf
            ),
            SessionEvent::Warning { message } => {
                println!("[{:>7.2}s] 警告     {message}", t as f64 / 1000.0)
            }
            SessionEvent::Error { message } => {
                println!("[{:>7.2}s] 错误     {message}", t as f64 / 1000.0)
            }
        }
    };

    if let Some(wav) = &args.wav {
        println!("模型  {}", model_dir.display());
        println!(
            "输入  {}（{}）",
            wav.display(),
            if args.realtime {
                "实时语速"
            } else {
                "尽快"
            }
        );
        replay_wav(wav, &cfg, args.realtime, stop, &mut sink)?;
    } else if args.mic {
        println!("模型  {}", model_dir.display());
        println!("输入  麦克风（{} 秒）", args.seconds);
        run_mic(&cfg, stop.clone(), &mut sink)?;
    } else {
        bail!("至少指定 --wav <文件> 或 --mic（--help 看用法）");
    }

    Ok(())
}

fn write_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

fn bar(rms: f32) -> String {
    let n = ((rms * 60.0).min(24.0)) as usize;
    format!("{}{}", "█".repeat(n), "·".repeat(24 - n))
}
