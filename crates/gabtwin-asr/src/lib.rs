//! 流式语音识别。
//!
//! 只暴露"喂音频 → 拿增量文本"这一件事，不关心音频从哪来、文本去哪。
//! 端点判定用 sherpa-onnx 自带规则（rule1 = 静音多少秒算一句话结束）。

use anyhow::{bail, Context, Result};
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OnlineRecognizer, OnlineRecognizerConfig,
    OnlineStream,
};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrKind {
    /// 自动按目录里的文件判断。
    Auto,
    /// transducer：encoder + decoder + joiner（如 streaming zipformer）。
    Transducer,
    /// CTC：单个 model.onnx（如 streaming zipformer2 CTC）。
    Ctc,
    /// paraformer：encoder + decoder，没有 joiner。
    Paraformer,
}

#[derive(Debug, Clone)]
pub struct AsrConfig {
    /// 模型目录。transducer 需要 encoder/decoder/joiner + tokens.txt；
    /// CTC 需要 model.int8.onnx + tokens.txt。
    pub model_dir: PathBuf,
    pub num_threads: i32,
    /// 句尾静音多久算一句话结束（秒）。
    pub endpoint_silence_s: f32,
    pub kind: AsrKind,
    /// 用 modified_beam_search（比 greedy 略准、略慢）。
    pub beam_search: bool,
    /// beam search 的活跃路径数。
    pub max_active_paths: i32,
    /// 负值鼓励输出 blank，可抑制重复字。
    pub blank_penalty: f32,
}

impl AsrConfig {
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            num_threads: 2,
            endpoint_silence_s: 0.8,
            kind: AsrKind::Auto,
            beam_search: false,
            max_active_paths: 4,
            blank_penalty: 0.0,
        }
    }
}

/// 在目录里找指定前缀的 .onnx；有 int8 版本优先。
/// 兼容 `encoder-epoch-99-avg-1.int8.onnx` 这类命名。
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

/// 一次增量识别结果。
#[derive(Debug, Clone, PartialEq)]
pub enum AsrUpdate {
    /// 当前这句话的完整假设（会不断被改写，不是追加）。
    Partial(String),
    /// 端点确认，这句话定稿。
    Segment(String),
}

pub struct Asr {
    recognizer: OnlineRecognizer,
    stream: OnlineStream,
    last_text: String,
}

impl Asr {
    pub fn new(cfg: &AsrConfig) -> Result<Self> {
        let dir = &cfg.model_dir;
        let tokens = dir.join("tokens.txt");
        if !tokens.is_file() {
            bail!(
                "模型文件缺失：{}\n提示：见 docs/dev/01-voice-input.md 与 scripts/fetch-model.sh",
                tokens.display()
            );
        }

        let encoder = pick_onnx(dir, "encoder");
        let decoder = pick_onnx(dir, "decoder");
        let joiner = pick_onnx(dir, "joiner");
        let ctc = pick_onnx(dir, "model");

        let kind = match cfg.kind {
            AsrKind::Auto => {
                if encoder.is_some() && joiner.is_some() {
                    AsrKind::Transducer
                } else if encoder.is_some() && decoder.is_some() {
                    AsrKind::Paraformer
                } else if ctc.is_some() {
                    AsrKind::Ctc
                } else {
                    bail!(
                        "在 {} 里既没找到 encoder*.onnx，也没找到 model*.onnx",
                        dir.display()
                    );
                }
            }
            other => other,
        };

        let mut config = OnlineRecognizerConfig::default();
        match kind {
            AsrKind::Transducer => {
                let (encoder, decoder, joiner) = match (encoder, decoder, joiner) {
                    (Some(e), Some(d), Some(j)) => (e, d, j),
                    _ => bail!(
                        "transducer 模型需要 encoder / decoder / joiner 三个文件（{}）",
                        dir.display()
                    ),
                };
                config.model_config.transducer.encoder = Some(encoder.display().to_string());
                config.model_config.transducer.decoder = Some(decoder.display().to_string());
                config.model_config.transducer.joiner = Some(joiner.display().to_string());
            }
            AsrKind::Ctc => {
                let ctc = ctc.ok_or_else(|| {
                    anyhow::anyhow!("CTC 模型需要 model.int8.onnx（{}）", dir.display())
                })?;
                config.model_config.zipformer2_ctc.model = Some(ctc.display().to_string());
            }
            AsrKind::Paraformer => {
                let (encoder, decoder) = match (encoder, decoder) {
                    (Some(e), Some(d)) => (e, d),
                    _ => bail!(
                        "paraformer 模型需要 encoder / decoder 两个文件（{}）",
                        dir.display()
                    ),
                };
                config.model_config.paraformer.encoder = Some(encoder.display().to_string());
                config.model_config.paraformer.decoder = Some(decoder.display().to_string());
            }
            AsrKind::Auto => unreachable!(),
        }
        config.model_config.tokens = Some(tokens.display().to_string());
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.provider = Some("cpu".to_string());

        config.blank_penalty = cfg.blank_penalty;
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = cfg.endpoint_silence_s;
        config.rule2_min_trailing_silence = 1.2;
        config.rule3_min_utterance_length = 20.0;
        if cfg.beam_search {
            config.decoding_method = Some("modified_beam_search".to_string());
            config.max_active_paths = cfg.max_active_paths.max(1);
        } else {
            config.decoding_method = Some("greedy_search".to_string());
        }

        let recognizer = OnlineRecognizer::create(&config)
            .with_context(|| format!("创建流式识别器失败：{}", dir.display()))?;
        let stream = recognizer.create_stream();

        Ok(Self {
            recognizer,
            stream,
            last_text: String::new(),
        })
    }

