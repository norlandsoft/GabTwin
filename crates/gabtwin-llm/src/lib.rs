//! LLM 流式客户端 + 对话上下文。
//!
//! 只做三件事：把消息发出去、把 delta 流式吐回来、把上下文裁到预算内。
//! 不负责 UI、不负责 TTS；调用方决定什么时候开一轮、什么时候取消。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// 例如 https://api.deepseek.com
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl LlmConfig {
    /// 从环境变量读；没有 key 返回 None。
    ///   DEEPSEEK_API_KEY / GABTWIN_LLM_KEY
    ///   GABTWIN_LLM_BASE（默认 https://api.deepseek.com）
    ///   GABTWIN_LLM_MODEL（默认 deepseek-chat）
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("GABTWIN_LLM_KEY")
            .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
            .ok()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self {
            base_url: std::env::var("GABTWIN_LLM_BASE")
                .unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
            api_key,
            model: std::env::var("GABTWIN_LLM_MODEL")
                .unwrap_or_else(|_| "deepseek-chat".to_string()),
            max_tokens: std::env::var("GABTWIN_LLM_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(512),
            temperature: 0.8,
        })
    }
}

#[derive(Debug, Default)]
pub struct StreamResult {
    pub text: String,
    pub cancelled: bool,
    /// 从发请求到第一个非空 delta 的毫秒数。
    pub first_token_ms: Option<u64>,
    /// 从发请求到流结束的毫秒数。
    pub total_ms: u64,
    /// stop / length / ...
    pub finish_reason: Option<String>,
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::blocking::Client,
    cfg: LlmConfig,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .context("创建 HTTP 客户端失败")?;
        Ok(Self { http, cfg })
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    /// 流式对话。每收到一个非空 delta 就调用 `on_delta`；
    /// `cancel` 置位后在下一个数据行处停下，已收到的文本仍会返回。
    /// 启动时预热：用同一段 system prompt 发一个极小请求，
    /// 让 server 完成权重换入与 system 前缀的 prompt cache。
    pub fn prewarm(&self, system: &str) -> Result<()> {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let _: serde_json::Value = self
            .http
            .post(url)
            .bearer_auth(&self.cfg.api_key)
            .json(&serde_json::json!({
                "model": self.cfg.model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": "你好"}
                ],
                "max_tokens": 1,
                "stream": false
            }))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(())
    }

    pub fn stream_chat<F>(
        &self,
        messages: &[Message],
        cancel: &AtomicBool,
        on_delta: F,
    ) -> Result<StreamResult>
    where
        F: FnMut(&str),
    {
        self.stream_chat_with(
            messages,
            cancel,
            self.cfg.temperature,
            self.cfg.max_tokens,
            on_delta,
        )
    }

    /// 与 `stream_chat` 相同，但允许覆盖 temperature / max_tokens（校对用低温度）。
    pub fn stream_chat_with<F>(
        &self,
        messages: &[Message],
        cancel: &AtomicBool,
        temperature: f32,
        max_tokens: u32,
        mut on_delta: F,
    ) -> Result<StreamResult>
    where
        F: FnMut(&str),
    {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": messages,
            "stream": true,
            "max_tokens": max_tokens,
            "temperature": temperature,
        });

        let started = Instant::now();
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            .send()
            .with_context(|| format!("请求 LLM 失败：{url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            let brief: String = text.chars().take(300).collect();
            bail!("LLM 返回 {status}：{brief}");
        }

        let mut reader = BufReader::new(resp);
        let mut line = String::new();
        let mut result = StreamResult::default();

        loop {
            if cancel.load(Ordering::Relaxed) {
                result.cancelled = true;
                break;
            }
            line.clear();
            let n = reader.read_line(&mut line).context("读取 LLM 流失败")?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(':') {
                continue;
            }
            let Some(data) = trimmed.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            if let Some(reason) = value["choices"][0]["finish_reason"].as_str() {
                if !reason.is_empty() {
                    result.finish_reason = Some(reason.to_string());
                }
            }
            if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
                if delta.is_empty() {
                    continue;
                }
                if result.first_token_ms.is_none() {
                    result.first_token_ms = Some(started.elapsed().as_millis() as u64);
                }
                result.text.push_str(delta);
                on_delta(delta);
            }
        }

        result.total_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }
}

/// 对话上下文：system + 最近若干轮，按条数与字符预算裁剪。
pub struct Dialog {
    system: String,
    history: Vec<Message>,
    max_messages: usize,
    max_chars: usize,
}

impl Dialog {
    pub fn new(system: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            history: Vec::new(),
            max_messages: 12,
            max_chars: 4000,
        }
    }

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.history.push(Message::user(text));
    }

    pub fn push_assistant(&mut self, text: impl Into<String>) {
        self.history.push(Message::assistant(text));
    }

    /// 组装请求消息：system + 裁剪后的历史。
    pub fn messages(&self) -> Vec<Message> {
        let mut picked: Vec<&Message> = Vec::new();
        let mut chars = 0usize;
        for msg in self.history.iter().rev() {
            chars += msg.content.chars().count();
            if picked.len() >= self.max_messages || chars > self.max_chars {
                break;
            }
            picked.push(msg);
        }
        picked.reverse();

        let mut out = Vec::with_capacity(picked.len() + 1);
        out.push(Message::system(self.system.clone()));
        out.extend(picked.into_iter().cloned());
        out
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }

    /// 热更新 system prompt，保留已有对话历史。
    pub fn set_system(&mut self, system: impl Into<String>) {
        self.system = system.into();
    }
}

/// Qwen3.5/3.6 需要官方身份首行 + 关闭思考，才能稳定出正文。
pub fn system_prompt_for(persona: &str, model: &str) -> String {
    let mut prompt = String::new();
    if model.contains("qwen3.6") || model.contains("qwen3_5") || model.contains("qwen3.5") {
        prompt.push_str(
            "You are Qwen, created by Alibaba Cloud. You are a helpful assistant. <|think_off|>\n",
        );
    }
    prompt.push_str(&spoken_system_prompt(persona));
    prompt
}

/// 口语化系统提示：短句、不念 Markdown、一次两三句。
pub fn spoken_system_prompt(persona: &str) -> String {
    format!(
        "{persona}\n\n\
你说的是口语，不是书面语。规则：\n\
- 始终用中文回答，除非用户明确要求英文；
- 平时两三句，先接话；他明确让你详细讲或问知识时，可以多说几句；\n\
- 不要用 Markdown、列表、标题、表情符号；\n\
- 不要念出括号里的动作或旁白；\n\
- 不要自称 AI 或助手；\n\
- 不知道就说不知道，不要编。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_trims_to_budget() {
        let mut d = Dialog::new("sys");
        for i in 0..30 {
            d.push_user(format!("消息{i}"));
            d.push_assistant("好的");
        }
        let msgs = d.messages();
        assert!(msgs.len() <= 13);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs.last().unwrap().content, "好的");
    }

    #[test]
    fn dialog_keeps_recent_messages_in_order() {
        let mut d = Dialog::new("sys");
        d.push_user("一");
        d.push_assistant("二");
        d.push_user("三");
        let msgs = d.messages();
        let contents: Vec<_> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["sys", "一", "二", "三"]);
    }
}
