//! The sherpa-onnx plugin — local, free, unmetered, and a first-class backend
//! alongside Azure (spec §D6).
//!
//! This module is split so that the parts with no native dependency stay
//! testable in a default build:
//!
//! - **Always compiled**: [`SherpaConfig`], [`ModelDesc`], [`SherpaVoice`],
//!   [`VoiceSel`] — the cast table and voice-reference parsing. Voices are
//!   **config, not code**, so changing the narrator (spec §4.4 assumption A1) is
//!   a JSON edit.
//! - **Behind the `sherpa` feature**: [`SherpaBackend`], the worker pool that
//!   actually links `sherpa-rs`. Off by default so a plain `cargo test` never
//!   builds native TTS libraries.
//!
//! # Measured facts this module encodes
//!
//! From Reverie's spike on `familiar` (Ryzen 9 3900X, 12C/24T) — not re-derived:
//!
//! - **4 workers × 4 threads beats 1 × 8 threads by 48–86%.** Thread scaling is
//!   strongly sublinear, so independent sessions extract more aggregate
//!   throughput than one fat process. 12 threads *regresses* from contention with
//!   the GPU-resident `qwen3-coder.service`. Never set threads to core count.
//! - **Speakers are an integer `sid` at synthesis time**, no model reload.
//! - **Zero reload penalty when one worker holds several models** — so shard by
//!   *segment*, not by model (Part 2 §2.7). A model-sharded pool would idle
//!   workers on a narration-heavy or dialogue-heavy chapter.
//! - **Piper emits 22 050 Hz, Kokoro 24 000 Hz**, both resampled to the 16 kHz
//!   boundary contract by [`crate::resample`].

use crate::backend::{Availability, CostClass, Gender, VoiceDesc};
use crate::error::{Result, TtsError};
use serde::{Deserialize, Serialize};
#[cfg(feature = "sherpa")]
use std::path::Path;
use std::path::PathBuf;

#[cfg(feature = "sherpa")]
mod engine;
#[cfg(feature = "sherpa")]
pub use engine::SherpaBackend;

/// Which sherpa model family a directory holds. They take different config
/// structs but the same `(text, sid, speed)` synthesis call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFamily {
    /// Piper / VITS. `cori` and `libritts_r`.
    Piper,
    Kokoro,
}

/// A model on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDesc {
    /// The id used in a `voice_ref` remainder, e.g. `piper-en_GB-cori`.
    pub id: String,
    pub family: ModelFamily,
    /// Directory name under `model_root` — the extracted tarball.
    pub dir: String,
    /// Native output rate, before the resample to 16 kHz.
    pub native_rate: u32,
    /// Speaker count; bounds the legal `sid`.
    pub speakers: u32,
    /// Explicit `.onnx` filename. When absent the single `.onnx` in the directory
    /// is discovered at load time — Piper tarballs name it after the voice
    /// (`en_GB-cori-medium.onnx`) while Kokoro uses `model.onnx`.
    #[serde(default)]
    pub model_file: Option<String>,
}

/// One curated, assignable voice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SherpaVoice {
    /// `voice_ref` remainder: `<model_id>:<sid>`.
    pub voice: String,
    pub label: String,
    pub lang: String,
    pub gender: Gender,
}

/// A parsed `voice_ref` remainder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSel {
    pub model_id: String,
    pub sid: i32,
}

impl VoiceSel {
    /// Parse `<model_id>[:<sid>]`.
    ///
    /// The engine has already split `sherpa:` off the front, so what arrives here
    /// is the plugin's own opaque remainder. A bare model id means speaker 0,
    /// which is the only legal speaker for single-speaker models like cori.
    pub fn parse(remainder: &str) -> Result<Self> {
        let (model_id, sid) = match remainder.rsplit_once(':') {
            Some((m, s)) => (m, Some(s)),
            None => (remainder, None),
        };
        if model_id.is_empty() {
            return Err(TtsError::UnknownVoice {
                backend: "sherpa".into(),
                voice: remainder.into(),
                reason: "model id is empty".into(),
            });
        }
        let sid = match sid {
            None => 0,
            Some(s) => s.parse::<i32>().map_err(|_| TtsError::UnknownVoice {
                backend: "sherpa".into(),
                voice: remainder.into(),
                reason: format!("sid '{s}' is not a non-negative integer"),
            })?,
        };
        if sid < 0 {
            return Err(TtsError::UnknownVoice {
                backend: "sherpa".into(),
                voice: remainder.into(),
                reason: format!("sid {sid} is negative"),
            });
        }
        Ok(Self {
            model_id: model_id.to_string(),
            sid,
        })
    }
}

