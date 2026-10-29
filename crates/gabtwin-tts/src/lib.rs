//! 本地 TTS：sherpa-onnx VITS（中文）+ 口语文本切分。
//!
//! 切分的目标不是"句子正确"，而是**首句尽量短**：LLM 的流式回复一到，
//! 只要凑出一个能听懂的小句就先合成播放，后面的句子继续合成。

use anyhow::{bail, Context, Result};
use std::io::Read;
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsZipvoiceModelConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub model_dir: PathBuf,
    pub num_threads: i32,
    pub speaker_id: i32,
    pub speed: f32,
    /// onnxruntime provider：cpu / coreml（macOS）
    pub provider: String,
    pub debug: bool,
    /// ZipVoice 零样本克隆：参考音频与参考文本。
    pub reference_wav: Option<PathBuf>,
    pub reference_text: Option<String>,
    /// ZipVoice 的流匹配步数（0 = 引擎默认）
    pub num_steps: i32,
}

impl TtsConfig {
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            num_threads: 8,
            speaker_id: 0,
            speed: 1.0,
            provider: "cpu".to_string(),
            debug: false,
            reference_wav: None,
            reference_text: None,
            num_steps: 0,
        }
    }
}

/// 把 WAV 字节解码成单声道 f32。
fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<std::result::Result<Vec<_>, _>>()?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };
    let mut samples: Vec<f32> = raw
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    cleanup(&mut samples, spec.sample_rate);
    Ok((samples, spec.sample_rate))
}

/// 远程 TTS 服务（如 IndexTTS sidecar）：POST /tts -> WAV。
pub struct RemoteTts {
    url: String,
    client: reqwest::blocking::Client,
}

impl RemoteTts {
    pub fn new(url: impl Into<String>) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()?;
        Ok(Self {
            url: url.into().trim_end_matches('/').to_string(),
            client,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        22_050
    }

    /// 重置同一对话的音色会话（sidecar 会清掉“第一段参考音”）。
    pub fn voice_reset(&self) -> Result<()> {
        self.client
            .post(format!("{}/voice_reset", self.url))
            .send()?
            .error_for_status()?;
        Ok(())
    }

    /// 流式 PCM（s16le，24 kHz）：边收边回调，首块不再等整句。
    pub fn stream_pcm<F>(&self, text: &str, on_chunk: &mut F) -> Result<()>
    where
        F: FnMut(&[f32], u32),
    {
        let response = self
            .client
            .post(format!("{}/tts_stream", self.url))
            .json(&serde_json::json!({ "text": text, "lang": "zh" }))
            .send()?;
        let status = response.status();
        if !status.is_success() {
            bail!("远程 TTS 流式返回 {status}");
        }
        let rate = response
            .headers()
            .get("x-sample-rate")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(24_000);
        let mut response = response;
        let mut carry: Vec<u8> = Vec::new();
        let mut raw = [0u8; 8192];
        loop {
            let read = response.read(&mut raw)?;
            if read == 0 {
                break;
            }
            carry.extend_from_slice(&raw[..read]);
            let even = carry.len() & !1;
            if even == 0 {
                continue;
            }
            let samples: Vec<f32> = carry[..even]
                .chunks_exact(2)
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0)
                .collect();
            carry.drain(..even);
            if !samples.is_empty() {
                on_chunk(&samples, rate);
            }
        }
        Ok(())
    }

    pub fn synthesize(&self, text: &str) -> Result<(Vec<f32>, u32)> {
        let response = self
            .client
            .post(format!("{}/tts", self.url))
            .json(&serde_json::json!({ "text": text, "lang": "zh" }))
            .send()?;
        let status = response.status();
        if !status.is_success() {
            bail!("远程 TTS 返回 {status}");
        }
        decode_wav(&response.bytes()?)
    }
}

/// 智谱 GLM-TTS 云端克隆音色。
pub struct GlmTts {
    key: String,
    voice: String,
    client: reqwest::blocking::Client,
}

impl GlmTts {
    pub fn new(key: impl Into<String>, voice: impl Into<String>) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        Ok(Self {
            key: key.into(),
            voice: voice.into(),
            client,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        24_000
    }

    pub fn synthesize(&self, text: &str) -> Result<(Vec<f32>, u32)> {
        let response = self
            .client
            .post("https://open.bigmodel.cn/api/paas/v4/audio/speech")
            .bearer_auth(&self.key)
            .json(&serde_json::json!({
                "model": "glm-tts",
                "input": text,
                "voice": self.voice,
                "response_format": "wav",
                "speed": 1.0
            }))
            .send()?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "GLM TTS 返回 {status}: {}",
                response.text().unwrap_or_default()
            );
        }
        decode_wav(&response.bytes()?)
    }
}

