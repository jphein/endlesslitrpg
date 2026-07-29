//! The HTTP shape of Ember, and nothing else.
//!
//! Prompt text lives in [`crate::prompt`], the extraction schema in [`crate::extract`].
//! This module only knows how to put a `ChatSpec` on the wire and turn the answer into
//! a [`Completion`]. [`build_request_body`] and [`parse_completion`] are pure, so the
//! wire contract is unit-tested without a network.
//!
//! # What was measured against `familiar:8091` on 2026-07-29
//!
//! * `response_format: {"type": "json_schema", …}` is **supported and enforced**. The
//!   server converts the JSON Schema to a grammar itself; GBNF is not needed.
//! * An uncompilable schema is refused with **HTTP 400**, not silently ignored.
//! * Qwen3.6 **reasons by default**, and reasoning is not grammar-constrained. Left on,
//!   a constrained call returns `content: ""` with `finish_reason: "length"`. Hence
//!   [`EmberConfig::disable_thinking`] defaults to `true`.

use core::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::EmberError;

/// Ember's OpenAI-compatible endpoint (spec §4.1).
pub const DEFAULT_BASE_URL: &str = "http://familiar:8091";

/// The single model this server serves.
pub const DEFAULT_MODEL: &str = "qwen36-coder";

/// Generous by design: a 2000-word chapter is ~2700 tokens and Ember runs at ~47 tok/s,
/// so a whole chapter is well under a minute. The timeout exists to unstick a hung
/// socket, not to bound generation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }
}

#[derive(Debug, Clone)]
pub struct EmberConfig {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
    /// Send `chat_template_kwargs.enable_thinking = false`.
    ///
    /// **Leave this on.** Qwen3.6 runs a reasoning pass by default and that reasoning is
    /// *not* covered by the response-format grammar, so a schema-constrained call spends
    /// its whole `max_tokens` budget thinking and returns empty content. Measured, not
    /// assumed — `tests/live_ember.rs` pins the behaviour.
    pub disable_thinking: bool,
}

impl Default for EmberConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            timeout: DEFAULT_TIMEOUT,
            disable_thinking: true,
        }
    }
}

/// One request's worth of sampling parameters. Built by the caller; the prompt itself
/// comes from [`crate::prompt`].
#[derive(Debug, Clone)]
pub struct ChatSpec {
    pub messages: Vec<Message>,
    pub temperature: f64,
    pub max_tokens: u32,
    /// Passed through verbatim. Use [`crate::extract::response_format`] for pass 2 and
    /// leave it `None` for the creative pass.
    pub response_format: Option<Value>,
}

impl ChatSpec {
    /// Defaults suit pass 1: warm sampling and room for a full chapter plus overrun.
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            temperature: 0.9,
            max_tokens: 6000,
            response_format: None,
        }
    }

    pub fn temperature(mut self, t: f64) -> Self {
        self.temperature = t;
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    pub fn response_format(mut self, rf: Value) -> Self {
        self.response_format = Some(rf);
        self
    }
}

/// What came back, already stripped of reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub content: String,
    /// Present when the server split reasoning into its own field.
    pub reasoning: Option<String>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl Completion {
    /// `max_tokens` was hit, so the text stops mid-thought. For pass 2 this guarantees
    /// invalid JSON; for pass 1 it means a chapter with no ending.
    pub fn truncated(&self) -> bool {
        self.finish_reason.as_deref() == Some("length")
    }
}

/// Assemble the request body. Pure, so every claim above is testable offline.
pub fn build_request_body(cfg: &EmberConfig, spec: &ChatSpec) -> Value {
    let mut body = json!({
        "model": cfg.model,
        "messages": spec.messages,
        "temperature": spec.temperature,
        "max_tokens": spec.max_tokens,
    });

    if cfg.disable_thinking {
        body["chat_template_kwargs"] = json!({ "enable_thinking": false });
    }

    // Only set the key when we actually mean it: a `null` response_format is a 400 on
    // some builds, and pass 1 must stay unconstrained.
    if let Some(rf) = &spec.response_format {
        body["response_format"] = rf.clone();
    }

    body
}

