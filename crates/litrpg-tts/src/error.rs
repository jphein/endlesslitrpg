//! One error type for the whole plugin layer.

use crate::pcm::PcmError;
use litrpg_core::VoiceRefError;

pub type Result<T> = core::result::Result<T, TtsError>;

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    /// The `voice_ref` named a backend that is not registered. Surfaced at
    /// cast-assignment time, not mid-render.
    #[error("no TTS backend registered with id '{0}'")]
    UnknownBackend(String),

    /// The backend exists but cannot run: models absent, key absent.
    #[error("TTS backend '{id}' is unavailable: {reason}")]
    BackendUnavailable { id: String, reason: String },

    #[error("a TTS backend with id '{0}' is already registered")]
    DuplicateBackend(String),

    /// `VoiceRefError` comes from the `no_std` core crate and does not implement
    /// `core::error::Error`, so it is carried by value rather than as a `source`.
    #[error("invalid voice_ref: {0}")]
    VoiceRef(VoiceRefError),

    /// The remainder of a `voice_ref` made no sense to its owning plugin.
    #[error("backend '{backend}' rejected voice '{voice}': {reason}")]
    UnknownVoice {
        backend: String,
        voice: String,
        reason: String,
    },

    /// A voice name that could inject SSML markup. Refused rather than silently
    /// replaced with a default — a silent substitution ships the wrong narrator.
    #[error("voice name is not attribute-safe: '{0}'")]
    InvalidVoiceName(String),

    #[error(transparent)]
    Pcm(#[from] PcmError),

    #[error("no Azure credential found: {0}")]
    MissingCredential(String),

    #[error("could not read {path}: {source}")]
    ConfigRead {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse {path}: {source}")]
    ConfigParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Azure TTS request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A non-2xx from Azure. The body is truncated and never contains the key.
    #[error("Azure TTS returned {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("{stage} failed with {status}: {stderr}")]
    Ffmpeg {
        stage: String,
        status: String,
        stderr: String,
    },

    #[error("could not run ffmpeg ({path}): {source}")]
    FfmpegSpawn {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A model file or directory the sherpa plugin needs is absent.
    #[error("sherpa model asset missing: {0}")]
    ModelMissing(String),

    #[error("sherpa synthesis failed: {0}")]
    Synthesis(String),

    /// A worker thread panicked or was cancelled.
    #[error("render worker failed: {0}")]
    Worker(String),
}

impl From<VoiceRefError> for TtsError {
    fn from(e: VoiceRefError) -> Self {
        Self::VoiceRef(e)
    }
}
