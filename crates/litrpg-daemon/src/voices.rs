//! `GET /api/voices` — the aggregated voice catalog behind cast selection (spec §9.1).
//!
//! # Three states, not two
//!
//! A cast-selection UI has to explain itself, so this route distinguishes:
//!
//! | State | `compiled_in` | `availability.state` | Meaning |
//! |---|---|---|---|
//! | usable | `true` | `ready` | assign freely |
//! | built but unusable | `true` | `missing` + `reason` | needs credentials or model files |
//! | not in this build | `false` | `missing` + `reason` | rebuild with the feature |
//!
//! `litrpg_tts::Availability` only encodes the first two — it is explicitly a *runtime*
//! fact. "Not compiled in" is a build fact, so it lives in a separate `compiled_in`
//! flag rather than being smuggled into a `reason` string that clients would have to
//! pattern-match.
//!
//! # Why this composes two sources
//!
//! `TtsRegistry::all_voices()` returns voices from **registered and available**
//! backends only — by design, so an unusable voice can never be assigned. But with the
//! default feature set there is no `SherpaBackend` to register at all (only the
//! `sherpa` feature compiles the worker pool), so the registry alone would report
//! sherpa as simply absent.
//!
//! `SherpaConfig` *is* always compiled — voices are config, not code — so its catalog
//! is available regardless. This module therefore composes registry voices with the
//! config-derived sherpa catalog, and marks each voice `assignable` so the UI can grey
//! out what it must not assign yet while still showing the cast the deployment intends.
//! Listing an unassignable voice is a deliberate widening of the registry's stricter
//! contract, safe because `assignable` is explicit per voice.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use litrpg_tts::backend::Availability;
use litrpg_tts::sherpa::SherpaConfig;
use litrpg_tts::{TtsRegistry, VoiceDesc};
use serde::Serialize;

use crate::AppState;
use crate::error::ApiResult;

/// Whether the `sherpa` cargo feature is compiled into this binary.
///
/// Always `false` for the daemon's normal build, and correct automatically if someone
/// later builds with `--features litrpg-tts/sherpa`.
pub const SHERPA_COMPILED_IN: bool = cfg!(feature = "sherpa");

#[derive(Debug, Clone, Serialize)]
pub struct BackendStatus {
    pub id: String,
    /// Present in this build. `false` means a rebuild is required, not a config fix.
    pub compiled_in: bool,
    /// Flattened: `{"state":"ready"}` or `{"state":"missing","reason":"…"}`.
    pub availability: Availability,
    /// Voices advertised, whether or not they are currently assignable.
    pub voice_count: usize,
    /// `availability.is_ready() && compiled_in`.
    pub assignable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceEntry {
    pub voice_ref: String,
    pub label: String,
    pub lang: String,
    pub gender: litrpg_tts::Gender,
    pub cost_class: litrpg_tts::CostClass,
    /// Owning backend id, split from `voice_ref` on the **first** colon — Azure voice
    /// names legitimately contain colons.
    pub backend: String,
    /// Whether assigning this voice right now would actually render.
    pub assignable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoicesResponse {
    pub backends: Vec<BackendStatus>,
    pub voices: Vec<VoiceEntry>,
    pub total: usize,
    /// Convenience so a UI need not filter to answer "can I cast anything at all?".
    pub assignable_count: usize,
}

fn backend_of(voice_ref: &str) -> String {
    voice_ref
        .split_once(':')
        .map(|(b, _)| b.to_string())
        .unwrap_or_default()
}

fn entries(voices: Vec<VoiceDesc>, assignable: bool) -> Vec<VoiceEntry> {
    voices
        .into_iter()
        .map(|v| VoiceEntry {
            backend: backend_of(&v.voice_ref),
            voice_ref: v.voice_ref,
            label: v.label,
            lang: v.lang,
            gender: v.gender,
            cost_class: v.cost_class,
            assignable,
        })
        .collect()
}

/// Build the catalog from a registry plus the always-compiled sherpa config.
///
/// Pure over its inputs so the composition rules are unit-testable without a router.
pub fn build_catalog(registry: &TtsRegistry, sherpa: &SherpaConfig) -> VoicesResponse {
    let mut backends = Vec::new();
    let mut voices = Vec::new();

    // ── Registered backends (Azure under the default feature set) ───────────
    for (id, availability) in registry.availability() {
        let ready = availability.is_ready();
        let backend_voices = registry.get(&id).map(|b| b.voices()).unwrap_or_default();

        backends.push(BackendStatus {
            id,
            compiled_in: true,
            availability,
            voice_count: backend_voices.len(),
            assignable: ready,
        });
        voices.extend(entries(backend_voices, ready));
    }

    // ── sherpa: catalog always, backend only with the feature ───────────────
    if !backends.iter().any(|b| b.id == "sherpa") {
        let sherpa_voices = sherpa.voice_descs();
        // Model files can be present while the pool is not compiled in, so report the
        // runtime check *and* the build fact rather than collapsing them.
        let availability = if SHERPA_COMPILED_IN {
            sherpa.availability()
        } else {
            match sherpa.availability() {
                // Assets are fine; the only thing missing is the build feature.
                Availability::Ready => Availability::missing(
                    "sherpa models present, but this binary was built without the \
                     `sherpa` feature; rebuild with --features litrpg-tts/sherpa",
                ),
                // Assets missing too — the asset reason is the more actionable one.
                missing => missing,
            }
        };
        let assignable = availability.is_ready() && SHERPA_COMPILED_IN;

        backends.push(BackendStatus {
            id: "sherpa".to_string(),
            compiled_in: SHERPA_COMPILED_IN,
            availability,
            voice_count: sherpa_voices.len(),
            assignable,
        });
        voices.extend(entries(sherpa_voices, assignable));
    }

    let total = voices.len();
    let assignable_count = voices.iter().filter(|v| v.assignable).count();

    VoicesResponse {
        backends,
        voices,
        total,
        assignable_count,
    }
}

/// `GET /api/voices`
pub async fn get_voices(State(state): State<Arc<AppState>>) -> ApiResult<Json<VoicesResponse>> {
    Ok(Json(build_catalog(&state.tts, &state.sherpa)))
}
