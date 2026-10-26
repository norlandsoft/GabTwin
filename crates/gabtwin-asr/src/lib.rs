//! 流式语音识别。
//!
//! 只暴露"喂音频 → 拿增量文本"这一件事，不关心音频从哪来、文本去哪。
//! 端点判定用 sherpa-onnx 自带规则（rule1 = 静音多少秒算一句话结束）。

use anyhow::{bail, Context, Result};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrKind {
    /// 自动按目录里的文件判断。
    Auto,
    /// transducer：encoder + decoder + joiner（如 streaming zipformer）。
    Transducer,
    /// CTC：单个 model.onnx（如 streaming zipformer2 CTC）。
    Ctc,
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
}

impl AsrConfig {
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            model_dir: model_dir.into(),
            num_threads: 2,
            endpoint_silence_s: 0.8,
            kind: AsrKind::Auto,
        }
    }
}

/// 在目录里按顺序找第一个存在的文件。
fn pick(dir: &std::path::Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
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

        let encoder = pick(dir, &["encoder.int8.onnx", "encoder.onnx"]);
        let decoder = pick(dir, &["decoder.int8.onnx", "decoder.onnx"]);
        let joiner = pick(dir, &["joiner.int8.onnx", "joiner.onnx"]);
        let ctc = pick(dir, &["model.int8.onnx", "model.onnx"]);

        let kind = match cfg.kind {
            AsrKind::Auto => {
                if encoder.is_some() {
                    AsrKind::Transducer
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
            AsrKind::Auto => unreachable!(),
        }
        config.model_config.tokens = Some(tokens.display().to_string());
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.provider = Some("cpu".to_string());

        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = cfg.endpoint_silence_s;
        config.rule2_min_trailing_silence = 1.2;
        config.rule3_min_utterance_length = 20.0;
        config.decoding_method = Some("greedy_search".to_string());

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
