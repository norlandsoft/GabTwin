//! 麦克风采集。
//!
//! 设计约束（见 docs/01-architecture.md）：
//! - 音频回调里只做"搬运"：把设备样本推入预分配的无锁环形缓冲，不做重采样、
//!   不做降混、不分配、不加锁。
//! - 降混、电平、识别都在工作线程完成。
//! - 采样率转换交给 sherpa-onnx（`accept_waveform` 接受任意采样率），
//!   所以这里先不做 rubato。

use anyhow::{anyhow, Context, Result};
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