fn default_model_root() -> PathBuf {
    if let Ok(p) = std::env::var("LITRPG_SHERPA_MODELS") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/share/litrpg/models")
}

fn default_workers() -> usize {
    4
}
fn default_threads() -> i32 {
    4
}
fn default_provider() -> String {
    "cpu".to_string()
}
fn default_speed() -> f32 {
    1.0
}
fn default_narrator() -> String {
    "piper-en_GB-cori:0".to_string()
}
fn default_system_voice() -> String {
    // The neutral speaker Reverie actually coloured in the mixed-model stitch.
    "kokoro-multi-lang-v1_0:18".to_string()
}

/// The sherpa plugin's configuration and cast table.
///
/// Every field has a default, so a partial JSON override is legal and adding a
/// model or a voice needs no code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SherpaConfig {
    /// Directory holding the extracted model tarballs. Defaults to
    /// `$LITRPG_SHERPA_MODELS`, else `~/.local/share/litrpg/models`. On `familiar`
    /// the spike left them in `~/tts-spike/models`.
    pub model_root: PathBuf,
    /// Concurrent worker count. 4 is the measured optimum.
    pub workers: usize,
    /// ONNX intra-op threads **per worker**. 4 is the measured optimum; 8 in a
    /// single process is slower in aggregate and 12 regresses outright.
    pub threads_per_worker: i32,
    /// ONNX provider. Stays `cpu` so the renderer never contends with the
    /// GPU-resident `qwen3-coder.service`.
    pub provider: String,
    pub speed: f32,
    /// `voice_ref` remainder for the narrator.
    pub narrator: String,
    /// `voice_ref` remainder of the neutral speaker used for `SYSTEM` blocks
    /// before the ffmpeg colouring.
    pub system_voice: String,
    /// Kept as an explicit `false` rather than an absence: Reverie measured that
    /// sharding a pool by model is the wrong topology.
    pub shard_by_model: bool,
    models: Vec<ModelDesc>,
    voices: Vec<SherpaVoice>,
}

impl Default for SherpaConfig {
    fn default() -> Self {
        Self {
            model_root: default_model_root(),
            workers: default_workers(),
            threads_per_worker: default_threads(),
            provider: default_provider(),
            speed: default_speed(),
            narrator: default_narrator(),
            system_voice: default_system_voice(),
            shard_by_model: false,
            models: default_models(),
            voices: default_voices(),
        }
    }
}

impl SherpaConfig {
    /// Parse a config document. Absent fields keep their defaults; a `models` or
    /// `voices` array **replaces** the default table rather than merging, so a
    /// deployment can express an exact cast.
    pub fn from_json_str(s: &str) -> Result<Self> {
        let mut cfg: Self = serde_json::from_str(s).map_err(|source| TtsError::ConfigParse {
            path: "<sherpa config json>".to_string(),
            source,
        })?;
        cfg.workers = cfg.workers.max(1);
        cfg.threads_per_worker = cfg.threads_per_worker.max(1);
        if cfg.speed <= 0.0 {
            cfg.speed = 1.0;
        }
        Ok(cfg)
    }

    pub fn models(&self) -> &[ModelDesc] {
        &self.models
    }

    pub fn voices(&self) -> &[SherpaVoice] {
        &self.voices
    }

    pub fn model(&self, id: &str) -> Option<&ModelDesc> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Absolute directory for a model.
    pub fn model_dir(&self, m: &ModelDesc) -> PathBuf {
        self.model_root.join(&m.dir)
    }

    /// Fully-qualified narrator reference (spec §7.3 default).
    pub fn narrator_voice_ref(&self) -> String {
        format!("sherpa:{}", self.narrator)
    }

    /// Fully-qualified neutral speaker for `SYSTEM` blocks.
    pub fn system_voice_ref(&self) -> String {
        format!("sherpa:{}", self.system_voice)
    }

    /// The catalog this plugin advertises.
    pub fn voice_descs(&self) -> Vec<VoiceDesc> {
        self.voices
            .iter()
            .map(|v| VoiceDesc {
                voice_ref: format!("sherpa:{}", v.voice),
                label: v.label.clone(),
                lang: v.lang.clone(),
                gender: v.gender,
                cost_class: CostClass::Free,
            })
            .collect()
    }

