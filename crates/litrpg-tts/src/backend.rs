//! The plugin trait. Two first-class implementations: sherpa-onnx and Azure.

use crate::error::TtsError;
use crate::pcm::Pcm16k;
use async_trait::async_trait;
use litrpg_core::{Segment, SpeakerKind, VoiceRef};
use serde::{Deserialize, Serialize};

/// Whether a backend can actually be used right now.
///
/// This is a runtime fact, not a compile-time one: the `sherpa` feature can be
/// built in while the model files are absent, and the Azure plugin can be built
/// in while no key is configured. The daemon reports this on `GET /api/voices`
/// so a cast assignment fails at assignment time rather than mid-chapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Availability {
    Ready,
    /// Present in the build but unusable, with a human-readable reason.
    Missing {
        reason: String,
    },
}

impl Availability {
    pub fn missing(reason: impl Into<String>) -> Self {
        Self::Missing {
            reason: reason.into(),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Missing { reason } => Some(reason),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gender {
    Female,
    Male,
    Neutral,
    Unknown,
}

/// Whether using a voice costs money. `Free` is local sherpa inference; `Metered`
/// is Azure. Surfaced in the voice catalog so the UI can warn before a cast
/// assignment starts billing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostClass {
    Free,
    Metered,
}

/// One selectable voice, as advertised by its owning plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceDesc {
    /// Fully qualified and directly assignable, e.g.
    /// `sherpa:kokoro-multi-lang-v1_0:18`.
    pub voice_ref: String,
    /// Human label, e.g. `am_puck` or `Ada (DragonHD)`.
    pub label: String,
    /// BCP-47-ish tag, e.g. `en-GB`.
    pub lang: String,
    pub gender: Gender,
    pub cost_class: CostClass,
}

/// One unit of work handed to a backend.
///
/// Deliberately *not* `litrpg_core::Segment`: a `Segment` carries `start_ms` /
/// `end_ms`, which are outputs of rendering, not inputs to it. A `RenderRequest`
/// carries only what synthesis needs, with the voice already parsed — so a
/// malformed `voice_ref` fails once, at construction, instead of inside every
/// plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequest {
    /// Position in the chapter; preserved through batching and re-sharding.
    pub idx: u32,
    pub text: String,
    pub voice: VoiceRef,
    pub kind: SpeakerKind,
}

impl RenderRequest {
    pub fn new(idx: u32, text: impl Into<String>, voice: VoiceRef, kind: SpeakerKind) -> Self {
        Self {
            idx,
            text: text.into(),
            voice,
            kind,
        }
    }

    /// Parse a `voice_ref` string into a request.
    ///
    /// Splits on the **first colon only** via [`VoiceRef::parse`] — Azure voice
    /// names contain colons (`en-GB-Ada:DragonHDLatestNeural`), so a three-way
    /// split is right for sherpa and wrong for Azure.
    pub fn parse(
        idx: u32,
        voice_ref: &str,
        text: impl Into<String>,
        kind: SpeakerKind,
    ) -> Result<Self, TtsError> {
        Ok(Self::new(
            idx,
            text,
            VoiceRef::parse(voice_ref).map_err(TtsError::VoiceRef)?,
            kind,
        ))
    }

    /// Build from a manifest segment, discarding its timing fields.
    pub fn from_segment(seg: &Segment) -> Result<Self, TtsError> {
        Self::parse(seg.idx, &seg.voice_ref, seg.text.clone(), seg.kind)
    }

    /// The backend-specific part of the voice reference, opaque to the engine.
    pub fn voice_remainder(&self) -> &str {
        &self.voice.remainder
    }

    /// True when there is nothing to synthesize.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A TTS plugin.
///
/// sherpa-onnx and Azure are **both first class** (spec §D6) — there is no
/// primary and no fallback. They differ only in what they override.
#[async_trait]
pub trait TtsBackend: Send + Sync {
    /// Stable id, and the first colon-delimited field of every `voice_ref` this
    /// plugin owns: `"sherpa"` or `"azure"`.
    fn id(&self) -> &str;

    /// Can this plugin run right now? Models present? Key present?
    fn available(&self) -> Availability;

    /// Everything this plugin can voice.
    fn voices(&self) -> Vec<VoiceDesc>;

    /// Render one segment to 16 kHz mono s16le raw PCM.
    ///
    /// The returned buffer is **whole-millisecond aligned** ([`Pcm16k::is_whole_ms`]),
    /// so `duration_ms() * 32 == len()` holds exactly and a manifest offset built
    /// from it addresses the real audio. Both shipped plugins align at this
    /// boundary rather than trusting the engine to remember.
    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k, TtsError>;

    /// Render many segments, **one `Pcm16k` per request, in request order**.
    ///
    /// This defaulted loop is the point of the whole design. The engine always
    /// calls `render_batch` and never learns what happened underneath:
    ///
    /// - **Azure** overrides it to fan out with bounded concurrency over one
    ///   HTTP/2 connection.
    /// - **sherpa** overrides it to shard across a worker pool, ~4 threads per
    ///   worker (Reverie measured +48–86% aggregate throughput over one fat
    ///   high-thread process).
    ///
    /// See [`TtsBackend::render_joined`] for the single-request multi-voice path.
    async fn render_batch(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>, TtsError> {
        let mut out = Vec::with_capacity(reqs.len());
        for r in reqs {
            out.push(self.render(r).await?);
        }
        Ok(out)
    }

    /// Render many segments into **one continuous stream**, discarding per-segment
    /// boundaries.
    ///
    /// This exists because Azure's real strength — one multi-voice SSML document
    /// per HTTP request — returns a single undifferentiated audio stream. The
    /// `cognitiveservices/v1` REST endpoint carries no boundary metadata (the
    /// SDK's `BookmarkReached` event is websocket-only), so that request shape
    /// physically cannot answer "where does segment 4 start". Callers that need
    /// segment timings (i.e. anything writing a manifest) use `render_batch`;
    /// callers that just want the audio use this and get the cheaper request.
    ///
    /// The default concatenates `render_batch`, so a plugin need only override
    /// the one that is genuinely cheaper for it.
    async fn render_joined(&self, reqs: &[RenderRequest]) -> Result<Pcm16k, TtsError> {
        Ok(Pcm16k::concat(&self.render_batch(reqs).await?))
    }
}
