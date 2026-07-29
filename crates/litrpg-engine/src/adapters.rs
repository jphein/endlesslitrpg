//! The production implementations of [`Generator`] and [`Renderer`].
//!
//! Both are thin on purpose. Every decision worth testing lives in [`crate::cycle`] or in
//! the pure functions of `litrpg-ember`; these two types only wire a port to a real
//! service, so there is nothing here for a fake to fail to reproduce.

use litrpg_ember::client::{ChatSpec, EmberClient};
use litrpg_ember::extract::response_format;
use litrpg_ember::{EmberError, Extraction, Pass1Input, extract, pass1_messages, pass2_messages};
use litrpg_tts::{Pcm16k, RenderRequest, TtsError, TtsRegistry, async_trait};

use crate::ports::{Generator, Renderer};

/// Token ceiling for the creative pass.
///
/// A 2000-word chapter is roughly 2700 tokens; the headroom absorbs a model that runs
/// long rather than truncating a chapter mid-sentence, which §10 treats as a pass-1
/// failure and would cost the whole cycle.
pub const PASS1_MAX_TOKENS: u32 = 6000;

/// The extraction payload is small — a summary plus a handful of deltas — but a chapter
/// with a large cast can produce a long `new_lore` array, and a truncated JSON object is
/// a total loss rather than a partial one.
pub const PASS2_MAX_TOKENS: u32 = 3000;

/// [`Generator`] over the real Ember endpoint.
pub struct EmberGenerator {
    client: EmberClient,
}

impl EmberGenerator {
    pub fn new(client: EmberClient) -> Self {
        Self { client }
    }

    /// Build a client from the shared config, so `ember_url` / `ember_model` are honoured.
    pub fn from_config(cfg: &litrpg_config::Config) -> Result<Self, EmberError> {
        let ember = litrpg_ember::EmberConfig {
            base_url: cfg.ember_url.clone(),
            model: cfg.ember_model.clone(),
            ..litrpg_ember::EmberConfig::default()
        };
        Ok(Self::new(EmberClient::new(ember)?))
    }

    pub fn client(&self) -> &EmberClient {
        &self.client
    }
}

#[async_trait]
impl Generator for EmberGenerator {
    async fn pass1(&self, input: &Pass1Input<'_>, temperature: f64) -> Result<String, EmberError> {
        let spec = ChatSpec::new(pass1_messages(input))
            .temperature(temperature)
            .max_tokens(PASS1_MAX_TOKENS);
        let out = self.client.chat(&spec).await?;

        // A chapter cut off at max_tokens has no ending. §10 forbids partial chapters, so
        // surface it as malformed and let the cycle retry rather than publishing a
        // chapter that stops mid-sentence.
        if out.truncated() {
            return Err(EmberError::Malformed {
                body: out.content,
                detail: "pass 1 hit max_tokens; the chapter has no ending".to_string(),
            });
        }
        Ok(out.content)
    }

    async fn pass2(
        &self,
        chapter_text: &str,
        known_subjects: &[String],
    ) -> Result<Extraction, EmberError> {
        let spec = ChatSpec::new(pass2_messages(chapter_text, known_subjects))
            .temperature(0.0)
            .max_tokens(PASS2_MAX_TOKENS)
            .response_format(response_format());
        let out = self.client.chat(&spec).await?;
        extract::parse_extraction(&out.content)
    }
}

/// [`Renderer`] over the TTS plugin registry.
pub struct RegistryRenderer {
    registry: TtsRegistry,
}

impl RegistryRenderer {
    pub fn new(registry: TtsRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &TtsRegistry {
        &self.registry
    }
}

#[async_trait]
impl Renderer for RegistryRenderer {
    async fn render_all(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>, TtsError> {
        // `render_all` groups by backend so each plugin gets one batch call — that is what
        // lets Azure emit a single multi-voice request and sherpa fill its worker pool.
        self.registry.render_all(reqs).await
    }
}
