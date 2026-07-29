//! The chapter loop (spec §5.1).
//!
//! Joins the other crates into one cycle: check the buffer, assemble a prompt, generate,
//! parse, assign voices, extract, validate into the ledger, render audio, publish
//! artifacts. Every stage is idempotent by chapter number, so a crash resumes from the
//! last completed stage rather than corrupting one.
//!
//! ```text
//!                      ┌──────────── litrpg-ember ────────────┐
//!   store ──▶ prompt ──▶ pass 1 ──▶ parse ──▶ cast ──▶ pass 2 ─┘──▶ gate ──▶ ledger
//!                                              │
//!                                              ▼
//!                                     litrpg-tts ──▶ manifest ──▶ md / pcm / mp3 / json
//! ```
//!
//! # Design constraint: the loop is testable without the world
//!
//! Every external dependency sits behind one of four traits in [`ports`] — [`Generator`]
//! (Ember), [`Renderer`] (TTS), [`Library`] (the story tables the store does not expose
//! yet) and [`Artifacts`] (filesystem + ffmpeg). The consequence is that the orderings
//! and degradations that matter most — and that are otherwise only observable by reading
//! forty chapters or listening to them — are ordinary unit tests.
//!
//! # The rules worth knowing before changing anything here
//!
//! * **`new_lore` is applied before the deltas.** A character introduced this chapter is
//!   only a known subject once their lore row exists; reverse the order and every new
//!   character's opening stats are rejected as `UnknownSubject`, silently.
//! * **Manifest timings come from measured PCM lengths, never predicted ones.**
//!   `loudnorm` changes stream length, and `duration_ms × 32 == len` is what every
//!   client's `Range` request depends on.
//! * **Voice assignment is a pure function** of the existing cast and first-appearance
//!   order. A cast that shuffles between runs is only *audible*, so it would surface
//!   dozens of published chapters too late.
//! * **A bookkeeping failure never costs a chapter** (§10). Pass 2 failing, a delta being
//!   rejected, TTS failing, an artifact write failing — the prose still ships. Only a
//!   pass-1 failure abandons the cycle, and it does so before anything is written.

pub mod adapters;
pub mod canon;
pub mod cast;
pub mod cycle;
pub mod error;
pub mod library;
pub mod ports;
pub mod publish;
pub mod render;
pub mod voices;

pub use adapters::{EmberGenerator, RegistryRenderer};
pub use canon::{SubjectResolution, resolve_subject};
pub use cast::{
    CastAssignment, NARRATOR_FALLBACK_VOICE, ParsedSpeaker, SYSTEM_VOICE, VoiceAssigner,
    character_pool, kokoro_voice_ref,
};
pub use cycle::{
    BufferCursor, Engine, EngineConfig, MAX_RESUME_ATTEMPTS, PASS1_TEMPERATURES,
    PASS2_TEMPERATURES, SUMMARY_WINDOW, derive_title, distinct_speakers, plain_chapter_text,
    plan_segments,
};
pub use error::{CycleOutcome, EngineError};
pub use library::StoreLibrary;
pub use ports::{Artifacts, Generator, Library, Renderer, StoryMeta};
pub use publish::FsArtifacts;
pub use render::{
    PlannedSegment, RenderedChapter, SENTENCE_TARGET_CHARS, assemble, chapter_markdown,
    sentence_pieces, split_by_sentence, word_count,
};
pub use voices::{VoicePlan, plan_voices};