struct Reference {
    samples: Vec<f32>,
    sample_rate: u32,
    text: String,
}

pub struct Tts {
    inner: OfflineTts,
    cfg: TtsConfig,
    reference: Option<Reference>,
    zipvoice: bool,
    kind: &'static str,
}

fn load_reference(path: &Path, text: &str) -> Result<Reference> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("读取参考音频失败：{}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => match spec.bits_per_sample {
            16 => reader
                .samples::<i16>()
                .map(|s| s.map(|v| v as f32 / 32768.0))
                .collect::<std::result::Result<Vec<_>, _>>()?,
            32 => reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / 2147483648.0))
                .collect::<std::result::Result<Vec<_>, _>>()?,
            bits => bail!("参考音频位数不支持：{bits}"),
        },
    };
    let samples: Vec<f32> = raw
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    if samples.len() < spec.sample_rate as usize / 2 {
        bail!("参考音频太短（<0.5 s）：{}", path.display());
    }
    Ok(Reference {
        samples,
        sample_rate: spec.sample_rate,
        text: text.trim().to_string(),
    })
}

/// 内置发音修正表（与 Matcha 词典同格式）。
const CORRECTIONS: &str = include_str!("../data/corrections.txt");

fn read_lexicon_text(text: &str, map: &mut BTreeMap<String, String>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(word), Some(first)) = (parts.next(), parts.next()) {
            let mut pinyin = first.to_string();
            for rest in parts {
                pinyin.push(' ');
                pinyin.push_str(rest);
            }
            map.insert(word.to_string(), pinyin);
        }
    }
}

fn read_lexicon(path: &Path, map: &mut BTreeMap<String, String>) {
    if let Ok(text) = std::fs::read_to_string(path) {
        read_lexicon_text(&text, map);
    }
}

/// 模型词典 + 内置修正 + 用户词条的合并词典；后写入的覆盖前面的。
fn merged_matcha_lexicon(dir: &Path) -> Result<PathBuf> {
    let mut map = BTreeMap::new();
    read_lexicon(&dir.join("lexicon.txt"), &mut map);
    read_lexicon_text(CORRECTIONS, &mut map);
    read_lexicon(&dir.join("user_lexicon.txt"), &mut map);

    let out = dir.join("lexicon.merged.txt");
    let mut text = String::with_capacity(2_000_000);
    for (word, pinyin) in &map {
        text.push_str(word);
        text.push(' ');
        text.push_str(pinyin);
        text.push('\n');
    }
    std::fs::write(&out, text)?;
    Ok(out)
}

