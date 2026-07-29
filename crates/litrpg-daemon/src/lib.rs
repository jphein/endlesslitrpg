//! HTTP surface for endless-litrpg.
//!
//! Plain HTTP, no TLS, no DNS — the ESP32-C6 watch can do none of those, so it
//! reaches this daemon at a literal `10.0.6.107:8093`.

pub mod chapters;
pub mod config;
pub mod datetime;
pub mod error;
pub mod feed;
pub mod media;
pub mod notes;
pub mod progress;
pub mod range;
pub mod state;
pub mod version;
pub mod voices;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use litrpg_store::Store;
use litrpg_tts::TtsRegistry;
use litrpg_tts::sherpa::SherpaConfig;
use tokio::sync::Mutex;

pub use config::{Config, StoryConfig};
pub use error::{ApiError, ApiResult};

/// Shared handler state.
///
/// # Why the store is behind a single `Mutex`, and why that must not be "optimised"
///
/// `rusqlite::Connection` is `Send` but **not `Sync`**, so `Arc<Store>` will not
/// compile across handlers — a mutex is mechanically required. But it is also load
/// bearing for correctness, which is the part worth protecting:
///
/// `Store::next_seq()` is `SELECT COALESCE(MAX(seq), 0) + 1 FROM ledger` followed by a
/// separate `INSERT` in `append_delta`. That read-then-write is racy: two concurrent
/// appenders can read the same `MAX(seq)` and both insert it. The ledger's whole design
/// rests on `seq` being a total order — `fold()` sorts by it and `rewind` slices on it —
/// so duplicate `seq` values would silently corrupt derived state rather than fail
/// loudly. The store is safe today only because of an **undocumented single-writer
/// assumption**.
///
/// Holding one `Mutex<Store>` for the entire process makes that assumption
/// **structural instead of undocumented**: there is exactly one connection and one
/// lock, so no two appends can interleave.
///
/// **Do not replace this with a connection pool** (r2d2, deadpool, one connection per
/// request) to remove lock contention. A pool restores genuine write concurrency and
/// silently reintroduces the `next_seq()` race. The correct order of operations is:
/// first make `seq` allocation atomic in `litrpg-store` — an `INSERT ... SELECT
/// MAX(seq)+1` in one statement, an `AUTOINCREMENT` column, or an explicit
/// `BEGIN IMMEDIATE` — *then* consider pooling. Contention is not the bottleneck here
/// anyway: these handlers do millisecond-scale reads, and the expensive route
/// (`/media/*.pcm`) never touches the store at all.
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub config: Config,
    /// Registered TTS plugins. Read-only after startup, and `TtsRegistry` is `Sync`,
    /// so unlike the store it needs no lock.
    pub tts: TtsRegistry,
    /// Always-compiled sherpa cast table. Held separately from `tts` because with the
    /// default feature set there is no `SherpaBackend` to register, yet
    /// `/api/voices` must still advertise the catalog — see `voices.rs`.
    pub sherpa: SherpaConfig,
}

impl AppState {
    /// An empty TTS registry and the default sherpa cast table.
    ///
    /// Empty by design: registering Azure would read credentials from the environment,
    /// which would make tests depend on the developer's machine. `main` registers real
    /// backends via [`AppState::with_tts`].
    pub fn new(store: Store, config: Config) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            config,
            tts: TtsRegistry::new(),
            sherpa: SherpaConfig::default(),
        }
    }

    #[must_use]
    pub fn with_tts(mut self, tts: TtsRegistry) -> Self {
        self.tts = tts;
        self
    }

    #[must_use]
    pub fn with_sherpa(mut self, sherpa: SherpaConfig) -> Self {
        self.sherpa = sherpa;
        self
    }
}

async fn healthz() -> &'static str {
    "ok"
}

/// Build the router. Returned rather than served so tests can drive it in-process
/// with `tower::ServiceExt::oneshot` — no socket, no port, no flakiness.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/version", get(version::get_version))
        .route("/api/story", get(chapters::get_story))
        .route("/api/chapters", get(chapters::list_chapters))
        .route("/api/chapters/{n}", get(chapters::get_chapter))
        .route("/api/state", get(state::get_state))
        // Two routes rather than an optional path parameter. `/api/character` with no
        // segment resolves to the protagonist, which keeps both URLs unambiguous and
        // cacheable — an `Option<Path<String>>` would make one URL mean two things and
        // leaves `/api/character/` (trailing slash, empty subject) ill-defined.
        .route("/api/character", get(state::get_protagonist))
        .route("/api/character/{subject}", get(state::get_character))
        .route("/api/voices", get(voices::get_voices))
        // ── Mutating routes ─────────────────────────────────────────────────
        // Both carry an explicit `DefaultBodyLimit`, which rejects **before** buffering.
        // The in-handler length checks stay as the semantic bound; this is the resource
        // bound, and only the layer prevents an unauthenticated LAN caller from making
        // the daemon buffer axum's 2 MB default just to have it rejected afterwards.
        .route(
            "/api/notes",
            post(notes::post_note).layer(DefaultBodyLimit::max(notes::MAX_NOTE_BODY_BYTES)),
        )
        .route(
            "/api/progress",
            get(progress::get_progress)
                .put(progress::put_progress)
                .layer(DefaultBodyLimit::max(progress::MAX_PROGRESS_BODY_BYTES)),
        )
        // One handler for both extensions; it parses `NNNN.pcm` / `NNNN.mp3` itself,
        // which is also where path traversal is rejected (see `media::parse_media_name`).
        .route("/media/{name}", get(media::serve_media))
        .route("/feed.xml", get(feed::get_feed))
        .with_state(state)
}
