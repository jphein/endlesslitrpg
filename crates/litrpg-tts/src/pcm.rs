//! The one audio representation that crosses the plugin boundary.

use litrpg_core::{BYTES_PER_MS, SAMPLE_RATE_HZ};

/// Constructing a `Pcm16k` can only fail one way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PcmError {
    /// An odd byte count means half a sample — a truncated read, a bad offset,
    /// or a header that leaked into a headerless stream. Always a bug.
    #[error("PCM buffer has an odd byte length ({0}); s16le samples are 2 bytes")]
    OddByteLength(usize),
}

/// **16 kHz mono s16le raw PCM, headerless** — exactly 32 000 B/s (spec §7.1).
///
/// Every backend returns this. Normalizing at the plugin boundary is what makes
/// mixing sherpa and Azure *within one chapter* safe: concatenation is
/// `Vec::extend`, and the manifest's `ms × 32` byte offsets address one
/// continuous stream regardless of which engine produced which segment.
///
/// This is byte-for-byte what the watch's `audio_out::play_pcm` consumes, which
/// is why no decoder exists anywhere in the system.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Pcm16k(Vec<u8>);

impl Pcm16k {
    /// 32 bytes per millisecond, exactly. Re-exported from `litrpg-core` so the
    /// constant has one definition across the daemon and the watch firmware.
    pub const BYTES_PER_MS: usize = BYTES_PER_MS as usize;

    /// 32 000 bytes per second.
    pub const BYTES_PER_SEC: usize = BYTES_PER_MS as usize * 1_000;

    /// 16 000 Hz.
    pub const SAMPLE_RATE_HZ: u32 = SAMPLE_RATE_HZ;

    /// Wrap raw bytes, rejecting a half sample.
    pub fn new(bytes: Vec<u8>) -> Result<Self, PcmError> {
        if !bytes.len().is_multiple_of(2) {
            return Err(PcmError::OddByteLength(bytes.len()));
        }
        Ok(Self(bytes))
    }

    /// Wrap a byte slice.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, PcmError> {
        Self::new(bytes.to_vec())
    }

    /// Build from decoded samples (what a native engine hands back after its own
    /// resample to 16 kHz).
    pub fn from_samples(samples: &[i16]) -> Self {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        Self(bytes)
    }

    pub fn empty() -> Self {
        Self(Vec::new())
    }

    /// `ms` milliseconds of digital silence. Used for inter-segment pacing gaps
    /// and for whole-millisecond padding.
    pub fn silence_ms(ms: u32) -> Self {
        Self(vec![0u8; ms as usize * Self::BYTES_PER_MS])
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Sample count (always `len() / 2`).
    pub fn samples(&self) -> usize {
        self.0.len() / 2
    }

    /// Whole milliseconds of audio, **floored**.
    ///
    /// Real TTS output lands on arbitrary sample counts, so a sub-millisecond
    /// tail is the norm rather than an edge case: 32 B = 16 samples = 1 ms, so
    /// any buffer whose length is not a multiple of 32 has a remainder. The
    /// identity `duration_ms() * 32 == len()` therefore holds exactly when
    /// [`Pcm16k::is_whole_ms`] is true — see [`Pcm16k::pad_to_whole_ms`], which
    /// is how the render pipeline establishes it before writing a manifest.
    pub fn duration_ms(&self) -> u32 {
        (self.0.len() / Self::BYTES_PER_MS) as u32
    }

    /// Duration including the sub-millisecond tail.
    pub fn duration_secs_f64(&self) -> f64 {
        self.0.len() as f64 / Self::BYTES_PER_SEC as f64
    }

    /// True when the buffer is an exact number of milliseconds.
    pub fn is_whole_ms(&self) -> bool {
        self.0.len().is_multiple_of(Self::BYTES_PER_MS)
    }

    /// Bytes past the last whole millisecond (0..32).
    pub fn remainder_bytes(&self) -> usize {
        self.0.len() % Self::BYTES_PER_MS
    }

    /// Zero-pad the tail up to the next whole millisecond.
    ///
    /// This is what makes a manifest's `start_byte = start_ms × 32` land on the
    /// real audio. Without it, segment *N*'s offset drifts by the accumulated
    /// remainders of segments `0..N`. The pad is at most 31 bytes — under one
    /// millisecond of silence, inaudible, and appended to the tail where every
    /// engine already emits near-zero amplitude.
    pub fn pad_to_whole_ms(&mut self) {
        let rem = self.remainder_bytes();
        if rem != 0 {
            self.0.resize(self.0.len() + (Self::BYTES_PER_MS - rem), 0);
        }
    }

    /// [`Pcm16k::pad_to_whole_ms`], by value.
    pub fn padded_to_whole_ms(mut self) -> Self {
        self.pad_to_whole_ms();
        self
    }

    /// Append another buffer. Both are already 16 kHz, so this is the whole of
    /// "mixing backends".
    pub fn extend(&mut self, other: &Pcm16k) {
        self.0.extend_from_slice(&other.0);
    }

    /// Join buffers in order.
    pub fn concat(parts: &[Pcm16k]) -> Pcm16k {
        let mut out = Vec::with_capacity(parts.iter().map(|p| p.0.len()).sum());
        for p in parts {
            out.extend_from_slice(&p.0);
        }
        Pcm16k(out)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Decode to samples. Allocates; intended for tests and analysis, not the
    /// render hot path.
    pub fn to_samples(&self) -> Vec<i16> {
        self.0
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }
}

impl core::fmt::Debug for Pcm16k {
    /// Never dump 25 MB of samples into a log line.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pcm16k")
            .field("bytes", &self.0.len())
            .field("ms", &self.duration_ms())
            .field("whole_ms", &self.is_whole_ms())
            .finish()
    }
}

impl TryFrom<Vec<u8>> for Pcm16k {
    type Error = PcmError;
    fn try_from(v: Vec<u8>) -> Result<Self, PcmError> {
        Self::new(v)
    }
}

impl AsRef<[u8]> for Pcm16k {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl FromIterator<Pcm16k> for Pcm16k {
    fn from_iter<I: IntoIterator<Item = Pcm16k>>(iter: I) -> Self {
        let mut out = Vec::new();
        for p in iter {
            out.extend_from_slice(&p.0);
        }
        Pcm16k(out)
    }
}