/// 把单声道 f32 写成 16-bit WAV。
pub fn save_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> Result<()> {
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

/// 找 `<prefix>*.onnx`，有 int8 优先。
fn pick_onnx(dir: &std::path::Path, prefix: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("onnx"))
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();
    candidates
        .iter()
        .find(|path| path.to_string_lossy().contains(".int8."))
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

impl Tts {
    pub fn new(cfg: &TtsConfig) -> Result<Self> {
        let dir = &cfg.model_dir;
        let tokens = dir.join("tokens.txt");
        let mut reference = None;
        let mut zipvoice = false;
        let mut kind = "VITS";
        if !tokens.is_file() {
            bail!("TTS 模型文件缺失：{}（需要 tokens.txt）", tokens.display());
        }

        let mut config = OfflineTtsConfig::default();
        config.model.num_threads = cfg.num_threads;
        config.model.provider = Some(cfg.provider.clone());
        config.model.debug = cfg.debug;
        config.max_num_sentences = 2;

        let zip_encoder = pick_onnx(dir, "encoder");
        let zip_decoder = pick_onnx(dir, "decoder");
        if let (Some(encoder), Some(decoder)) = (zip_encoder, zip_decoder) {
            // ZipVoice：零样本克隆，中英，24 kHz 声码器
            let vocoder = pick_onnx(dir, "vocos").ok_or_else(|| {
                anyhow::anyhow!("ZipVoice 需要 vocos_24khz.onnx（{}）", dir.display())
            })?;
            let mut lexicons: Vec<String> = Vec::new();
            for name in ["lexicon.txt", "lexicon-en-us.txt"] {
                let path = dir.join(name);
                if path.is_file() {
                    lexicons.push(path.display().to_string());
                }
            }
            config.model.zipvoice = OfflineTtsZipvoiceModelConfig {
                tokens: Some(tokens.display().to_string()),
                encoder: Some(encoder.display().to_string()),
                decoder: Some(decoder.display().to_string()),
                vocoder: Some(vocoder.display().to_string()),
                lexicon: if lexicons.is_empty() {
                    None
                } else {
                    Some(lexicons.join(","))
                },
                feat_scale: 0.1,
                t_shift: 0.5,
                target_rms: 0.1,
                guidance_scale: 1.0,
                ..Default::default()
            };
            zipvoice = true;
            kind = "ZipVoice 克隆";
            reference = match (&cfg.reference_wav, &cfg.reference_text) {
                (Some(wav), Some(text)) if !text.trim().is_empty() => {
                    Some(load_reference(wav, text)?)
                }
                _ => bail!(
                    "ZipVoice 需要参考音频和参考文本（GABTWIN_TTS_REF_WAV / GABTWIN_TTS_REF_TEXT）"
                ),
            };
        } else if let Some(acoustic) = pick_onnx(dir, "model-steps") {
            kind = "Matcha";
            // Matcha（中英，16 kHz，配 vocos 声码器；非自回归，快）
            let vocoder = pick_onnx(dir, "vocos")
                .or_else(|| pick_onnx(dir, "vocoder"))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Matcha 需要声码器 vocos-16khz-univ.onnx（{}）",
                        dir.display()
                    )
                })?;
            let lexicon = merged_matcha_lexicon(dir)?;
            config.model.matcha.acoustic_model = Some(acoustic.display().to_string());
            config.model.matcha.vocoder = Some(vocoder.display().to_string());
            config.model.matcha.lexicon = Some(lexicon.display().to_string());
            config.model.matcha.tokens = Some(tokens.display().to_string());
        } else if dir.join("voices.bin").is_file() {
            kind = "Kokoro";
            // Kokoro（24 kHz 多语，中文音色从 sid 3 开始）
            let model = pick_onnx(dir, "model")
                .ok_or_else(|| anyhow::anyhow!("Kokoro 需要 model*.onnx（{}）", dir.display()))?;
            let voices = dir.join("voices.bin");
            config.model.kokoro.model = Some(model.display().to_string());
            config.model.kokoro.voices = Some(voices.display().to_string());
            config.model.kokoro.tokens = Some(tokens.display().to_string());
            let lexicons: Vec<String> = ["lexicon-zh.txt", "lexicon-us-en.txt"]
                .iter()
                .map(|name| dir.join(name))
                .filter(|path| path.is_file())
                .map(|path| path.display().to_string())
                .collect();
            if !lexicons.is_empty() {
                config.model.kokoro.lexicon = Some(lexicons.join(","));
            }
            config.model.kokoro.lang = Some("zh".to_string());
        } else {
            // VITS（16 kHz 单音色）
            let model = dir.join("model.onnx");
            let lexicon = dir.join("lexicon.txt");
            for (path, hint) in [(&model, "model.onnx"), (&lexicon, "lexicon.txt")] {
                if !path.is_file() {
                    bail!(
                        "TTS 模型文件缺失：{}\n提示：需要 {hint}（见 docs/dev/04-tts.md）",
                        path.display()
                    );
                }
            }
            config.model.vits.model = Some(model.display().to_string());
            config.model.vits.lexicon = Some(lexicon.display().to_string());
            config.model.vits.tokens = Some(tokens.display().to_string());
        }

        // 数字、日期、多音字规则；有就挂上。
        let fsts: Vec<String> = [
            "date.fst",
            "date-zh.fst",
            "number.fst",
            "number-zh.fst",
            "phone.fst",
            "phone-zh.fst",
            "new_heteronym.fst",
        ]
        .iter()
        .map(|name| dir.join(name))
        .filter(|path| path.is_file())
        .map(|path| path.display().to_string())
        .collect();
        if !fsts.is_empty() {
            config.rule_fsts = Some(fsts.join(","));
        }

        let inner = OfflineTts::create(&config)
            .with_context(|| format!("创建 TTS 失败：{}", dir.display()))?;
        Ok(Self {
            inner,
            cfg: cfg.clone(),
            reference,
            zipvoice,
            kind,
        })
    }

    pub fn engine_name(&self) -> &'static str {
        self.kind
    }

    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate() as u32
    }

    /// 合成一句，返回单声道 f32 采样（已做 80 Hz 高通与边缘淡入淡出）。
    pub fn synthesize(&self, text: &str) -> Result<Vec<f32>> {
        let mut generation = GenerationConfig {
            sid: self.cfg.speaker_id,
            speed: self.cfg.speed,
            ..Default::default()
        };
        if let Some(reference) = &self.reference {
            generation.reference_audio = Some(reference.samples.clone());
            generation.reference_sample_rate = reference.sample_rate as i32;
            generation.reference_text = Some(reference.text.clone());
        }
        // ZipVoice 默认 2 步：本机 RTF<1，能边合成边播；4 步质量更好但 RTF≈1.9。
        let steps = if self.cfg.num_steps > 0 {
            self.cfg.num_steps
        } else if self.zipvoice {
            2
        } else {
            0
        };
        if steps > 0 {
            generation.num_steps = steps;
        }
        let audio = self
            .inner
            .generate_with_config(text, &generation, None::<fn(&[f32], f32) -> bool>)
            .ok_or_else(|| anyhow::anyhow!("TTS 合成失败：{text}"))?;
        let mut samples = audio.samples().to_vec();
        cleanup(&mut samples, self.sample_rate());
        Ok(samples)
    }
}

