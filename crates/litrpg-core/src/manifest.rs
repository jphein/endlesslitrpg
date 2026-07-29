//! Chapter audio manifest — the artifact that drives Range requests and
//! sentence highlighting on every client.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::voice::{VoiceRef, VoiceRefError};

/// Every TTS plugin normalizes to this rate (spec §7.1). It is byte-for-byte what
/// the watch's `audio_out::play_pcm` consumes, which is why no decoder exists.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// 16 kHz * 2 bytes/sample / 1000 = 32 bytes per millisecond, exactly.
pub const BYTES_PER_MS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeakerKind {
    Narrator,
    Character,
    System,
}

impl SpeakerKind {
    /// The canonical string form — the same one `serde(rename_all = "lowercase")`
    /// produces, asserted by test so the two cannot drift.
    ///
    /// # Why this exists
    ///
    /// The same fact had four independent spellings: this serde attribute (inside every
    /// `manifest_json` and every daemon response), the store's `kind_str`/`kind_from_str`
    /// for the `segments.kind` column, the engine's own match, and the CLI's. They
    /// agreed, and nothing made them.
    ///
    /// That mattered more than the filename convention did, because **kind selects the
    /// voice** — the cast assigner maps `Narrator` and `System` to different voices from
    /// `Character`. A drift between the manifest and the `segments` table means a
    /// mis-voiced chapter, and the only symptom is *hearing the wrong person speak*.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Narrator => "narrator",
            Self::Character => "character",
            Self::System => "system",
        }
    }

    /// Parse the canonical form. **Strict on purpose** — see [`SpeakerKind::as_str`].
    ///
    /// The store previously coerced anything unrecognised to `Narrator`, which is the
    /// same destructive-default asymmetry that made `op_from_str` strict: a silent
    /// fallback here re-voices a character as the narrator, and nothing reports it. If
    /// the canonical casing ever changed, `"System"` would have quietly become
    /// `Narrator`.
    pub fn from_canonical(s: &str) -> Option<Self> {
        match s {
            "narrator" => Some(Self::Narrator),
            "character" => Some(Self::Character),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    /// Every variant, for exhaustive tests and for iterating a cast.
    pub const ALL: [Self; 3] = [Self::Narrator, Self::Character, Self::System];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub idx: u32,
    pub speaker: String,
    pub kind: SpeakerKind,
    pub voice_ref: String,
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
}

impl Segment {
    pub fn start_byte(&self) -> u64 {
        self.start_ms as u64 * BYTES_PER_MS as u64
    }

    pub fn end_byte(&self) -> u64 {
        self.end_ms as u64 * BYTES_PER_MS as u64
    }

    pub fn duration_ms(&self) -> u32 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    pub fn voice(&self) -> Result<VoiceRef, VoiceRefError> {
        VoiceRef::parse(&self.voice_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub chapter: u32,
    pub sample_rate: u32,
    pub bytes_per_ms: u32,
    pub duration_ms: u32,
    pub segments: Vec<Segment>,
}

impl Manifest {
    pub fn new(chapter: u32, segments: Vec<Segment>) -> Self {
        let duration_ms = segments.last().map(|s| s.end_ms).unwrap_or(0);
        Self {
            chapter,
            sample_rate: SAMPLE_RATE_HZ,
            bytes_per_ms: BYTES_PER_MS,
            duration_ms,
            segments,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.duration_ms as u64 * BYTES_PER_MS as u64
    }

    /// Half-open lookup: `start_ms <= ms < end_ms`.
    pub fn segment_at_ms(&self, ms: u32) -> Option<&Segment> {
        self.segments
            .iter()
            .find(|s| ms >= s.start_ms && ms < s.end_ms)
    }

    /// True when segments start at 0 and leave no gaps — required for the byte
    /// offsets to address one continuous PCM stream.
    pub fn is_contiguous(&self) -> bool {
        self.segments
            .first()
            .map(|s| s.start_ms == 0)
            .unwrap_or(true)
            && self
                .segments
                .windows(2)
                .all(|w| w[0].end_ms == w[1].start_ms)
    }
}
