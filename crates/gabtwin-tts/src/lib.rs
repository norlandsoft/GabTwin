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
///
/// 原则：**句子优先**。句末标点（引号外）最自然；等不及了退到范围内最后一个逗号；
/// 再不行才硬切。引号里的内容会多等一会儿，等引号闭合再切。
fn find_split(text: &str, first: bool) -> Option<usize> {
    let hard_max = if first { 40 } else { 60 };
    let comma_min = if first { 12 } else { 20 };

    let mut chars = 0usize;
    let mut depth = 0i32;
    let mut comma: Option<usize> = None;
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
        // 逗号取范围内最后一个，切得尽量靠后。
        if matches!(c, '，' | ',' | '、' | '：' | ':') && chars >= comma_min {
            comma = Some(end);
        }
        if chars >= hard_max {
            if depth == 0 {
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
    if hard.is_some() {
        return comma.or(hard);
    }
    None
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
        let mut s = ClauseSplitter::new();
        let text = "一二三四五六七八九十，十一十二十三十四十五十六，十七十八十九二十二一二二二三二四二五二六二七二八二九三十三一三二";
        let out = s.push(text);
        assert_eq!(out.len(), 1);
        // 切在范围内最后一个逗号之后，而不是第一个。
        assert!(out[0].ends_with('，'));
        assert!(out[0].chars().count() >= 20);
    }

    #[test]
    fn does_not_split_inside_quotes() {
        let mut s = ClauseSplitter::new();
        let text = "你刚才说的是「对我做了介绍啊，那么我想说的是呢，大家如果对我的研究感兴趣呢」。";
        let out = s.push(text);
        assert_eq!(out, vec![text.to_string()]);
    }
}
