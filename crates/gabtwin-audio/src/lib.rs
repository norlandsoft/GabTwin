//! 麦克风采集。
//!
//! 设计约束（见 docs/01-architecture.md）：
//! - 音频回调里只做"搬运"：把设备样本推入预分配的无锁环形缓冲，不做重采样、
//!   不做降混、不分配、不加锁。
//! - 降混、电平、识别都在工作线程完成。
//! - 采样率转换交给 sherpa-onnx（`accept_waveform` 接受任意采样率），
//!   所以这里先不做 rubato。

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

/// 枚举全部输入设备。默认设备优先排在前面。
pub fn input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.description().ok().map(|x| x.name().to_string()));
    let mut out = Vec::new();
    for device in host.input_devices().context("枚举输入设备失败")? {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        let cfg = device.default_input_config().ok();
        out.push(DeviceInfo {
            is_default: Some(&name) == default_name.as_ref(),
            name,
            sample_rate: cfg.as_ref().map(|c| c.sample_rate()).unwrap_or(0),
            channels: cfg.as_ref().map(|c| c.channels()).unwrap_or(0),
        });
    }
    out.sort_by_key(|d| !d.is_default);
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// `None` 表示系统默认输入设备。
    pub device_name: Option<String>,
    /// 环形缓冲长度（秒）。太短会丢帧，太长会拖慢停止。
    pub buffer_seconds: f32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            buffer_seconds: 10.0,
        }
    }
}

/// 采集句柄。必须保证 `stream` 与 `consumer` 在同一个线程里存活到结束。
pub struct Capture {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub consumer: HeapCons<f32>,
    /// 环形缓冲写满（丢帧）时置位，用来区分"识别慢"与"采集丢帧"。
    pub overflowed: Arc<AtomicBool>,
    /// 最近一次回调的峰值电平（f32 位模式存放），供 UI 轮询。
    pub peak: Arc<AtomicU32>,
    _stream: cpal::Stream,
}

impl Capture {
    pub fn start(cfg: &CaptureConfig) -> Result<Self> {
        let host = cpal::default_host();
        let device = match &cfg.device_name {
            Some(name) => host
                .input_devices()
                .context("枚举输入设备失败")?
                .find(|d| {
                    d.description()
                        .ok()
                        .is_some_and(|x| x.name() == name.as_str())
                })
                .ok_or_else(|| anyhow!("找不到输入设备：{name}"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("没有可用的默认输入设备"))?,
        };

        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        let supported = device
            .default_input_config()
            .context("读取默认输入配置失败（macOS 上通常是未授予麦克风权限）")?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let channels = config.channels as usize;
        let sample_rate = config.sample_rate;

        let capacity =
            (sample_rate as f32 * cfg.buffer_seconds * channels as f32).max(4096.0) as usize;
        let rb = HeapRb::<f32>::new(capacity);
        let (mut producer, consumer) = rb.split();
        let overflowed = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU32::new(0f32.to_bits()));

        let ov = overflowed.clone();
        let pk = peak.clone();
        let err_fn = |err| eprintln!("[audio] 采集流错误：{err}");

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                config.clone(),
                move |data: &[f32], _| {
                    let _ = pk.store(f32_peak(data).to_bits(), Ordering::Relaxed);
                    if producer.push_slice(data) < data.len() {
                        ov.store(true, Ordering::Relaxed);
                    }
                },
                err_fn,
                None,
            )?,
            SampleFormat::I16 => {
                let mut scratch: Vec<f32> = Vec::new();
                device.build_input_stream(
                    config.clone(),
                    move |data: &[i16], _| {
                        scratch.clear();
                        scratch.extend(data.iter().map(|&s| s as f32 / 32768.0));
                        let _ = pk.store(f32_peak(&scratch).to_bits(), Ordering::Relaxed);
                        if producer.push_slice(&scratch) < scratch.len() {
                            ov.store(true, Ordering::Relaxed);
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            SampleFormat::U16 => {
                let mut scratch: Vec<f32> = Vec::new();
                device.build_input_stream(
                    config.clone(),
                    move |data: &[u16], _| {
                        scratch.clear();
                        scratch.extend(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0));
                        let _ = pk.store(f32_peak(&scratch).to_bits(), Ordering::Relaxed);
                        if producer.push_slice(&scratch) < scratch.len() {
                            ov.store(true, Ordering::Relaxed);
                        }
                    },
                    err_fn,
                    None,
                )?
            }
            other => {
                return Err(anyhow!(
                    "暂不支持的采样格式 {other:?}（当前支持 f32 / i16 / u16）"
                ))
            }
        };

        stream.play().context("启动采集流失败")?;

        // 让消费者先跑起来，避免首帧前溢出误报。
        std::thread::sleep(std::time::Duration::from_millis(20));

        Ok(Self {
            device_name,
            sample_rate,
            channels: channels as u16,
            consumer,
            overflowed,
            peak,
            _stream: stream,
        })
    }
}

/// 峰值绝对值，用于判断是否有声音。
pub fn f32_peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0f32, |m, s| m.max(s.abs()))
}

