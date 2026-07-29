//! Native rate → 16 kHz, plus the `SYSTEM` colouring (spec §7.4, §7.5).
//!
//! sherpa models emit 22 050 Hz (Piper / cori) or 24 000 Hz (Kokoro); the plugin
//! boundary demands 16 kHz. **This must exist or the watch plays chipmunks.**
//!
//! Implemented as a trait so the ffmpeg dependency is a seam, not a hard-wired
//! assumption — a pure-Rust resampler can be dropped in later without touching
//! the sherpa plugin. `ffmpeg` is the default because Reverie measured it at
//! ~850× realtime, i.e. under 2% of synthesis cost, and byte-exact against the
//! 32 000 B/s contract (spike Part 2 §2.3).

use crate::error::{Result, TtsError};
use crate::pcm::Pcm16k;
use std::io::Write;
use std::process::{Command, Stdio};

/// What to do to a segment on its way to 16 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PostProcess {
    /// Apply the `SYSTEM` robot colouring. A **post-render stage, not a voice** —
    /// no sherpa model ships a synthetic-sounding speaker, so SYSTEM stat blocks
    /// are a neutral speaker plus this filter chain.
    pub system_fx: bool,
    /// Apply EBU R128 loudness normalization.
    ///
    /// Reverie found the real hazard here is not the resample but a **4.1 LU
    /// spread** across cori / Kokoro / Kokoro+FX, plainly audible as a level jump
    /// at segment joins. `loudnorm` closes it to 0.7 LU, below the ~1 LU JND, and
    /// lands the chapter inside the ACX window (spike Part 2 §2.6).
    pub loudnorm: bool,
}

impl PostProcess {
    /// Resample only.
    pub fn plain() -> Self {
        Self::default()
    }

    /// The default for narration and dialogue: normalize, then resample.
    pub fn normalized() -> Self {
        Self {
            system_fx: false,
            loudnorm: true,
        }
    }

    /// The `SYSTEM` stage: colour, normalize, resample — all in one ffmpeg pass.
    pub fn system_voice() -> Self {
        Self {
            system_fx: true,
            loudnorm: true,
        }
    }

    pub fn with_loudnorm(mut self) -> Self {
        self.loudnorm = true;
        self
    }

    pub fn without_loudnorm(mut self) -> Self {
        self.loudnorm = false;
        self
    }

    pub fn with_system_fx(mut self) -> Self {
        self.system_fx = true;
        self
    }

    /// Choose the stage from the speaker kind.
    pub fn for_kind(kind: litrpg_core::SpeakerKind) -> Self {
        match kind {
            litrpg_core::SpeakerKind::System => Self::system_voice(),
            _ => Self::normalized(),
        }
    }

    /// The `-af` argument, or empty when nothing is to be done.
    ///
    /// Order is load-bearing: colouring first (its `acompressor` drives the level
    /// hot), loudness normalization second so it measures the final signal. The
    /// resample is not a filter — it is `-ar 16000` on the output, so ffmpeg's
    /// soxr handles the non-integer 22 050→16 000 and 24 000→16 000 ratios.
    pub fn filter_chain(&self, native_rate: u32) -> String {
        let mut stages: Vec<String> = Vec::new();
        if self.system_fx {
            // Reverie's verified chain, parameterized by the source rate: she
            // measured it at 24 000 Hz (Kokoro), and hardcoding that constant
            // would pitch-shift a 22 050 Hz Piper render by 8.8%. asetrate drops
            // the pitch, atempo restores the duration.
            stages.push(format!(
                "asetrate={r}*0.92,aresample={r},atempo=1/0.92,\
                 tremolo=f=60:d=0.7,highpass=f=180,lowpass=f=5200,acompressor",
                r = native_rate
            ));
        }
        if self.loudnorm {
            stages.push("loudnorm=I=-20:TP=-2:LRA=7".to_string());
        }
        stages.join(",")
    }
}

/// Turns native-rate float samples into the 16 kHz contract.
pub trait PostProcessor: Send + Sync {
    fn process(&self, samples: &[f32], native_rate: u32, pp: PostProcess) -> Result<Pcm16k>;
}

