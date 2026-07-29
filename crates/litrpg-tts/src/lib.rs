//! TTS plugin registry.
//!
//! Two first-class backends — sherpa-onnx (local, free, unmetered) and Azure
//! DragonHD — behind one trait. Every plugin returns **16 kHz mono s16le raw PCM**,
//! which is what makes mixing backends within a single chapter safe.
//!
//! # Layout
//!
//! - [`pcm`] — [`Pcm16k`], the normalized output contract (spec §7.1).
//! - [`backend`] — [`TtsBackend`], [`RenderRequest`], [`Availability`], [`VoiceDesc`].
//! - [`registry`] — [`TtsRegistry`]: `voice_ref` → owning plugin, plus the
//!   aggregated voice catalog behind `GET /api/voices`.
//! - [`azure`] — Azure DragonHD. 16 kHz native, one multi-voice SSML document per
//!   request, credentials shared with `speech-to-cli`.
//! - [`resample`] — native rate → 16 kHz and the `SYSTEM` colouring, behind a
//!   [`resample::PostProcessor`] seam.
//! - [`sherpa`] — sherpa-onnx, behind the **off-by-default `sherpa` feature** so a
//!   plain `cargo test` never builds native TTS libraries.
//!
//! # The shape of the design
//!
//! [`TtsBackend::render_batch`] has a defaulted loop over
//! [`TtsBackend::render`]. That default is the hinge: Azure overrides it to fan
//! out concurrently over one pooled connection, sherpa overrides it to shard
//! across a worker pool, and the engine calls the same method either way.
//! [`TtsBackend::render_joined`] is the companion for callers that want one
//! continuous stream instead of per-segment buffers — that is the shape Azure
//! serves with a **single** multi-voice SSML request.
//!
//! ```no_run
//! use litrpg_core::SpeakerKind;
//! use litrpg_tts::{DEFAULT_GAP_MS, RenderRequest, TtsRegistry, assemble, azure::AzureBackend};
//!
//! # async fn demo() -> Result<(), litrpg_tts::TtsError> {
//! let registry = TtsRegistry::new().with(Box::new(AzureBackend::from_default_config()?));
//!
//! let reqs = vec![RenderRequest::parse(
//!     0,
//!     "azure:en-GB-Ada:DragonHDLatestNeural",
//!     "The vale smelled of iron and wet ash.",
//!     SpeakerKind::Narrator,
//! )?];
//!
//! // `render_all` shards by backend; `assemble` joins the results and *measures*
//! // where each segment landed. Never predict those offsets — `loudnorm` changes
//! // stream length, so they are only true once the audio exists.
//! let chapter = assemble(&registry.render_all(&reqs).await?, DEFAULT_GAP_MS);
//! assert_eq!(chapter.pcm.len() as u32, chapter.pcm.duration_ms() * 32);
//! for span in &chapter.spans {
//!     let _range_header = (span.start_byte(), span.end_byte());
//! }
//! # Ok(())
//! # }
//! ```

pub mod azure;
pub mod backend;
pub mod error;
pub mod pcm;
pub mod registry;
pub mod resample;
pub mod sherpa;

pub use azure::{RenderPart, VoicePreflight};
pub use backend::{Availability, CostClass, Gender, RenderRequest, TtsBackend, VoiceDesc};
pub use error::{Result, TtsError};
pub use pcm::{
    Assembly, DEFAULT_GAP_MS, FailedSegment, Pcm16k, PcmError, Span, assemble,
    fill_failures_with_silence,
};
pub use registry::TtsRegistry;
pub use resample::{FfmpegPostProcessor, PostProcess, PostProcessor};

/// Re-exported so a downstream crate can `impl TtsBackend` without adding
/// `async-trait` to its own manifest and risking a version skew.
pub use async_trait::async_trait;