/// 均方根，用于电平表与起始点检测。
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// 多声道降混为单声道。
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// 从环形缓冲里取一块，阻塞等待最多 `timeout`。
/// 返回为空表示超时。
pub fn recv_chunk(
    consumer: &mut HeapCons<f32>,
    scratch: &mut Vec<f32>,
    want: usize,
    timeout: std::time::Duration,
) -> usize {
    let deadline = std::time::Instant::now() + timeout;
    let mut got = 0;
    while got < want {
        let n = consumer.pop_slice(&mut scratch[got..want]);
        got += n;
        if got >= want {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    got
}

// ---------- 播放 ----------

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

pub enum SpeakerCommand {
    Play(Vec<f32>),
    Clear,
    Quit,
}

/// 播放句柄。真正的 cpal 输出流在自己的线程里，句柄只负责投递音频。
pub struct SpeakerHandle {
    tx: mpsc::Sender<SpeakerCommand>,
    pub sample_rate: u32,
    /// 输出设备是耳机/耳麦时，回声很小，可以边播边听、支持插话。
    pub headphones: bool,
}

impl SpeakerHandle {
    pub fn push(&self, samples: Vec<f32>) {
        let _ = self.tx.send(SpeakerCommand::Play(samples));
    }
    pub fn clear(&self) {
        let _ = self.tx.send(SpeakerCommand::Clear);
    }
}

/// 启动默认输出设备；返回句柄和实际输出采样率。
pub fn start_speaker() -> Result<SpeakerHandle> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        match build_output_stream(buffer.clone()) {
            Ok((stream, rate, headphones)) => {
                let _ = ready_tx.send(Ok((rate, headphones)));
                while let Ok(command) = rx.recv() {
                    match command {
                        SpeakerCommand::Play(samples) => {
                            if let Ok(mut queue) = buffer.lock() {
                                queue.extend(samples);
                            }
                        }
                        SpeakerCommand::Clear => {
                            if let Ok(mut queue) = buffer.lock() {
                                queue.clear();
                            }
                        }
                        SpeakerCommand::Quit => break,
                    }
                }
                drop(stream);
            }
            Err(err) => {
                let _ = ready_tx.send(Err(format!("{err:#}")));
            }
        }
    });

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok((rate, headphones))) => Ok(SpeakerHandle {
            tx,
            sample_rate: rate,
            headphones,
        }),
        Ok(Err(message)) => Err(anyhow!(message)),
        Err(_) => Err(anyhow!("启动音频输出超时")),
    }
}

fn build_output_stream(
    buffer: Arc<Mutex<VecDeque<f32>>>,
) -> Result<(cpal::Stream, u32, bool), anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("没有可用的默认输出设备"))?;
    let supported = device
        .default_output_config()
        .context("读取默认输出配置失败")?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.config();
    let headphones = device
        .description()
        .map(|d| {
            matches!(
                d.device_type(),
                cpal::DeviceType::Headphones
                    | cpal::DeviceType::Headset
                    | cpal::DeviceType::Earpiece
                    | cpal::DeviceType::HearingAid
            )
        })
        .unwrap_or(false);
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let err_fn = |err| eprintln!("[audio] 输出流错误：{err}");

    let stream = match sample_format {
        SampleFormat::F32 => {
            let b = buffer.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [f32], _| fill_f32(data, channels, &b),
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let b = buffer.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [i16], _| fill_i16(data, channels, &b),
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let b = buffer.clone();
            device.build_output_stream(
                config.clone(),
                move |data: &mut [u16], _| fill_u16(data, channels, &b),
                err_fn,
                None,
            )?
        }
        other => bail!("暂不支持的输出采样格式 {other:?}"),
    };
    stream.play()?;
    Ok((stream, rate, headphones))
}

fn pop_samples(buffer: &Arc<Mutex<VecDeque<f32>>>, frames: usize) -> Option<Vec<f32>> {
    let mut queue = buffer.try_lock().ok()?;
    Some(
        (0..frames)
            .map(|_| queue.pop_front().unwrap_or(0.0))
            .collect(),
    )
}

fn fill_f32(data: &mut [f32], channels: usize, buffer: &Arc<Mutex<VecDeque<f32>>>) {
    if channels == 0 {
        return;
    }
    let frames = data.len() / channels;
    let Some(values) = pop_samples(buffer, frames) else {
        data.fill(0.0);
        return;
    };
    for (i, value) in values.into_iter().enumerate() {
        for c in 0..channels {
            data[i * channels + c] = value;
        }
    }
}

fn fill_i16(data: &mut [i16], channels: usize, buffer: &Arc<Mutex<VecDeque<f32>>>) {
    if channels == 0 {
        return;
    }
    let frames = data.len() / channels;
    let Some(values) = pop_samples(buffer, frames) else {
        data.fill(0);
        return;
    };
    for (i, value) in values.into_iter().enumerate() {
        let sample = (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        for c in 0..channels {
            data[i * channels + c] = sample;
        }
    }
}

fn fill_u16(data: &mut [u16], channels: usize, buffer: &Arc<Mutex<VecDeque<f32>>>) {
    if channels == 0 {
        return;
    }
    let frames = data.len() / channels;
    let Some(values) = pop_samples(buffer, frames) else {
        data.fill(32768);
        return;
    };
    for (i, value) in values.into_iter().enumerate() {
        let sample = (((value.clamp(-1.0, 1.0) * 0.5) + 0.5) * u16::MAX as f32) as u16;
        for c in 0..channels {
            data[i * channels + c] = sample;
        }
    }
}

/// 线性重采样（TTS 22050 → 设备 48000 这类）。语音够用，不做抗混叠。
pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}
