//! The worker pool that links `sherpa-rs`. Behind the `sherpa` feature.

use super::{ModelDesc, ModelFamily, SherpaConfig, as_str, shard};
use crate::backend::{Availability, RenderRequest, TtsBackend, VoiceDesc};
use crate::error::{Result, TtsError};
use crate::pcm::Pcm16k;
use crate::resample::{FfmpegPostProcessor, PostProcess, PostProcessor};
use async_trait::async_trait;
use sherpa_rs::OnnxConfig;
use sherpa_rs::tts::{CommonTtsConfig, KokoroTts, KokoroTtsConfig, VitsTts, VitsTtsConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One loaded model. Both families expose the identical `(text, sid, speed)`
/// primitive, which is why per-character casting is a config concern.
enum Engine {
    Vits(VitsTts),
    Kokoro(KokoroTts),
}

impl Engine {
    fn create(&mut self, text: &str, sid: i32, speed: f32) -> Result<(Vec<f32>, u32)> {
        let audio = match self {
            Engine::Vits(t) => t.create(text, sid, speed),
            Engine::Kokoro(t) => t.create(text, sid, speed),
        }
        .map_err(|e| TtsError::Synthesis(e.to_string()))?;
        Ok((audio.samples, audio.sample_rate))
    }
}

fn build_engine(cfg: &SherpaConfig, m: &ModelDesc) -> Result<Engine> {
    let onnx = OnnxConfig {
        provider: cfg.provider.clone(),
        debug: false,
        // Per worker, not per host. 4 is the measured optimum; setting this to
        // core count regresses because of contention with qwen3-coder.
        num_threads: cfg.threads_per_worker,
    };
    // ⚠️ Everything here is `asset()` (required, returns Err) rather than
    // `optional_asset()` (returns ""), because sherpa-onnx validates several of
    // these by calling **`exit()`** instead of returning an error. Checking in Rust
    // converts an unkillable process abort into a catchable
    // `TtsError::ModelMissing`. See `SherpaConfig::required_assets`.
    let model = as_str(&cfg.onnx_path(m)?);
    let tokens = as_str(&cfg.asset(m, "tokens.txt")?);
    let data_dir = as_str(&cfg.asset(m, "espeak-ng-data")?);

    // One sentence per synthesis call: segments are already sentence- or
    // paragraph-sized, and batching sentences inside a call costs the caller the
    // per-segment boundaries it needs for the manifest.
    let common = CommonTtsConfig {
        max_num_sentences: 1,
        ..Default::default()
    };

    Ok(match m.family {
        ModelFamily::Piper => Engine::Vits(VitsTts::new(VitsTtsConfig {
            model,
            tokens,
            data_dir,
            lexicon: String::new(),
            dict_dir: String::new(),
            // VITS defaults; `#[derive(Default)]` would leave these at 0.0, which
            // the C side reads as a degenerate voice.
            length_scale: 1.0,
            noise_scale: 0.667,
            noise_scale_w: 0.8,
            silence_scale: 0.2,
            onnx_config: onnx,
            tts_config: common,
        })),
        ModelFamily::Kokoro => Engine::Kokoro(KokoroTts::new(KokoroTtsConfig {
            model,
            voices: as_str(&cfg.asset(m, "voices.bin")?),
            tokens,
            data_dir,
            // ⚠️⚠️ **Do not "fix" this to an empty string.** Reverie's Python spike
            // (sherpa-onnx 1.13.4) found `dict_dir` deprecated-and-ignored for
            // kokoro-multi-lang-v1_0, so omitting it looked correct — and JP's
            // VoxSherpa-TTS and upstream's own `tts_kokoro.rs` both still set it,
            // which read as stale code but was actually the answer.
            //
            // The core bundled by sherpa-rs 0.6.8 is OLDER and **requires** it. With
            // it empty, InitFrontend logs "please pass --kokoro-lexicon and
            // --kokoro-dict-dir" and then **aborts the process (exit 255)** — not a
            // recoverable Err, so nothing upstream can catch or degrade from it.
            // Passing it is safe on both versions (newer ones warn and ignore), so
            // it is always passed. `asset()` not `optional_asset()`: a missing dict/
            // must be a catchable Err, never a silent empty string that reaches C++.
            dict_dir: as_str(&cfg.asset(m, "dict")?),
            // Absolute, comma-joined — relative entries resolve against the
            // process CWD rather than the model directory and silently degrade
            // pronunciation instead of failing. An empty list is the other half of
            // the "please pass --kokoro-lexicon" abort, so refuse it in Rust.
            lexicon: {
                let lex = cfg.kokoro_lexicons(m);
                if lex.is_empty() {
                    return Err(TtsError::ModelMissing(format!(
                        "{}: none of {:?} found; an empty Kokoro lexicon list aborts sherpa-onnx",
                        cfg.model_dir(m).display(),
                        cfg.kokoro_lexicon_files
                    )));
                }
                lex
            },
            length_scale: 1.0,
            onnx_config: onnx,
            common_config: common,
            lang: "en-us".to_string(),
        })),
    })
}

/// One pool worker: its own ONNX sessions, its own ffmpeg invocations.
///
/// A worker holds **every model it is asked for**, lazily. Reverie measured zero
/// reload penalty for alternating cori and Kokoro inside one process (spike
/// Part 2 §2.7), so any worker can take any segment and scheduling stays trivial.
/// Memory is ~120 MB for cori alone and ~542 MB with both resident, so a pool of
/// 4 is ~2.2 GB — comfortable on `familiar`.
struct Worker {
    cfg: Arc<SherpaConfig>,
    engines: HashMap<String, Engine>,
    post: FfmpegPostProcessor,
}

impl Worker {
    fn engine(&mut self, m: &ModelDesc) -> Result<&mut Engine> {
        if !self.engines.contains_key(&m.id) {
            let engine = build_engine(&self.cfg, m)?;
            self.engines.insert(m.id.clone(), engine);
        }
        Ok(self
            .engines
            .get_mut(&m.id)
            .expect("just inserted this model's engine"))
    }

    /// Synthesis only — no resample, no colouring, no normalization. Returns the
    /// engine's own reported rate and the wall time of inference alone, which is
    /// what an honest RTF figure needs.
    fn synthesize(&mut self, remainder: &str, text: &str) -> Result<NativeRender> {
        let cfg = Arc::clone(&self.cfg);
        let (sel, model) = cfg.resolve(remainder)?;
        let model = model.clone();

        // Build (or fetch) the engine *outside* the timed region: a model load is
        // ~1.5 s for cori-high and would otherwise be charged to the first
        // segment's synthesis, making a cold RTF look ~12x worse than it is.
        let engine = self.engine(&model)?;
        let started = Instant::now();
        let (samples, reported_rate) = engine.create(text, sel.sid, cfg.speed)?;
        let synth_wall = started.elapsed();

        // Trust the rate the engine reports over the configured one — a model swap
        // in the config table shouldn't be able to cause a pitch shift.
        let sample_rate = if reported_rate == 0 {
            model.native_rate
        } else {
            reported_rate
        };

        Ok(NativeRender {
            model_id: model.id,
            sid: sel.sid,
            samples,
            sample_rate,
            synth_wall,
        })
    }

    fn render(&mut self, req: &RenderRequest) -> Result<Pcm16k> {
        if req.is_blank() {
            return Ok(Pcm16k::empty());
        }
        let native = self.synthesize(req.voice_remainder(), &req.text)?;

        // Resample to the 16 kHz boundary contract, colour SYSTEM blocks, and
        // loudness-normalize — one ffmpeg pass, <2% of synthesis cost.
        self.post.process(
            &native.samples,
            native.sample_rate,
            PostProcess::for_kind(req.kind),
        )
    }
}

/// Raw synthesis output, before the 16 kHz boundary contract is applied.
///
/// Exists because `sherpa-rs` 0.6.8 exposes a model's sample rate **only** on a
/// generated `TtsAudio` — there is no `num_speakers()` or `sample_rate()` accessor
/// on the TTS handle. So the only way to confirm from Rust that cori really is
/// 22 050 Hz is to generate and look, which is what this returns.
#[derive(Debug, Clone)]
pub struct NativeRender {
    pub model_id: String,
    pub sid: i32,
    pub samples: Vec<f32>,
    /// The rate the **engine** reported, not the one the config table claims.
    pub sample_rate: u32,
    /// Inference only — excludes ffmpeg resample, colouring and normalization.
    pub synth_wall: Duration,
}

impl NativeRender {
    pub fn audio_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / self.sample_rate as f64
    }

    /// Seconds of audio produced per second of wall clock. Higher is better.
    pub fn rtf(&self) -> f64 {
        let wall = self.synth_wall.as_secs_f64();
        if wall == 0.0 {
            return f64::INFINITY;
        }
        self.audio_secs() / wall
    }

    /// Peak absolute amplitude as an i16, for "is this actually audio" checks.
    pub fn peak_i16(&self) -> u16 {
        self.samples
            .iter()
            .map(|s| (s.abs().min(1.0) * 32_767.0) as u16)
            .max()
            .unwrap_or(0)
    }
}

