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
pub mod range;
pub mod state;
pub mod version;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use litrpg_store::Store;
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
}

impl AppState {
    pub fn new(store: Store, config: Config) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            config,
        }
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
        .route("/api/character/{subject}", get(state::get_character))
        .route("/api/notes", post(notes::post_note))
        // One handler for both extensions; it parses `NNNN.pcm` / `NNNN.mp3` itself,
        // which is also where path traversal is rejected (see `media::parse_media_name`).
        .route("/media/{name}", get(media::serve_media))
        .route("/feed.xml", get(feed::get_feed))
        // TODO(litrpg-tts): `GET /api/voices` — the aggregated voice inventory that
        // drives cast selection (spec §9.1). Needs the `litrpg-tts` crate's backend
        // registry to enumerate sherpa + Azure voices; that crate is still being built,
        // so the route is intentionally absent rather than stubbed with a wrong shape.
        .with_state(state)
}
