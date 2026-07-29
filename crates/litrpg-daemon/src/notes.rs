//! `POST /api/notes` — director notes from the CLI, the watch's push-to-talk, or
//! Candela.
//!
//! Previously a documented `501`: the `notes` table existed in the schema but
//! `litrpg-store` exposed no way to write it. `Store::insert_note` now exists, so the
//! route persists for real and returns `201 Created` with the row id.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
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

/// Returns `201 Created`. The body is stored **trimmed**, and the response echoes what
/// was actually persisted rather than what was sent, so a client never has to guess
/// whether its whitespace survived.
pub async fn post_note(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NoteRequest>,
) -> ApiResult<impl IntoResponse> {
    validate_note(&req)?;

    let body = req.body.trim().to_string();
    let id = state.store.lock().await.insert_note(&body, &req.source)?;

    Ok((
        StatusCode::CREATED,
        Json(NoteAccepted {
            id,
            body,
            source: req.source,
        }),
    ))
}
