//! Engine errors, and the outcome of one cycle.
//!
//! The governing rule from spec §10 is **a bookkeeping failure must never cost a
//! chapter**. That rule is the reason most failures in this crate are *not* errors:
//! a rejected delta, a failed extraction and a failed render are all recorded
//! outcomes that let the chapter ship. Only a failure that leaves nothing worth
//! publishing aborts a cycle.

use litrpg_ember::EmberError;
use litrpg_store::StoreError;
use litrpg_tts::TtsError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("store failure: {0}")]
    Store(#[from] StoreError),

    /// Only for the *extraction* pass reaching the store, or a health probe. Pass-1
    /// failures are folded into [`CycleOutcome::Abandoned`] instead, because a
    /// chapter that was never written is not an engine fault.
    #[error("ember failure: {0}")]
    Ember(#[from] EmberError),

    #[error("tts failure: {0}")]
    Tts(#[from] TtsError),

    #[error("could not write chapter artifact: {detail}")]
    Artifact { detail: String },

    #[error("story library: {detail}")]
    Library { detail: String },
}

/// What one turn of the loop did. Every variant is a normal, expected result — the
/// caller logs it and sleeps; nothing here means "the daemon is broken".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleOutcome {
    /// The rendered-ahead buffer is at or above target, so no work was done.
    Idle { buffer_depth: u32 },

    /// A chapter already had prose but no audio — a crash between publish stages, or
    /// an earlier TTS failure — and the render was retried. No text was regenerated,
    /// because regenerating prose that already shipped would rewrite history.
    ResumedRender { chapter: u32, has_audio: bool },

    /// A new chapter was produced. `has_audio == false` means the text shipped and
    /// the render is queued for retry (§10). `state_dirty == true` means pass 2
    /// failed and the deltas were never extracted.
    Produced {
        chapter: u32,
        has_audio: bool,
        state_dirty: bool,
        applied: usize,
        rejected: usize,
    },

    /// Pass 1 failed. **No partial chapter is ever written** (§10), so there is
    /// nothing to resume and nothing to clean up.
    Abandoned {
        chapter: u32,
        reason: String,
        /// True when the cause was the network or an unwell server, so the caller
        /// should back off rather than re-prompt immediately.
        backoff: bool,
    },
}

impl CycleOutcome {
    /// Whether this turn wrote a chapter. Useful for a caller deciding whether to
    /// loop again immediately or sleep.
    pub fn produced_chapter(&self) -> Option<u32> {
        match self {
            Self::Produced { chapter, .. } | Self::ResumedRender { chapter, .. } => Some(*chapter),
            _ => None,
        }
    }

    /// Whether the caller should back off before the next turn.
    pub fn should_backoff(&self) -> bool {
        matches!(self, Self::Abandoned { backoff: true, .. })
    }
}