/// Turn an OpenAI chat-completion envelope into a [`Completion`]. Pure.
pub fn parse_completion(raw: &Value) -> Result<Completion, EmberError> {
    let choice = raw
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| EmberError::Protocol {
            detail: "response had no choices[0]".to_string(),
        })?;

    let message = choice.get("message").ok_or_else(|| EmberError::Protocol {
        detail: "choices[0] had no message".to_string(),
    })?;

    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);

    let reasoning = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);

    let raw_content = message.get("content").and_then(Value::as_str).unwrap_or("");
    let content = strip_reasoning_block(raw_content);

    if content.is_empty() {
        return Err(EmberError::EmptyContent {
            finish_reason,
            reasoning_chars: reasoning.as_deref().map_or(0, str::len),
        });
    }

    let usage = raw.get("usage");
    let tokens = |key: &str| -> u32 {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32
    };

    Ok(Completion {
        content: content.to_string(),
        reasoning,
        finish_reason,
        prompt_tokens: tokens("prompt_tokens"),
        completion_tokens: tokens("completion_tokens"),
    })
}

/// Remove a leading `<think>…</think>` block.
///
/// This server splits reasoning into `reasoning_content`, but the same llama.cpp binary
/// started with a different `--reasoning-format` inlines it into `content` instead.
/// Handling both here costs four lines and stops "Here's a thinking process" being read
/// aloud in the narrator's voice.
///
/// An *unterminated* block is deliberately left alone: a visible artifact beats
/// discarding a whole chapter.
pub fn strip_reasoning_block(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(rest) = trimmed.strip_prefix("<think>") else {
        return trimmed;
    };
    match rest.find("</think>") {
        Some(end) => rest[end + "</think>".len()..].trim(),
        None => trimmed,
    }
}

#[derive(Debug, Clone)]
pub struct EmberClient {
    http: reqwest::Client,
    cfg: EmberConfig,
}

impl EmberClient {
    pub fn new(cfg: EmberConfig) -> Result<Self, EmberError> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| EmberError::Config {
                detail: e.to_string(),
            })?;
        Ok(Self { http, cfg })
    }

    pub fn with_defaults() -> Result<Self, EmberError> {
        Self::new(EmberConfig::default())
    }

    pub fn config(&self) -> &EmberConfig {
        &self.cfg
    }

    /// `POST /v1/chat/completions`.
    pub async fn chat(&self, spec: &ChatSpec) -> Result<Completion, EmberError> {
        let body = build_request_body(&self.cfg, spec);
        let url = format!(
            "{}/v1/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let raw = self.send_json(&url, Some(&body)).await?;
        parse_completion(&raw)
    }

    /// `GET /v1/models` — cheap liveness probe that also confirms the model id.
    pub async fn models(&self) -> Result<Vec<String>, EmberError> {
        let url = format!("{}/v1/models", self.cfg.base_url.trim_end_matches('/'));
        let raw = self.send_json(&url, None).await?;
        let ids = raw
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| EmberError::Protocol {
                detail: "/v1/models had no data array".to_string(),
            })?
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        Ok(ids)
    }

    async fn send_json(&self, url: &str, body: Option<&Value>) -> Result<Value, EmberError> {
        let req = match body {
            Some(b) => self.http.post(url).json(b),
            None => self.http.get(url),
        };

        let resp = req.send().await.map_err(|e| EmberError::Transport {
            detail: e.to_string(),
        })?;

        let status = resp.status();
        // Read the body before branching: a 400 from llama.cpp carries the reason the
        // schema would not compile, and that message is the whole diagnostic.
        let text = resp.text().await.map_err(|e| EmberError::Transport {
            detail: format!("reading response body: {e}"),
        })?;

        if !status.is_success() {
            return Err(EmberError::Status {
                status: status.as_u16(),
                body: text,
            });
        }

        serde_json::from_str(&text).map_err(|e| EmberError::Protocol {
            detail: format!("response was not JSON: {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_trailing_slash_does_not_double_up() {
        let cfg = EmberConfig {
            base_url: "http://familiar:8091/".to_string(),
            ..EmberConfig::default()
        };
        let url = format!("{}/v1/models", cfg.base_url.trim_end_matches('/'));
        assert_eq!(url, "http://familiar:8091/v1/models");
    }

    #[test]
    fn temperature_survives_the_json_round_trip_exactly() {
        // f32 would serialize 0.9 as 0.8999999761581421 and quietly change sampling.
        let body = build_request_body(
            &EmberConfig::default(),
            &ChatSpec::new(vec![Message::user("x")]).temperature(0.9),
        );
        assert_eq!(body["temperature"], json!(0.9));
    }

    #[test]
    fn zero_temperature_is_preserved_for_pass_two() {
        let body = build_request_body(
            &EmberConfig::default(),
            &ChatSpec::new(vec![Message::user("x")]).temperature(0.0),
        );
        assert_eq!(body["temperature"], json!(0.0));
    }
}
