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
    /// at segment joins. `loudnorm` closes it to well under the ~1 LU JND and lands
    /// the chapter inside the ACX window (spike Part 2 §2.6) — **provided it runs as
    /// its own pass** rather than fused into the FX graph.
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

    /// The `SYSTEM` stage: colour + resample, then normalize as a separate pass.
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

    /// The **stage-1** `-af` argument: the `SYSTEM` colouring, or empty.
    ///
    /// `loudnorm` is deliberately **not** here. It runs as its own ffmpeg invocation
    /// against the finished 16 kHz stream — see [`FfmpegPostProcessor`] for the
    /// measurement that forced the split. The resample is not a filter either; it is
    /// `-ar 16000` on the output, so ffmpeg's soxr handles the non-integer
    /// 22 050→16 000 and 24 000→16 000 ratios.
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
        stages.join(",")
    }
}

/// EBU R128 target. `I=-20` sits inside the ACX audiobook window (−18 to −23 LUFS).
const LOUDNORM: &str = "loudnorm=I=-20:TP=-2:LRA=7";

/// Turns native-rate float samples into the 16 kHz contract.
///
/// Implementations must return **whole-millisecond-aligned** audio, i.e.
/// [`Pcm16k::is_whole_ms`] holds, so `duration_ms() * 32 == len()` is exact for
/// everything downstream.
pub trait PostProcessor: Send + Sync {
    fn process(&self, samples: &[f32], native_rate: u32, pp: PostProcess) -> Result<Pcm16k>;
}

/// The default [`PostProcessor`]. Samples in on stdin, raw PCM out on stdout — no
/// temp files.
///
/// **Two stages, deliberately not one.** Stage 1 fuses the `SYSTEM` colouring with
/// the resample; stage 2 runs `loudnorm` on its own, against the finished 16 kHz
/// stream. That is exactly how Reverie measured it (spike Part 2 **§2.5** fuses FX
/// with the *resample*; **§2.6** runs `loudnorm` as a standalone invocation), and
/// the split is load-bearing:
///
/// Appending `loudnorm` downstream of `acompressor,highpass,lowpass` in a single
/// filter graph made the SYSTEM segment land at **−23.1 LUFS** instead of −20 — a
/// 3.3 LU spread, audible as "the system voice is oddly quiet". `loudnorm`
/// estimating gain over a signal whose dynamics were just crushed and whose
/// spectrum was just band-limited undershoots badly on a short segment.
///
/// **Do not "optimise" these back into one pass.** The extra invocation is cheap
/// (see [`FfmpegPostProcessor::loudnorm_two_pass`]) and fusing silently
/// reintroduces a level defect that only shows up dozens of chapters in.
#[derive(Debug, Clone)]
pub struct FfmpegPostProcessor {
    pub ffmpeg: String,
    /// Whether stage 2 measures before correcting.
    ///
    /// Two-pass is strictly more accurate — it hands `loudnorm` the real
    /// `measured_I/TP/LRA/thresh` instead of letting it adapt as it streams — and on
    /// the 2–3 s segments a chapter is made of that accuracy is what keeps short
    /// segments on target. Costs one more ffmpeg invocation per segment.
    pub loudnorm_two_pass: bool,
}

impl Default for FfmpegPostProcessor {
    fn default() -> Self {
        Self {
            ffmpeg: std::env::var("LITRPG_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string()),
            // Overridable so the two options can be measured against each other on
            // real audio, and so an operator can trade accuracy for wall clock.
            loudnorm_two_pass: std::env::var("LITRPG_LOUDNORM_TWO_PASS")
                .map(|v| v != "0")
                .unwrap_or(true),
        }
    }
}

impl FfmpegPostProcessor {
    pub fn new(ffmpeg: impl Into<String>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ..Self::default()
        }
    }