/// 去掉低频"咚咚"（80 Hz 高通）并在首尾做 15 ms 淡入淡出，避免拼接处的爆音。
pub fn cleanup(samples: &mut [f32], sample_rate: u32) {
    if samples.is_empty() || sample_rate == 0 {
        return;
    }
    // RBJ 2nd-order high-pass, Q=0.707
    let fs = sample_rate as f32;
    let f0 = 80.0f32;
    let w0 = 2.0 * std::f32::consts::PI * f0 / fs;
    let cos = w0.cos();
    let alpha = w0.sin() / (2.0 * 0.707);
    let a0 = 1.0 + alpha;
    let b0 = (1.0 + cos) / 2.0 / a0;
    let b1 = -(1.0 + cos) / a0;
    let b2 = (1.0 + cos) / 2.0 / a0;
    let a1 = -2.0 * cos / a0;
    let a2 = (1.0 - alpha) / a0;
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for sample in samples.iter_mut() {
        let x0 = *sample;
        let y0 = b0 * x0 + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        x2 = x1;
        x1 = x0;
        y2 = y1;
        y1 = y0;
        *sample = y0;
    }
    // 峰值归一：太轻的提上来，接近 1.0 的压到 0.9，避免削波
    let peak = samples.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
    if peak > 0.0001 {
        let gain = (0.9 / peak).min(2.5);
        for sample in samples.iter_mut() {
            *sample = (*sample * gain).clamp(-1.0, 1.0);
        }
    }
    // 15 ms 淡入/淡出
    let fade = ((sample_rate as f32 * 0.015) as usize).min(samples.len() / 2);
    for i in 0..fade {
        let gain = i as f32 / fade as f32;
        samples[i] *= gain;
        let last = samples.len() - 1 - i;
        samples[last] *= gain;
    }
}

/// 把 LLM 的流式增量切成适合合成的短句。
pub struct ClauseSplitter {
    buffer: String,
    /// 太短、不值得单独合成的小片段（比如强行切出来的"说。"），攒到下一句一起合成。
    pending: String,
    first: bool,
    /// 首句最多合成的字数（越小出声越快）。
    first_cap: usize,
}