    /// Resolve a remainder to its model, checking the `sid` against the speaker
    /// count so a bad cast assignment fails before any synthesis happens.
    pub fn resolve(&self, remainder: &str) -> Result<(VoiceSel, &ModelDesc)> {
        let sel = VoiceSel::parse(remainder)?;
        let model = self
            .model(&sel.model_id)
            .ok_or_else(|| TtsError::UnknownVoice {
                backend: "sherpa".into(),
                voice: remainder.to_string(),
                reason: format!("no model '{}' is configured", sel.model_id),
            })?;
        if (sel.sid as u32) >= model.speakers {
            return Err(TtsError::UnknownVoice {
                backend: "sherpa".into(),
                voice: remainder.to_string(),
                reason: format!(
                    "sid {} out of range; '{}' has {} speakers",
                    sel.sid, model.id, model.speakers
                ),
            });
        }
        Ok((sel, model))
    }

    /// Whether the plugin can run: model root present, at least one configured
    /// model directory present, and `ffmpeg` runnable for the resample.
    ///
    /// The reason string always names the offending path — an unavailable backend
    /// is only actionable if it says what to install where.
    pub fn availability(&self) -> Availability {
        if !self.model_root.is_dir() {
            return Availability::missing(format!(
                "sherpa model root not found: {}",
                self.model_root.display()
            ));
        }
        let present: Vec<&ModelDesc> = self
            .models
            .iter()
            .filter(|m| self.model_dir(m).is_dir())
            .collect();
        if present.is_empty() {
            return Availability::missing(format!(
                "no configured model directories under {}",
                self.model_root.display()
            ));
        }
        if !crate::resample::FfmpegPostProcessor::default().is_available() {
            return Availability::missing(
                "ffmpeg not runnable; sherpa output cannot be resampled to 16 kHz".to_string(),
            );
        }
        Availability::Ready
    }

    /// Discover the `.onnx` file for a model.
    pub fn onnx_path(&self, m: &ModelDesc) -> Result<PathBuf> {
        let dir = self.model_dir(m);
        if let Some(f) = &m.model_file {
            let p = dir.join(f);
            return p
                .is_file()
                .then_some(p.clone())
                .ok_or_else(|| TtsError::ModelMissing(p.display().to_string()));
        }
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|_| TtsError::ModelMissing(dir.display().to_string()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
            .collect();
        found.sort();
        match found.len() {
            0 => Err(TtsError::ModelMissing(format!(
                "no .onnx in {}",
                dir.display()
            ))),
            1 => Ok(found.remove(0)),
            _ => {
                // Prefer `model.onnx` (Kokoro's name), else demand an explicit
                // choice rather than guessing which voice was intended.
                found
                    .iter()
                    .find(|p| p.file_name().is_some_and(|n| n == "model.onnx"))
                    .cloned()
                    .ok_or_else(|| {
                        TtsError::ModelMissing(format!(
                            "{} holds {} .onnx files; set model_file",
                            dir.display(),
                            found.len()
                        ))
                    })
            }
        }
    }

    /// A required asset inside a model directory.
    pub fn asset(&self, m: &ModelDesc, name: &str) -> Result<PathBuf> {
        let p = self.model_dir(m).join(name);
        if p.exists() {
            Ok(p)
        } else {
            Err(TtsError::ModelMissing(p.display().to_string()))
        }
    }

    /// Optional asset — returns an empty string when absent, which is what the
    /// sherpa config structs expect for "not set".
    pub fn optional_asset(&self, m: &ModelDesc, name: &str) -> String {
        let p = self.model_dir(m).join(name);
        if p.exists() {
            p.display().to_string()
        } else {
            String::new()
        }
    }

    /// Kokoro's comma-joined lexicon list.
    ///
    /// **Absolute paths only** — Reverie found relative entries resolve against
    /// the process CWD, not the model directory, which silently degrades
    /// pronunciation instead of failing.
    pub fn kokoro_lexicons(&self, m: &ModelDesc) -> String {
        let dir = self.model_dir(m);
        let mut out: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("lexicon") && n.ends_with(".txt"))
                })
                .collect();
            paths.sort();
            out = paths.iter().map(|p| p.display().to_string()).collect();
        }
        out.join(",")
    }
}