    /// 传入任意采样率的单声道音频；sherpa-onnx 内部重采样到 16 kHz。
    pub fn accept(&mut self, sample_rate: u32, samples: &[f32]) -> Vec<AsrUpdate> {
        let mut out = Vec::new();
        self.stream.accept_waveform(sample_rate as i32, samples);

        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);

            let text = self
                .recognizer
                .get_result(&self.stream)
                .map(|r| r.text)
                .unwrap_or_default();

            if !text.is_empty() && text != self.last_text {
                self.last_text = text.clone();
                out.push(AsrUpdate::Partial(text));
            }

            if self.recognizer.is_endpoint(&self.stream) {
                let final_text = self
                    .recognizer
                    .get_result(&self.stream)
                    .map(|r| r.text)
                    .unwrap_or_default();
                if !final_text.trim().is_empty() {
                    out.push(AsrUpdate::Segment(final_text.trim().to_string()));
                }
                self.recognizer.reset(&self.stream);
                self.last_text.clear();
            }
        }

        out
    }

    /// 丢掉当前这句话的假设，重新开始（用于播放期间屏蔽输入）。
    pub fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
        self.last_text.clear();
    }

    /// 音频结束：把尾部还没解码的部分冲出来。
    pub fn finish(&mut self) -> Vec<AsrUpdate> {
        self.stream.input_finished();
        let mut out = Vec::new();
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
            if self.recognizer.is_endpoint(&self.stream) {
                break;
            }
        }
        let text = self
            .recognizer
            .get_result(&self.stream)
            .map(|r| r.text)
            .unwrap_or_default();
        if !text.trim().is_empty() {
            out.push(AsrUpdate::Segment(text.trim().to_string()));
        }
        self.recognizer.reset(&self.stream);
        self.last_text.clear();
        out
    }
}

/// 离线识别（SenseVoice）：一次喂完整段音频，拿最终文本。
/// 用于"流式出字幕 + 句末用更强模型定稿"的组合。
#[derive(Debug, Clone)]
pub struct OfflineAsrConfig {
    pub model_dir: PathBuf,
    pub num_threads: i32,
    /// None = auto（模型自带语言识别）
    pub language: Option<String>,
    pub use_itn: bool,
}

impl OfflineAsrConfig {
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            num_threads: 2,
            language: None,
            use_itn: true,
        }
    }
}

pub struct OfflineAsr {
    recognizer: OfflineRecognizer,
}

impl OfflineAsr {
    pub fn new(cfg: &OfflineAsrConfig) -> Result<Self> {
        let dir = &cfg.model_dir;
        let model = pick_onnx(dir, "model").ok_or_else(|| {
            anyhow::anyhow!(
                "找不到 model*.onnx（{}）\n提示：SenseVoice 需要 model.int8.onnx + tokens.txt",
                dir.display()
            )
        })?;
        let tokens = dir.join("tokens.txt");
        if !tokens.is_file() {
            bail!("模型文件缺失：{}", tokens.display());
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice.model = Some(model.display().to_string());
        config.model_config.sense_voice.language = cfg.language.clone();
        config.model_config.sense_voice.use_itn = cfg.use_itn;
        config.model_config.tokens = Some(tokens.display().to_string());
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.provider = Some("cpu".to_string());

        let recognizer = OfflineRecognizer::create(&config)
            .with_context(|| format!("创建离线识别器失败：{}", dir.display()))?;
        Ok(Self { recognizer })
    }

    pub fn transcribe(&self, sample_rate: u32, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(sample_rate as i32, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }
}