/// The sherpa-onnx backend: a pool of independent workers sharded by segment.
pub struct SherpaBackend {
    cfg: Arc<SherpaConfig>,
    workers: Vec<Arc<Mutex<Worker>>>,
    /// Computed once at construction. `available()` is called on every dispatch,
    /// and probing `ffmpeg -version` per segment would be absurd; call
    /// [`SherpaBackend::refresh_availability`] after installing models.
    availability: Availability,
}

impl SherpaBackend {
    pub fn new(cfg: SherpaConfig) -> Self {
        let availability = cfg.availability();
        let cfg = Arc::new(cfg);
        let post = FfmpegPostProcessor::default();
        let workers = (0..cfg.workers.max(1))
            .map(|_| {
                Arc::new(Mutex::new(Worker {
                    cfg: Arc::clone(&cfg),
                    engines: HashMap::new(),
                    post: post.clone(),
                }))
            })
            .collect();
        Self {
            cfg,
            workers,
            availability,
        }
    }

    /// Build with defaults, or from `$LITRPG_SHERPA_CONFIG` when set.
    pub fn from_default_config() -> Result<Self> {
        match std::env::var("LITRPG_SHERPA_CONFIG") {
            Ok(path) => {
                let s = std::fs::read_to_string(&path).map_err(|source| TtsError::ConfigRead {
                    path: path.clone(),
                    source,
                })?;
                Ok(Self::new(SherpaConfig::from_json_str(&s)?))
            }
            Err(_) => Ok(Self::new(SherpaConfig::default())),
        }
    }

