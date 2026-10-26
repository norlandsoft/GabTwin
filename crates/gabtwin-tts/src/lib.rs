//! 本地 TTS：sherpa-onnx VITS（中文）+ 口语文本切分。
//!
//! 切分的目标不是"句子正确"，而是**首句尽量短**：LLM 的流式回复一到，
//! 只要凑出一个能听懂的小句就先合成播放，后面的句子继续合成。

use anyhow::{bail, Context, Result};
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub model_dir: PathBuf,
    pub num_threads: i32,
    pub speaker_id: i32,
    pub speed: f32,
    /// onnxruntime provider：cpu / coreml（macOS）
    pub provider: String,
    pub debug: bool,
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
        }
    }
}

pub struct Tts {
    inner: OfflineTts,
    cfg: TtsConfig,
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
        if !tokens.is_file() {
            bail!("TTS 模型文件缺失：{}（需要 tokens.txt）", tokens.display());
        }

        let mut config = OfflineTtsConfig::default();
        config.model.num_threads = cfg.num_threads;
        config.model.provider = Some(cfg.provider.clone());
        config.model.debug = cfg.debug;
        config.max_num_sentences = 2;

        if let Some(acoustic) = pick_onnx(dir, "model-steps") {
            // Matcha（中英，16 kHz，配 vocos 声码器；非自回归，快）
            let vocoder = pick_onnx(dir, "vocos")
                .or_else(|| pick_onnx(dir, "vocoder"))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Matcha 需要声码器 vocos-16khz-univ.onnx（{}）",
                        dir.display()
                    )
                })?;
            let lexicon = dir.join("lexicon.txt");
            config.model.matcha.acoustic_model = Some(acoustic.display().to_string());
            config.model.matcha.vocoder = Some(vocoder.display().to_string());
            if lexicon.is_file() {
                config.model.matcha.lexicon = Some(lexicon.display().to_string());
            }
            config.model.matcha.tokens = Some(tokens.display().to_string());
        } else if dir.join("voices.bin").is_file() {
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
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate() as u32
    }

    /// 合成一句，返回单声道 f32 采样。
    pub fn synthesize(&self, text: &str) -> Result<Vec<f32>> {
        let generation = GenerationConfig {
            sid: self.cfg.speaker_id,
            speed: self.cfg.speed,
            ..Default::default()
        };
        let audio = self
            .inner
            .generate_with_config(text, &generation, None::<fn(&[f32], f32) -> bool>)
            .ok_or_else(|| anyhow::anyhow!("TTS 合成失败：{text}"))?;
        Ok(audio.samples().to_vec())
    }
}

/// 把 LLM 的流式增量切成适合合成的短句。
pub struct ClauseSplitter {
    buffer: String,
    /// 太短、不值得单独合成的小片段（比如强行切出来的"说。"），攒到下一句一起合成。
    pending: String,
    first: bool,
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
        while let Some(cut) = find_split(&self.buffer, self.first) {
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

fn is_speakable(text: &str) -> bool {
    text.chars().any(|c| c.is_alphanumeric() || is_han(c))
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

/// 去掉 Markdown 记号；保留标点。
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '~' | '|' | '>' | '\r'))
        .collect()
}

/// 找到第一个合适的分句点（返回字节下标，切点在标点之后）。
fn find_split(text: &str, first: bool) -> Option<usize> {
    // 首句越短，出声越快：6 个字大约是 0.5 s 音频、300–500 ms 合成。
    let min_soft = if first { 4 } else { 10 };
    let max_len = if first { 6 } else { 30 };

    let mut chars = 0usize;
    let mut soft: Option<usize> = None;
    let mut hard: Option<usize> = None;

    for (idx, c) in text.char_indices() {
        chars += 1;
        let end = idx + c.len_utf8();
        if matches!(c, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            hard = Some(end);
            break;
        }
        if matches!(c, '，' | ',' | '、' | '：' | ':') && chars >= min_soft && soft.is_none() {
            soft = Some(end);
        }
        if chars >= max_len {
            break;
        }
    }

    hard.or(soft).or_else(|| {
        if chars >= max_len {
            text.char_indices()
                .nth(max_len - 1)
                .map(|(idx, c)| idx + c.len_utf8())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_first_clause_short() {
        let mut s = ClauseSplitter::new();
        let out = s.push("嗯，我觉得这个事情");
        assert_eq!(out, vec!["嗯，我觉得这".to_string()]);
        let rest = s.flush().unwrap();
        assert_eq!(rest, "个事情");
    }

    #[test]
    fn later_clauses_wait_for_punctuation() {
        let mut s = ClauseSplitter::new();
        // 首句按 6 字硬切，之后按标点切
        let out = s.push("我们先看看再说。后面还有");
        assert_eq!(out, vec!["我们先看看再".to_string()]);
        let rest = s.flush().unwrap();
        assert_eq!(rest, "说。后面还有");
    }

    #[test]
    fn strips_markdown() {
        let mut s = ClauseSplitter::new();
        s.push("**你好**");
        assert_eq!(s.flush().unwrap(), "你好");
    }

    #[test]
    fn hard_cap_for_first_clause() {
        let mut s = ClauseSplitter::new();
        let out = s.push("一二三四五六七八九十一二三四五");
        assert_eq!(out.len(), 1);
        assert!(out[0].chars().count() <= 6);
    }
}
