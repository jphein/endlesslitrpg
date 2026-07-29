//! `GET /api/progress` and `PUT /api/progress` — the playback cursor.
//!
//! The cursor lives in the database rather than in a CLI flag precisely so that
//! non-CLI clients own it: the watch marks a chapter finished as playback ends, and a
//! Candela source does the same when a reader reaches the bottom. Both are remote and
//! stateless with respect to each other, so this endpoint — not `litrpg listened` — is
//! the primary writer.
//!
//! # Two counts, not one
//!
//! "How many chapters are ahead of me" and "how many can I actually play" are different
//! questions once a render fails (spec §10 ships text without audio). `chapters_ahead`
//! answers the first, `ready_ahead` the second, and a watch showing "3 ready" wants the
//! second. Reporting only one number would make a text-only chapter look playable —
//! exactly the failure the Azure timeout is producing right now.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Body limit for `PUT /api/progress`.
///
/// The payload is one small integer, so anything larger is malformed or hostile. Applied
/// as an `axum::extract::DefaultBodyLimit` layer in the router, which rejects **before**
/// buffering — a limit checked after parsing would still have read the whole body.
pub const MAX_PROGRESS_BODY_BYTES: usize = 256;

#[derive(Debug, Clone, Serialize)]
pub struct ProgressResponse {
    /// Highest chapter the listener has finished. `0` means not started.
    pub consumed_through: u32,
    pub latest_chapter: u32,
    /// Chapters numbered above the cursor, whether or not their audio exists.
    pub chapters_ahead: u32,
    /// Chapters above the cursor that have rendered audio — what "3 ready" means.
    pub ready_ahead: u32,
    /// The engine's target for `ready_ahead` (`litrpg-config`'s `buffer_target`).
    pub buffer_target: u32,
    /// `ready_ahead >= buffer_target`.
    pub buffer_healthy: bool,
    /// Next chapter above the cursor that exists at all, playable or not.
    pub next_chapter: Option<u32>,
    /// Next chapter above the cursor with audio, plus its media URLs — so the watch can
    /// resume from one request instead of three.
    pub next_playable: Option<u32>,
    pub next_pcm_url: Option<String>,
    pub next_mp3_url: Option<String>,
    /// `false` before `litrpg init`; a `PUT` would then be a `409`.
    pub initialised: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgressUpdate {
    pub consumed_through: u32,
}

/// Shared read path, so `GET` and the echo from `PUT` cannot disagree.
async fn snapshot(state: &Arc<AppState>) -> ApiResult<ProgressResponse> {
    let store = state.store.lock().await;
    let consumed_through = store.consumed_through()?;
    let latest_chapter = store.latest_number()?;
    let initialised = store.story()?.is_some();
    // One query, then count in memory: the two "ahead" figures come from the same row
    // set, so fetching once means they cannot describe different instants.
    let ahead = store.chapters_since(consumed_through)?;
    drop(store);

    let chapters_ahead = ahead.len() as u32;
    let ready: Vec<u32> = ahead
        .iter()
        .filter(|c| c.has_audio)
        .map(|c| c.number)
        .collect();
    let ready_ahead = ready.len() as u32;

    let base = &state.config.story.base_url;
    let next_playable = ready.first().copied();

    Ok(ProgressResponse {
        consumed_through,
        latest_chapter,
        chapters_ahead,
        ready_ahead,
        buffer_target: state.config.buffer_target,
        buffer_healthy: ready_ahead >= state.config.buffer_target,
        // `chapters_since` is ordered by number, so the first row is the next chapter.
        next_chapter: ahead.first().map(|c| c.number),
        next_playable,
        next_pcm_url: next_playable.map(|n| format!("{base}/media/{n:04}.pcm")),
        next_mp3_url: next_playable.map(|n| format!("{base}/media/{n:04}.mp3")),
        initialised,
    })
}

/// `GET /api/progress`
pub async fn get_progress(State(state): State<Arc<AppState>>) -> ApiResult<Json<ProgressResponse>> {
    Ok(Json(snapshot(&state).await?))
}

/// `PUT /api/progress` — body `{"consumed_through": N}`.
///
/// Validation rules, each deliberate:
///
/// * **`0` is allowed.** It is the honest encoding of "not started", and rejecting it
///   would leave a client no way to undo a mistaken mark-as-listened.
/// * **Going backwards is allowed.** Re-listening is legitimate; refusing it would make
///   the cursor a ratchet rather than a position (the store's own doc comment says so).
/// * **Beyond the highest existing chapter is rejected**, with the latest number in the
///   message — a cursor past the end would make `chapters_ahead` meaningless and the
///   client cannot guess the bound without being told it.
/// * **No story row is a `409`**, surfaced from `StoreError::NoStoryRow`: the request is
///   fine, the deployment is simply not initialised.
///
/// Returns the same shape as `GET`, so a client refreshes its whole view from the write
/// and never needs a follow-up read.
pub async fn put_progress(
    State(state): State<Arc<AppState>>,
    Json(update): Json<ProgressUpdate>,
) -> ApiResult<Json<ProgressResponse>> {
    let requested = update.consumed_through;

    {
        let store = state.store.lock().await;
        let latest = store.latest_number()?;

        // `0` is always acceptable, including on an empty story where `latest` is 0.
        if requested > latest {
            return Err(ApiError::BadRequest(format!(
                "consumed_through {requested} is beyond the latest chapter {latest}"
            )));
        }

        // Held across the check and the write: releasing the lock between them would
        // let a concurrent request move `latest` and admit a now-invalid cursor.
        store.set_consumed_through(requested)?;
    }

    Ok(Json(snapshot(&state).await?))
}