/// Model table. Directory names are the extracted sherpa-onnx `tts-models`
/// tarballs, and the rates and speaker counts are Reverie's runtime-confirmed
/// figures, not the model cards'.
fn default_models() -> Vec<ModelDesc> {
    vec![
        ModelDesc {
            id: "piper-en_GB-cori".into(),
            family: ModelFamily::Piper,
            dir: "vits-piper-en_GB-cori-medium".into(),
            native_rate: 22_050,
            speakers: 1,
            model_file: None,
        },
        ModelDesc {
            id: "piper-en_GB-cori-high".into(),
            family: ModelFamily::Piper,
            dir: "vits-piper-en_GB-cori-high".into(),
            native_rate: 22_050,
            speakers: 1,
            model_file: None,
        },
        ModelDesc {
            id: "kokoro-multi-lang-v1_0".into(),
            family: ModelFamily::Kokoro,
            dir: "kokoro-multi-lang-v1_0".into(),
            native_rate: 24_000,
            speakers: 53,
            model_file: Some("model.onnx".into()),
        },
        ModelDesc {
            id: "piper-en_US-libritts_r".into(),
            family: ModelFamily::Piper,
            dir: "vits-piper-en_US-libritts_r-medium".into(),
            native_rate: 22_050,
            speakers: 904,
            model_file: None,
        },
    ]
}

/// The default cast: cori for narration, the 28 labelled English Kokoro voices
/// for characters. `libritts_r`'s 904 speakers are unlabelled and need a
/// one-time audition pass, so none are advertised until curated.
fn default_voices() -> Vec<SherpaVoice> {
    // (sid, name, lang, gender) — Reverie's recovered Kokoro map, English range.
    const KOKORO_EN: &[(u32, &str, &str, Gender)] = &[
        (0, "af_alloy", "en-US", Gender::Female),
        (1, "af_aoede", "en-US", Gender::Female),
        (2, "af_bella", "en-US", Gender::Female),
        (3, "af_heart", "en-US", Gender::Female),
        (4, "af_jessica", "en-US", Gender::Female),
        (5, "af_kore", "en-US", Gender::Female),
        (6, "af_nicole", "en-US", Gender::Female),
        (7, "af_nova", "en-US", Gender::Female),
        (8, "af_river", "en-US", Gender::Female),
        (9, "af_sarah", "en-US", Gender::Female),
        (10, "af_sky", "en-US", Gender::Female),
        (11, "am_adam", "en-US", Gender::Male),
        (12, "am_echo", "en-US", Gender::Male),
        (13, "am_eric", "en-US", Gender::Male),
        (14, "am_fenrir", "en-US", Gender::Male),
        (15, "am_liam", "en-US", Gender::Male),
        (16, "am_michael", "en-US", Gender::Male),
        (17, "am_onyx", "en-US", Gender::Male),
        (18, "am_puck", "en-US", Gender::Male),
        (19, "am_santa", "en-US", Gender::Male),
        (20, "bf_alice", "en-GB", Gender::Female),
        (21, "bf_emma", "en-GB", Gender::Female),
        (22, "bf_isabella", "en-GB", Gender::Female),
        (23, "bf_lily", "en-GB", Gender::Female),
        (24, "bm_daniel", "en-GB", Gender::Male),
        (25, "bm_fable", "en-GB", Gender::Male),
        (26, "bm_george", "en-GB", Gender::Male),
        (27, "bm_lewis", "en-GB", Gender::Male),
    ];

    let mut out = vec![
        SherpaVoice {
            voice: "piper-en_GB-cori:0".into(),
            // Reverie flagged this: cori is female, trained on 24 h of LibriVox
            // audiobook narration.
            label: "cori (narrator, en_GB female)".into(),
            lang: "en-GB".into(),
            gender: Gender::Female,
        },
        SherpaVoice {
            voice: "piper-en_GB-cori-high:0".into(),
            label: "cori-high (narrator, best quality)".into(),
            lang: "en-GB".into(),
            gender: Gender::Female,
        },
    ];
    out.extend(KOKORO_EN.iter().map(|(sid, name, lang, gender)| SherpaVoice {
        voice: format!("kokoro-multi-lang-v1_0:{sid}"),
        label: (*name).to_string(),
        lang: (*lang).to_string(),
        gender: *gender,
    }));
    out
}

/// Round-robin shard positions across `workers` buckets.
///
/// Shards by **segment**, never by model — every worker holds every model it is
/// asked for, and Reverie measured no reload penalty for alternating engines
/// mid-chapter. Round-robin rather than contiguous chunks so a chapter whose long
/// narration blocks cluster together still spreads across the pool.
pub fn shard(len: usize, workers: usize) -> Vec<Vec<usize>> {
    let workers = workers.max(1).min(len.max(1));
    let mut buckets = vec![Vec::new(); workers];
    for i in 0..len {
        buckets[i % workers].push(i);
    }
    buckets
}

/// Path helper for the engine's `String`-typed sherpa config fields.
#[cfg(feature = "sherpa")]
pub(crate) fn as_str(p: &Path) -> String {
    p.display().to_string()
}