    /// Stage 2 argv: `loudnorm` alone, 16 kHz s16le in and out.
    ///
    /// `chain` is the loudnorm spec — bare for a measuring pass, or carrying
    /// `measured_*` for an applying one. `verbose` raises the log level so
    /// `print_format=json` survives (it logs at INFO, and `-v error` would drop it —
    /// the same trap Reverie hit with `ebur128`).
    fn args_loudnorm(&self, chain: &str, verbose: bool, discard: bool) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-nostats".into(),
            "-v".into(),
            if verbose {
                "info".into()
            } else {
                "error".into()
            },
            "-f".into(),
            "s16le".into(),
            "-ar".into(),
            "16000".into(),
            "-ac".into(),
            "1".into(),
            "-i".into(),
            "pipe:0".into(),
            "-af".into(),
            chain.to_string(),
        ];
        if discard {
            args.extend(["-f".into(), "null".into(), "-".into()]);
        } else {
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
        }
        args
    }

    /// Loudness-normalize an **already 16 kHz** buffer, with no resample.
    ///
    /// This is the seam the Azure path needs: Azure serves 16 kHz natively, so it
    /// never goes through `process()` — and consequently was never normalized at all.
    /// Measured 2026-07-29 on a real chapter, `en-US-Steffan` came back **4.9 LU
    /// quieter than `en-GB-Ada`**, an audible step at every narrator/SYSTEM
    /// transition. Same defect class as the 4.1 LU spread Reverie found across the
    /// sherpa engines; it just had no owner on the Azure side.
    pub fn normalize_16k(&self, pcm: &Pcm16k) -> Result<Pcm16k> {
        if pcm.is_empty() {
            return Ok(Pcm16k::empty());
        }
        Ok(Pcm16k::new(self.apply_loudnorm(pcm.as_bytes())?)?.padded_to_whole_ms())
    }

    /// Stage 2: normalize an already-resampled 16 kHz stream.
    fn apply_loudnorm(&self, pcm16k: &[u8]) -> Result<Vec<u8>> {
        if self.loudnorm_two_pass {
            let (_, stderr) = self.run_pass(
                pcm16k,
                self.args_loudnorm(&format!("{LOUDNORM}:print_format=json"), true, true),
                "loudnorm measure",
            )?;
            // Near-silent input measures `-inf`; correcting from that is worse than
            // not correcting, so fall through to the streaming pass.
            if let Some(s) = LoudnormStats::parse(&stderr) {
                let chain = format!(
                    "{LOUDNORM}:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}\
                     :measured_thresh={:.2}:offset={:.2}:linear=true",
                    s.input_i, s.input_tp, s.input_lra, s.input_thresh, s.target_offset
                );
                let (out, _) = self.run_pass(
                    pcm16k,
                    self.args_loudnorm(&chain, false, false),
                    "loudnorm apply",
                )?;
                return Ok(out);
            }
        }
        let (out, _) = self.run_pass(
            pcm16k,
            self.args_loudnorm(LOUDNORM, false, false),
            "loudnorm 1-pass",
        )?;
        Ok(out)
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
        self.args_with_chain(native_rate, &pp.filter_chain(native_rate))
    }

    /// Build the argv around an explicit filter chain.
    ///
    /// Takes the chain as a parameter rather than splicing it in afterwards: `-ar`
    /// appears in *both* the input and output sections, so "insert before the first
    /// `-ar`" put the filter in the input options and ffmpeg rejected it.
    fn args_with_chain(&self, native_rate: u32, chain: &str) -> Vec<String> {
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
        if !chain.is_empty() {
            args.push("-af".into());
            args.push(chain.to_string());
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

    /// Run one ffmpeg pass, returning `(stdout, stderr)`.
    fn run_pass(&self, input: &[u8], args: Vec<String>, stage: &str) -> Result<(Vec<u8>, String)> {
        let mut child = Command::new(&self.ffmpeg)
            .args(args)
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

        let stderr_text = String::from_utf8_lossy(&out.stderr).into_owned();

        if !out.status.success() {
            return Err(TtsError::Ffmpeg {
                stage: stage.to_string(),
                status: out.status.to_string(),
                stderr: stderr_text
                    .chars()
                    .rev()
                    .take(600)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect(),
            });
        }
        if write_failed && out.stdout.is_empty() && !stage.contains("measure") {
            return Err(TtsError::Ffmpeg {
                stage: format!("{stage} (stdin)"),
                status: "write failed".into(),
                stderr: stderr_text.chars().take(500).collect(),
            });
        }

        Ok((out.stdout, stderr_text))
    }

    /// Align at the boundary, not at the call site. ffmpeg lands on whatever sample
    /// count the resample ratio and `loudnorm`'s filter delay produce, so the engine
    /// would otherwise have to remember to pad every segment — and forgetting is
    /// silent, cumulative manifest drift.
    fn finish(&self, stdout: Vec<u8>) -> Result<Pcm16k> {
        Ok(Pcm16k::new(stdout)?.padded_to_whole_ms())
    }
}

/// `loudnorm`'s first-pass measurements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnormStats {
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub input_thresh: f64,
    pub target_offset: f64,
}

impl LoudnormStats {
    /// Parse the `print_format=json` block out of ffmpeg's stderr.
    ///
    /// Returns `None` when the block is absent or any figure is non-finite. `-inf`
    /// is what a silent or near-silent buffer measures, and feeding that back into
    /// pass two produces garbage — so the caller falls back to a single pass.
    pub fn parse(stderr: &str) -> Option<Self> {
        let start = stderr.rfind('{')?;
        let end = stderr[start..].find('}')? + start + 1;
        let map: std::collections::HashMap<String, String> =
            serde_json::from_str(&stderr[start..end]).ok()?;
        let get = |k: &str| -> Option<f64> {
            let v = map.get(k)?.trim().parse::<f64>().ok()?;
            v.is_finite().then_some(v)
        };
        Some(Self {
            input_i: get("input_i")?,
            input_tp: get("input_tp")?,
            input_lra: get("input_lra")?,
            input_thresh: get("input_thresh")?,
            target_offset: get("target_offset")?,
        })
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

        // Stage 1 — [SYSTEM colouring,] resample to the 16 kHz contract. Reverie's
        // spike Part 2 §2.5: FX and resample compose correctly in one pass.
        let (resampled, _) = self.run_pass(
            &bytes,
            self.args(native_rate, pp.without_loudnorm()),
            "resample",
        )?;

        if !pp.loudnorm {
            return self.finish(resampled);
        }

        // Stage 2 — loudnorm, on its own, against the finished stream (§2.6).
        // Never fused with stage 1: see the type-level comment.
        self.finish(self.apply_loudnorm(&resampled)?)
    }
}