impl Default for ClauseSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl ClauseSplitter {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            pending: String::new(),
            first: true,
            first_cap: 30,
        }
    }

    pub fn with_first_cap(cap: usize) -> Self {
        Self {
            first_cap: cap.max(6),
            ..Self::new()
        }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.pending.clear();
        self.first = true;
    }

    /// 喂一段流式文本，返回已经可以合成的短句。
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buffer.push_str(delta);
        self.buffer = clean(&self.buffer);

        let mut out = Vec::new();
        while let Some(cut) = find_split(&self.buffer, self.first, self.first_cap) {
            let clause = self.buffer[..cut].trim().to_string();
            self.buffer = self.buffer[cut..].trim_start().to_string();
            if !is_speakable(&clause) {
                continue;
            }
            let mut clause = if self.pending.is_empty() {
                clause
            } else {
                let mut merged = std::mem::take(&mut self.pending);
                merged.push_str(&clause);
                merged
            };
            if clause.chars().count() < 3 && self.pending.chars().count() < 12 {
                self.pending = clause;
                continue;
            }
            if clause.chars().count() >= 3 {
                self.first = false;
                out.push(std::mem::take(&mut clause));
            }
        }
        out
    }

    /// 文本结束时把余下部分交出来。
    pub fn flush(&mut self) -> Option<String> {
        let rest = self.buffer.trim().to_string();
        self.buffer.clear();
        let mut text = std::mem::take(&mut self.pending);
        text.push_str(&rest);
        let text = text.trim().to_string();
        if is_speakable(&text) {
            self.first = false;
            Some(text)
        } else {
            None
        }
    }
}

/// 流式首句切分：只切出第一句，之后全部攒着，等 LLM 结束后一次送出。
///
/// 目的：LLM 一边输出，TTS 先把第一句念出来；剩余整段仍然一次合成，
/// 整条回复最多一个拼接点，而不是每句一个。
pub struct OverlapSplitter {
    buffer: String,
    first: Option<String>,
    max_first: usize,
    /// 第一段至少攒这么多字再送 TTS；避免把“狸猫！”这种短感叹单独合成成一声吼。
    min_first: usize,
}

impl OverlapSplitter {
    pub fn new(max_first: usize) -> Self {
        Self::with_first_min(max_first, 12)
    }

    pub fn with_first_min(max_first: usize, min_first: usize) -> Self {
        Self {
            buffer: String::new(),
            first: None,
            max_first: max_first.max(12),
            min_first: min_first.max(3),
        }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.first = None;
    }

    /// 喂一段流式文本。只有第一句会返回；之后返回 None，直到 finish。
    pub fn push(&mut self, delta: &str) -> Option<String> {
        self.buffer.push_str(delta);
        self.buffer = clean(&self.buffer);
        if self.first.is_some() {
            return None;
        }
        let cut = find_first_cut(&self.buffer, self.max_first, self.min_first)?;
        let clause = self.buffer[..cut].trim().to_string();
        let rest = self.buffer[cut..].trim_start().to_string();
        if !is_speakable(&clause) {
            self.buffer = rest;
            return None;
        }
        self.buffer = rest;
        self.first = Some(clause.clone());
        Some(clause)
    }

    /// LLM 结束：返回第一句之后的所有内容；如果第一句还没切出来，返回整段。
    pub fn finish(&mut self, full: &str) -> Option<String> {
        let full = full.trim();
        let rest = if let Some(first) = &self.first {
            if let Some(rest) = full.strip_prefix(first.as_str()) {
                rest.trim().to_string()
            } else if let Some(pos) = full.find(first.as_str()) {
                full[pos + first.len()..].trim().to_string()
            } else {
                self.buffer.trim().to_string()
            }
        } else {
            full.to_string()
        };
        self.buffer.clear();
        self.first = None;
        if is_speakable(&rest) {
            Some(rest)
        } else {
            None
        }
    }
}

/// 找第一句的切点：句末标点优先；超过 max_first 才退到最后一个逗号。
fn find_first_cut(text: &str, max_first: usize, min_first: usize) -> Option<usize> {
    let mut chars = 0usize;
    let mut depth = 0i32;
    let mut last_comma: Option<usize> = None;

    for (idx, c) in text.char_indices() {
        chars += 1;
        let end = idx + c.len_utf8();
        match c {
            '「' | '『' | '“' | '‘' | '（' | '(' | '《' | '【' | '[' => depth += 1,
            '」' | '』' | '”' | '’' | '）' | ')' | '》' | '】' | ']' => {
                depth = (depth - 1).max(0)
            }
            _ => {}
        }
        if depth == 0 && matches!(c, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            // 太短的感叹/招呼不单独成段，接着攒到下一句；否则 Breeze 会把
            // 孤立的“狸猫！”合成成一声喊。
            if chars >= min_first {
                return Some(end);
            }
            continue;
        }
        if depth == 0 && matches!(c, '，' | ',' | '、' | '：' | ':') {
            last_comma = Some(end);
        }
        if chars >= max_first {
            if depth == 0 {
                // 下一个字符就是句末标点：让它把这一句读完。
                let next_is_end = text[end..]
                    .chars()
                    .next()
                    .map(|n| matches!(n, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
                    .unwrap_or(false);
                if next_is_end {
                    continue;
                }
                return last_comma
                    .filter(|cut| text[..*cut].chars().count() >= max_first / 2)
                    .or(Some(end));
            }
            if chars >= max_first + 20 {
                return Some(end);
            }
        }
    }
    None
}

fn is_speakable(text: &str) -> bool {
    text.chars().any(|c| c.is_alphanumeric() || is_han(c))
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

/// 去掉 Markdown 记号；把感叹号降成逗号，避免 Breeze 把短感叹合成一声吼。
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev = '\0';
    for c in text
        .chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '~' | '|' | '>' | '\r'))
    {
        let c = match c {
            '！' | '!' => '，',
            _ => c,
        };
        if c == '，' && prev == '，' {
            continue;
        }
        out.push(c);
        prev = c;
    }
    out
}

