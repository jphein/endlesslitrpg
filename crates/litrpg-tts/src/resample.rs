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
            stages.push(LOUDNORM.to_string());
        }
        stages.join(",")
    }

    /// Pass-one chain: identical filters, plus JSON statistics.
    fn measure_chain(&self, native_rate: u32) -> String {
        let mut chain = self.without_loudnorm().filter_chain(native_rate);
        if !chain.is_empty() {
            chain.push(',');
        }
        chain.push_str(LOUDNORM);
        chain.push_str(":print_format=json");
        chain
    }

    /// Pass-two chain: `loudnorm` given pass one's measurements, so it applies a
    /// known correction instead of adapting blind. `linear=true` asks for a single
    /// gain change across the whole segment rather than dynamic movement, which is
    /// what keeps a narration segment sounding un-pumped.
    fn apply_chain(&self, native_rate: u32, s: &LoudnormStats) -> String {
        let mut chain = self.without_loudnorm().filter_chain(native_rate);
        if !chain.is_empty() {
            chain.push(',');
        }
        chain.push_str(&format!(
            "{LOUDNORM}:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}\
             :measured_thresh={:.2}:offset={:.2}:linear=true",
            s.input_i, s.input_tp, s.input_lra, s.input_thresh, s.target_offset
        ));
        chain
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

    /// The measurement half of two-pass `loudnorm`: same filters, output discarded,
    /// statistics printed as JSON.
    ///
    /// `-v info` is deliberate. `loudnorm`'s `print_format=json` block is logged at
    /// INFO, so `-v error` would suppress it and this pass would silently measure
    /// nothing — the same trap Reverie hit with `ebur128`.
    fn args_measure(&self, native_rate: u32, pp: PostProcess) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-nostats".into(),
            "-v".into(),
            "info".into(),
            "-f".into(),
            "f32le".into(),
            "-ar".into(),
            native_rate.to_string(),
            "-ac".into(),
            "1".into(),
            "-i".into(),
            "pipe:0".into(),
            "-af".into(),
            pp.measure_chain(native_rate),
            "-f".into(),
            "null".into(),
            "-".into(),
        ];
        args.retain(|a| !a.is_empty());
        args
    }

    /// The applying half: `loudnorm` fed the measured statistics, so it corrects
    /// with full knowledge of the signal instead of streaming blind.
    fn args_apply(&self, native_rate: u32, pp: PostProcess, stats: &LoudnormStats) -> Vec<String> {
        self.args_with_chain(native_rate, &pp.apply_chain(native_rate, stats))
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
                stderr: stderr_text.chars().rev().take(600).collect::<String>()
                    .chars().rev().collect(),
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

        if !pp.loudnorm {
            let (out, _) = self.run_pass(&bytes, self.args(native_rate, pp), "resample")?;
            return self.finish(out);
        }

        // Two-pass loudnorm. Single-pass `loudnorm` is a *streaming* normalizer: it
        // corrects as it goes, with only its lookahead buffer to work from. On the
        // 2-3 s segments a chapter is made of that is not enough to converge —
        // measured a 2.0 LU spread across cori / Kokoro / Kokoro+FX, above the ~1 LU
        // just-noticeable difference and plainly audible at a segment join. Reverie's
        // 0.7 LU figure came from 8-13 s segments, which is why single-pass looked
        // sufficient in the spike. Measuring first and then correcting with the real
        // statistics is what makes short segments level.
        //
        // Cost is one extra ffmpeg invocation per segment, still far under 2% of
        // synthesis. The FX chain runs in *both* passes so pass one measures the
        // coloured signal, not the raw one.
        let (_, stderr) = self.run_pass(
            &bytes,
            self.args_measure(native_rate, pp),
            "loudnorm measure",
        )?;

        match LoudnormStats::parse(&stderr) {
            Some(stats) => {
                let (out, _) = self.run_pass(
                    &bytes,
                    self.args_apply(native_rate, pp, &stats),
                    "loudnorm apply",
                )?;
                self.finish(out)
            }
            // Near-silent input measures as `-inf`; correcting from that is worse
            // than not correcting. Degrade to the single streaming pass.
            None => {
                let (out, _) =
                    self.run_pass(&bytes, self.args(native_rate, pp), "resample (1-pass)")?;
                self.finish(out)
            }
        }
    }
}
