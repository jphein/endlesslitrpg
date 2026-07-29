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
    let model = as_str(&cfg.onnx_path(m)?);
    let tokens = as_str(&cfg.asset(m, "tokens.txt")?);
    let data_dir = cfg.optional_asset(m, "espeak-ng-data");

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
            // `dict_dir` is **deprecated** for kokoro-multi-lang-v1_0 as of
            // sherpa-onnx >= 1.12.15: passing it prints a stderr warning and is
            // ignored. Upstream's own example still sets it; don't.
            dict_dir: String::new(),
            // Absolute, comma-joined — relative entries resolve against the
            // process CWD rather than the model directory and silently degrade
            // pronunciation instead of failing.
            lexicon: cfg.kokoro_lexicons(m),
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

    fn render(&mut self, req: &RenderRequest) -> Result<Pcm16k> {
        if req.is_blank() {
            return Ok(Pcm16k::empty());
        }
        let cfg = Arc::clone(&self.cfg);
        let (sel, model) = cfg.resolve(req.voice_remainder())?;
        let model = model.clone();
        let (samples, reported_rate) =
            self.engine(&model)?
                .create(&req.text, sel.sid, cfg.speed)?;

        // Trust the rate the engine reports over the configured one — a model
        // swap in the config table shouldn't be able to cause a pitch shift.
        let native_rate = if reported_rate == 0 {
            model.native_rate
        } else {
            reported_rate
        };

        // Resample to the 16 kHz boundary contract, colour SYSTEM blocks, and
        // loudness-normalize — one ffmpeg pass, <2% of synthesis cost.
        self.post
            .process(&samples, native_rate, PostProcess::for_kind(req.kind))
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
        for (wi, positions) in shard(reqs.len(), self.workers.len()).into_iter().enumerate() {
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