    pub fn config(&self) -> &SherpaConfig {
        &self.cfg
    }

    /// Re-probe models and ffmpeg.
    pub fn refresh_availability(&mut self) {
        self.availability = self.cfg.availability();
    }

    /// Number of pool workers.
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// **Diagnostic only.** Synthesize without post-processing and report the
    /// engine's own sample rate plus inference-only wall time.
    ///
    /// Added purely so the live verification suite can confirm native rates and
    /// measure RTF from Rust — `sherpa-rs` 0.6.8 has no `sample_rate()` or
    /// `num_speakers()` accessor on a TTS handle, so generating is the only way to
    /// observe either. The render path does not use this; it is not a second
    /// pipeline.
    pub async fn probe(&self, voice_remainder: &str, text: &str) -> Result<NativeRender> {
        let worker = Arc::clone(&self.workers[0]);
        let remainder = voice_remainder.to_string();
        let text = text.to_string();
        tokio::task::spawn_blocking(move || {
            let mut w = worker.lock().unwrap_or_else(|e| e.into_inner());
            w.synthesize(&remainder, &text)
        })
        .await
        .map_err(|e| TtsError::Worker(e.to_string()))?
    }
}

#[async_trait]
impl TtsBackend for SherpaBackend {
    fn id(&self) -> &str {
        "sherpa"
    }

    fn available(&self) -> Availability {
        self.availability.clone()
    }

    fn voices(&self) -> Vec<VoiceDesc> {
        self.cfg.voice_descs()
    }

    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k> {
        Ok(self
            .render_batch(std::slice::from_ref(req))
            .await?
            .pop()
            .unwrap_or_default())
    }

    /// Fan out across the worker pool, **sharded by segment**.
    ///
    /// Round-robin rather than contiguous chunks, so a chapter whose long
    /// narration blocks cluster together still spreads across the pool. Each
    /// worker processes its shard sequentially on a blocking thread — ONNX
    /// inference is CPU-bound and must never run on a tokio runtime worker.
    async fn render_batch(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>> {
        if reqs.is_empty() {
            return Ok(Vec::new());
        }
        // Resolve every voice before loading a model or synthesizing anything: a
        // bad cast assignment should cost nothing (spec §7.3).
        for r in reqs {
            self.cfg.resolve(r.voice_remainder())?;
        }

        let mut set = tokio::task::JoinSet::new();
        for (wi, positions) in shard(reqs.len(), self.workers.len())
            .into_iter()
            .enumerate()
        {
            if positions.is_empty() {
                continue;
            }
            let worker = Arc::clone(&self.workers[wi % self.workers.len()]);
            let batch: Vec<(usize, RenderRequest)> =
                positions.iter().map(|&p| (p, reqs[p].clone())).collect();
            set.spawn_blocking(move || {
                // A poisoned lock means a previous panic; the engine's state
                // lives on the C side, so recovering is safe and better than
                // taking the whole pool down.
                let mut w = worker.lock().unwrap_or_else(|e| e.into_inner());
                let mut out = Vec::with_capacity(batch.len());
                for (pos, req) in batch {
                    out.push((pos, w.render(&req)?));
                }
                Ok::<_, TtsError>(out)
            });
        }

        let mut slots: Vec<Option<Pcm16k>> = vec![None; reqs.len()];
        while let Some(joined) = set.join_next().await {
            for (pos, pcm) in joined.map_err(|e| TtsError::Worker(e.to_string()))?? {
                slots[pos] = Some(pcm);
            }
        }
        Ok(slots.into_iter().map(Option::unwrap_or_default).collect())
    }
}