/// 送 TTS 前的文本清理：感叹号全部降为逗号，结尾补句号。
///
/// 这是“情绪平稳”的关键：Breeze 对 `！` 非常敏感，孤立的“狸猫！”会直接
/// 变成 400+ Hz 的一声喊；改成“狸猫，”/“狸猫。”后回到正常语调。
pub fn sanitize_for_tts(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev = '\0';
    for c in text.chars() {
        let c = match c {
            '！' | '!' => '，',
            '\r' => continue,
            _ => c,
        };
        if c == '，' && prev == '，' {
            continue;
        }
        out.push(c);
        prev = c;
    }
    let mut result = out.trim().to_string();
    while result.ends_with('，') || result.ends_with(',') || result.ends_with('、') {
        result.pop();
    }
    if !result.is_empty()
        && !result.ends_with('。')
        && !result.ends_with('？')
        && !result.ends_with('?')
    {
        result.push('。');
    }
    result
}

/// 找到第一个合适的分句点（返回字节下标，切点在标点之后）。
///
/// 原则：**句子优先**。句末标点（引号外）最自然；等不及了退到范围内最后一个逗号；
/// 再不行才硬切。引号里的内容会多等一会儿，等引号闭合再切。
fn find_split(text: &str, first: bool, first_cap: usize) -> Option<usize> {
    let hard_max = if first { first_cap } else { 40 };
    let comma_min = if first { 8 } else { 12 };

    let mut chars = 0usize;
    let mut depth = 0i32;
    let mut sentence: Option<usize> = None;
    let mut hard: Option<usize> = None;

    for (idx, c) in text.char_indices() {
        chars += 1;
        let end = idx + c.len_utf8();
        match c {
            '「' | '『' | '“' | '‘' | '（' | '(' | '《' | '【' | '[' => depth += 1,
            '」' | '』' | '”' | '’' | '）' | ')' | '》' | '】' | ']' => {
                depth = (depth - 1).max(0)
            }
            _ => {}
        }
        // 句末标点只认引号外的。
        if depth == 0 && matches!(c, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            sentence = Some(end);
            break;
        }
        // 标点级断句：逗号够长、且不在引号内，就直接切。
        if depth == 0
            && matches!(c, '，' | ',' | '、' | '：' | ':')
            && chars >= comma_min
        {
            return Some(end);
        }
        if chars >= hard_max {
            if depth == 0 {
                // 引号刚好在硬上限处闭合：先看下一个字符是不是句末标点，
                // 是的话别在这里硬切，让句号完成这一句。
                let next_is_sentence_end = text[end..]
                    .chars()
                    .next()
                    .map(|n| matches!(n, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n'))
                    .unwrap_or(false);
                if next_is_sentence_end {
                    continue;
                }
                hard = Some(end);
                break;
            }
            // 引号里：最多再等 40 字，等它闭合。
            if chars >= hard_max + 40 {
                hard = Some(end);
                break;
            }
        }
    }

    // 流式输入：还没到句末、也没到硬上限时，不要拿一个逗号就切——
    // 后面可能还有半句。等到句末，或者攒够 hard_max 再退到最后一个逗号。
    if sentence.is_some() {
        return sentence;
    }
    hard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_a_natural_boundary() {
        let mut s = ClauseSplitter::new();
        // 没有句末标点、也没到 26 字：先攒着，不硬切。
        let out = s.push("嗯，我觉得这个事情");
        assert!(out.is_empty());
        assert_eq!(s.flush().unwrap(), "嗯，我觉得这个事情");
    }

    #[test]
    fn later_clauses_wait_for_punctuation() {
        let mut s = ClauseSplitter::new();
        // 句末标点优先，切在"。"之后
        let out = s.push("我们先看看再说。后面还有");
        assert_eq!(out, vec!["我们先看看再说。".to_string()]);
        let rest = s.flush().unwrap();
        assert_eq!(rest, "后面还有");
    }

    #[test]
    fn strips_markdown() {
        let mut s = ClauseSplitter::new();
        s.push("**你好**");
        assert_eq!(s.flush().unwrap(), "你好");
    }

    #[test]
    fn waits_through_a_short_comma() {
        let mut s = ClauseSplitter::new();
        let out = s.push("嗯，我听到了，");
        assert!(out.is_empty());
        let out = s.push("后面还有。");
        assert_eq!(out, vec!["嗯，我听到了，后面还有。".to_string()]);
    }

    #[test]
    fn long_first_clause_falls_back_to_last_comma() {
        let mut s = ClauseSplitter::with_first_cap(40);
        let text = "一二三四五六七八九十，十一十二十三十四十五十六，十七十八十九二十二一二二二三二四二五二六二七二八二九三十三一三二";
        let out = s.push(text);
        assert!(!out.is_empty());
        assert!(out[0].ends_with('，'));
        assert!(out[0].chars().count() >= 8);
    }


    #[test]
    fn overlap_waits_for_a_full_sentence() {
        let mut s = OverlapSplitter::new(30);
        assert!(s.push("我刚刷到一个视频，").is_none());
        let first = s.push("讲的是一个人。").expect("first sentence");
        assert_eq!(first, "我刚刷到一个视频，讲的是一个人。");
        assert!(s.push("后面还有。").is_none());
        let rest = s.finish("我刚刷到一个视频，讲的是一个人。后面还有。").unwrap();
        assert_eq!(rest, "后面还有。");
    }

    #[test]
    fn overlap_caps_a_long_sentence_at_last_comma() {
        let mut s = OverlapSplitter::new(12);
        let first = s.push("一二三四五六七八，九十十一十二").expect("first chunk");
        assert_eq!(first, "一二三四五六七八，");
    }

    #[test]
    fn overlap_streaming_is_lossless() {
        let text = "我刚刷到一个视频，讲的是一个人用三年时间把后院改成了植物园。后面还有一句。";
        let mut s = OverlapSplitter::new(30);
        let mut first = None;
        for ch in text.chars() {
            let out = s.push(&ch.to_string());
            if first.is_none() {
                first = out;
            }
        }
        let rest = s.finish(text).unwrap();
        assert_eq!(format!("{}{}", first.unwrap(), rest), text);
    }

    #[test]
    fn overlap_short_reply_stays_whole() {
        let mut s = OverlapSplitter::new(30);
        assert!(s.push("你好。").is_none());
        assert_eq!(s.finish("你好。").unwrap(), "你好。");
    }

    #[test]
    fn sanitize_tames_exclamations() {
        assert_eq!(sanitize_for_tts("狸猫！"), "狸猫。");
        assert_eq!(sanitize_for_tts("狸猫！那种特别瘦的野猫，"), "狸猫，那种特别瘦的野猫。");
        assert_eq!(sanitize_for_tts("太好了！！！"), "太好了。");
        assert_eq!(sanitize_for_tts("怎么办？"), "怎么办？");
    }

    #[test]
    fn overlap_does_not_isolate_a_short_exclamation() {
        let mut s = OverlapSplitter::new(30);
        assert!(s.push("狸猫！").is_none());
        assert!(s.push("那种特别瘦的野猫，").is_none());
        let first = s.push("眼睛特别大。").expect("first chunk");
        assert!(first.starts_with("狸猫，"));
        assert!(first.chars().count() > 8);
    }

    #[test]
    fn does_not_split_inside_quotes() {
        let mut s = ClauseSplitter::new();
        let text = "你刚才说的是「对我做了介绍啊，那么我想说的是呢，大家如果对我的研究感兴趣呢」。";
        let out = s.push(text);
        assert_eq!(out, vec![text.to_string()]);
    }
}
