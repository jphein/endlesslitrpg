//! The four seams the loop is written against.
//!
//! Testability is the design constraint here, not an afterthought. The cycle in
//! [`crate::cycle`] talks only to these traits, so the whole pipeline — including the
//! ordering rules that are the easiest thing in the system to get silently wrong —
//! is exercised with **no network, no GPU and no ffmpeg**.
//!
//! | Port | Real implementation | What it hides |
//! |---|---|---|
//! | [`Generator`] | [`crate::adapters::EmberGenerator`] | Ember on `familiar:8091` |
//! | [`Renderer`] | [`crate::adapters::RegistryRenderer`] | the TTS plugin registry |
//! | [`Library`] | *(none yet — see below)* | `story`, `lore`, `summaries` tables |
//! | [`Artifacts`] | [`crate::publish::FsArtifacts`] | the filesystem and ffmpeg |
//!
//! # Why [`Library`] exists
//!
//! `litrpg-store` currently exposes writers for lore (`upsert_lore`) but **no readers**
//! for the `story`, `lore` or `summaries` tables, and no writer for a chapter summary.
//! The lorebook loop needs all four: lore written during chapter 5 has to be
//! retrievable when chapter 6 is assembled, or §6.3 retrieval silently degrades to
//! "always-on entries only" — a failure with no error message.
//!
//! Rather than reach into a crate another agent owns, the engine declares the reads it
//! needs as a port. Once the store grows those queries, the real implementation is a
//! thin wrapper over them; nothing in the cycle changes.

use litrpg_core::Manifest;
use litrpg_ember::prompt::{ChapterSummary, LoreEntry};
use litrpg_ember::{EmberError, Extraction, Pass1Input};
use litrpg_tts::{Pcm16k, RenderRequest, TtsError, async_trait};

use crate::error::EngineError;

/// The `story` row (spec §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryMeta {
    pub title: String,
    pub protagonist: String,
    /// Contents of `story/prompt.md`, the git-tracked source of truth (§9.3).
    pub prompt_md: String,
    pub arc_outline_md: String,
    pub target_words: u32,
}

/// Ember, reduced to the two calls the loop makes.
///
/// Retry policy deliberately lives in the cycle, not here: the two passes degrade
/// differently (§10) and an implementation that retried internally would hide the
/// distinction the error taxonomy exists to preserve.
#[async_trait]
pub trait Generator: Send + Sync {
    /// The creative pass. Returns raw tagged prose, unparsed.
    async fn pass1(&self, input: &Pass1Input<'_>, temperature: f64) -> Result<String, EmberError>;

    /// The extraction pass, schema-constrained, temperature 0.
    ///
    /// `speakers` is the **parsed** speaker list, supplied rather than requested: the engine already
    /// knows who spoke, and asking the model to enumerate them invites the omission that left two
    /// of three live characters without a gender hint.
    async fn pass2(
        &self,
        chapter_text: &str,
        known_subjects: &[String],
        speakers: &[String],
    ) -> Result<Extraction, EmberError>;
}

/// Segments in, one `Pcm16k` per request out, in request order.
#[async_trait]
pub trait Renderer: Send + Sync {
    async fn render_all(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>, TtsError>;
}

/// The story-context reads and the summary write that `litrpg-store` does not expose yet.
pub trait Library: Send + Sync {
    fn story(&self) -> Result<StoryMeta, EngineError>;

    /// Every lore row. Selection is [`litrpg_ember::match_lore`]'s job, not the
    /// store's, so that keyword matching stays a pure function.
    fn lore(&self) -> Result<Vec<LoreEntry>, EngineError>;

    /// The most recent `limit` chapter summaries (`summaries.level = 0`), oldest first.
    fn recent_summaries(&self, limit: usize) -> Result<Vec<ChapterSummary>, EngineError>;

    /// Persist a chapter summary. Idempotent by chapter — a re-extraction of a
    /// `state_dirty` chapter must replace, not duplicate.
    fn put_summary(&self, chapter: u32, body_md: &str) -> Result<(), EngineError>;

    /// Record the prompt hash now **in effect**.
    ///
    /// `story.prompt_hash` means "the premise chapters are currently being written from", which is
    /// why `litrpg prompt` deliberately does not touch it: an edited file is *pending* until a
    /// chapter boundary picks it up. This is the other half of that contract — without it, "in
    /// effect" silently degrades to "in effect as of `init`", and `litrpg status` reports a pending
    /// edit forever even though the edit has demonstrably been applied.
    fn set_prompt_hash(&self, hash: &str) -> Result<(), EngineError>;
}

/// Chapter artifacts on disk (§8). Behind a trait so the cycle's tests need no
/// filesystem and no ffmpeg binary.
#[async_trait]
pub trait Artifacts: Send + Sync {
    /// `NNNN.md` — canonical text, permanent.
    async fn write_text(&self, chapter: u32, text_md: &str) -> Result<String, EngineError>;

    /// `NNNN.pcm` — watch playback. Pruned outside the buffer window.
    async fn write_pcm(&self, chapter: u32, pcm: &Pcm16k) -> Result<String, EngineError>;

    /// `NNNN.json` — the manifest every client derives Range requests from.
    async fn write_manifest(
        &self,
        chapter: u32,
        manifest: &Manifest,
    ) -> Result<String, EngineError>;

    /// `NNNN.mp3`, derived from the `.pcm` (§8: pcm is the source, mp3 the derivative).
    async fn encode_mp3(&self, chapter: u32, pcm_path: &str) -> Result<String, EngineError>;
}
