//! Fully-qualified voice references: `backend_id` `:` backend-specific remainder.

use alloc::string::{String, ToString};
use core::fmt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceRefError {
    MissingColon,
    EmptyBackend,
    EmptyRemainder,
}

impl fmt::Display for VoiceRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColon => f.write_str("voice_ref must contain ':'"),
            Self::EmptyBackend => f.write_str("voice_ref backend is empty"),
            Self::EmptyRemainder => f.write_str("voice_ref remainder is empty"),
        }
    }
}

/// A voice reference such as `sherpa:kokoro-multi-lang-v1_0:18` or
/// `azure:en-GB-Ada:DragonHDLatestNeural`.
///
/// Split on the **first colon only**. The remainder is opaque to the engine and
/// parsed by the owning plugin — Azure voice names legitimately contain colons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceRef {
    pub backend: String,
    pub remainder: String,
}

impl VoiceRef {
    pub fn parse(s: &str) -> Result<Self, VoiceRefError> {
        let (backend, remainder) = s.split_once(':').ok_or(VoiceRefError::MissingColon)?;
        if backend.is_empty() {
            return Err(VoiceRefError::EmptyBackend);
        }
        if remainder.is_empty() {
            return Err(VoiceRefError::EmptyRemainder);
        }
        Ok(Self {
            backend: backend.to_string(),
            remainder: remainder.to_string(),
        })
    }
}

impl fmt::Display for VoiceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.backend, self.remainder)
    }
}
