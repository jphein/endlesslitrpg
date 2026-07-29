//! `POST /api/notes` — director notes from the CLI, the watch's push-to-talk, or
//! Candela.
//!
//! # BLOCKED on a `litrpg-store` accessor
//!
//! The schema has the table (`notes(id, body, source, created_at, consumed_chapter)`
//! in `001_initial.sql`) but `litrpg-store` exposes **no** method to write it, and
//! `Store::conn` is `pub(crate)` — so from this crate the table is unreachable.
//!
//! Needed, and deliberately *not* added here because `litrpg-store` is owned by
//! another agent:
//!
//! ```ignore
//! impl Store {
//!     pub fn insert_note(&self, body: &str, source: &str) -> Result<i64>;
//!     pub fn pending_notes(&self) -> Result<Vec<NoteRow>>;   // consumed_chapter IS NULL
//! }
//! ```
//!
//! Until then this route validates the payload fully and returns **501 Not
//! Implemented**. Two rejected alternatives, both worse:
//!
//! * Returning `202 Accepted` and dropping the note. A director note that vanishes
//!   silently is the single worst outcome here — the user believes the story was
//!   steered and it was not.
//! * Opening a second `rusqlite::Connection` to the same file from this crate. That
//!   would bypass the single-writer mutex in `AppState` and reintroduce exactly the
//!   `next_seq()` race the mutex exists to make structural.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Whitelisted note origins (per the route contract).
pub const NOTE_SOURCES: &[&str] = &["cli", "watch", "candela"];

/// Longest accepted note body. A director note is an instruction, not prose, and the
/// watch's push-to-talk transcript is short — an unbounded field reachable from an
/// unauthenticated LAN endpoint is a needless liability.
pub const MAX_NOTE_BYTES: usize = 4096;

#[derive(Debug, Deserialize)]
pub struct NoteRequest {
    pub body: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct NoteAccepted {
    pub id: i64,
    pub body: String,
    pub source: String,
}

/// Validate a note independently of persistence, so the contract is testable and
/// stays correct once the store method lands.
pub fn validate_note(req: &NoteRequest) -> ApiResult<()> {
    let body = req.body.trim();
    if body.is_empty() {
        return Err(ApiError::BadRequest("note body is empty".into()));
    }
    if req.body.len() > MAX_NOTE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "note body exceeds {MAX_NOTE_BYTES} bytes"
        )));
    }
    if !NOTE_SOURCES.contains(&req.source.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "source must be one of {NOTE_SOURCES:?}, got {:?}",
            req.source
        )));
    }
    Ok(())
}

pub async fn post_note(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<NoteRequest>,
) -> ApiResult<Json<NoteAccepted>> {
    validate_note(&req)?;

    // TODO(litrpg-store): replace with
    //   let id = state.store.lock().await.insert_note(req.body.trim(), &req.source)?;
    //   Ok(Json(NoteAccepted { id, body: req.body, source: req.source }))
    Err(ApiError::NotImplemented(
        "director notes need Store::insert_note; the notes table has no accessor in litrpg-store"
            .into(),
    ))
}