/// The default [`PostProcessor`]: one `ffmpeg` subprocess per segment, samples in
/// on stdin and raw PCM out on stdout — no temp files.
#[derive(Debug, Clone)]
pub struct FfmpegPostProcessor {
    pub ffmpeg: String,
}

impl Default for FfmpegPostProcessor {
    fn default() -> Self {
        Self {
            ffmpeg: std::env::var("LITRPG_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string()),
        }
    }
}

impl FfmpegPostProcessor {
    pub fn new(ffmpeg: impl Into<String>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
        }
    }

    /// Whether the binary can be executed. Used by `available()` so a missing
    /// ffmpeg is reported at startup rather than as a failed chapter.
    pub fn is_available(&self) -> bool {
        Command::new(&self.ffmpeg)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// The full argv, exposed for diagnostics and tests.
    pub fn args(&self, native_rate: u32, pp: PostProcess) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-v".into(),
            "error".into(),
            // Input: bare f32 little-endian mono at the model's native rate.
            "-f".into(),
            "f32le".into(),
            "-ar".into(),
            native_rate.to_string(),
            "-ac".into(),
            "1".into(),
            "-i".into(),
            "pipe:0".into(),
        ];
        let chain = pp.filter_chain(native_rate);
        if !chain.is_empty() {
            args.push("-af".into());
            args.push(chain);
        }
        // Output: headerless 16 kHz mono s16le. `-f s16le` is what makes it
        // headerless — a `.pcm` extension alone does not, and a leaked 44-byte
        // RIFF header reads as ~1.4 ms of garbage on the watch.
        args.extend([
            "-ar".into(),
            "16000".into(),
            "-ac".into(),
            "1".into(),
            "-f".into(),
            "s16le".into(),
            "-acodec".into(),
            "pcm_s16le".into(),
            "pipe:1".into(),
        ]);
        args
    }

    /// Process raw bytes already in some pcm-ish input format. Split out so the
    /// sherpa plugin can avoid a second copy of the sample buffer.
    fn run(&self, input: &[u8], native_rate: u32, pp: PostProcess) -> Result<Pcm16k> {
        let mut child = Command::new(&self.ffmpeg)
            .args(self.args(native_rate, pp))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| TtsError::FfmpegSpawn {
                path: self.ffmpeg.clone(),
                source,
            })?;

        // ffmpeg's output can exceed the pipe buffer, so drain stdout on another
        // thread while stdin is being fed — otherwise both sides deadlock on a
        // long segment.
        let mut stdin = child.stdin.take().expect("stdin piped");
        let owned: Vec<u8> = input.to_vec();
        let writer = std::thread::spawn(move || {
            let r = stdin.write_all(&owned);
            drop(stdin);
            r
        });

        let out = child
            .wait_with_output()
            .map_err(|source| TtsError::FfmpegSpawn {
                path: self.ffmpeg.clone(),
                source,
            })?;

        // A broken pipe here is ffmpeg exiting early; the status check below
        // reports the real reason, so don't mask it with the write error.
        let write_failed = matches!(writer.join(), Ok(Err(_)) | Err(_));

        if !out.status.success() {
            return Err(TtsError::Ffmpeg {
                stage: format!(
                    "ffmpeg {}→16k{}{}",
                    native_rate,
                    if pp.system_fx { " +fx" } else { "" },
                    if pp.loudnorm { " +loudnorm" } else { "" }
                ),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(500)
                    .collect(),
            });
        }
        if write_failed && out.stdout.is_empty() {
            return Err(TtsError::Ffmpeg {
                stage: "ffmpeg stdin".into(),
                status: "write failed".into(),
                stderr: String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(500)
                    .collect(),
            });
        }

        Pcm16k::new(out.stdout).map_err(Into::into)
    }
}

impl PostProcessor for FfmpegPostProcessor {
    fn process(&self, samples: &[f32], native_rate: u32, pp: PostProcess) -> Result<Pcm16k> {
        if samples.is_empty() {
            return Ok(Pcm16k::empty());
        }
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.run(&bytes, native_rate, pp)
    }
}
