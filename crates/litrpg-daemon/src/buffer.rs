//! `POST /api/buffer` — change the buffer target, and say why.
//!
//! This is the honest lever for "write another chapter". The dishonest one is advancing
//! `consumed_through`, which would also start the engine — by putting a false number in
//! the one field the throttle measures from. "I want another chapter" and "I finished
//! chapter 3" are different claims and only one of them is true, so this route touches
//! the cursor never, under any input.
//!
//! # Why a relative bump exists
//!
//! The primary caller is a spoken incantation. A voice command carries no arithmetic: the
//! executor sends a fixed JSON body with at most one dictated field, so it cannot compute
//! `current + 1` and cannot be trusted to have read a current value that is still true by
//! the time the write lands. `{"bump": 1}` is resolved **server-side against the effective
//! target under the same lock as the write**, which makes it correct under concurrency in a
//! way a client-computed absolute never is. `target` remains for the CLI and other tools,
//! where an operator genuinely means one specific number.
//!
//! # Why the response reports the effective value
//!
//! Because the bug this whole table exists to kill was two processes disagreeing about the
//! target. Echoing back the request would recreate it in miniature: a caller could be told
//! `10` while the engine used something else. The response is read back from the store
//! after the write.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::notes::NOTE_SOURCES;

/// Largest single relative bump.
///
/// Three, because the trail this replaces moved one at a time (6 → 7 → 8 → 9), each with
/// its own written reason, and the discipline behind that is worth keeping: a target is
/// raised deliberately, not swung. It also bounds a misheard number — "bump by three" is a
/// recoverable mistake, "bump by thirty" is an afternoon of CPU nobody asked for.
pub const MAX_BUMP: u32 = 3;

/// Hard ceiling on the target, whatever route sets it.
///
/// A chapter is ~2 000 words, ~10 minutes of narration and ~25 MB of PCM. Twenty-four is
/// therefore about four hours of audio and ~600 MB of media held ahead of the listener —
/// beyond any plausible spoken request, and a bound on how much unattended generation a
/// single command can commit the machine to. Not a storage limit; a limit on how far one
/// sentence can run away with the box.
pub const MAX_BUFFER_TARGET: u32 = 24;

/// Longest accepted justification. It is a sentence, and on the voice path it is an STT
/// transcript, which is short. Bounded for the same reason a note body is.
pub const MAX_JUSTIFICATION_BYTES: usize = 512;

/// Transport bound for the whole body, applied as a `DefaultBodyLimit` layer so an
/// oversized payload is refused **before** being buffered.
pub const MAX_BUFFER_BODY_BYTES: usize = MAX_JUSTIFICATION_BYTES + 512;

#[derive(Debug, Deserialize)]
pub struct BufferRequest {
    /// Raise the target by this much, resolved server-side. Mutually exclusive with
    /// `target`.
    pub bump: Option<u32>,
    /// Set the target to exactly this. Mutually exclusive with `bump`.
    pub target: Option<u32>,
    pub source: String,
    /// Why. Optional on the wire because a spoken command may carry no dictated reason;
    /// absent or blank is recorded honestly rather than invented — see
    /// [`justification_or_default`].
    pub justification: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BufferChanged {
    pub id: i64,
    /// The target now in force, read back from the store — what the engine will actually
    /// use on its next cycle. Never an echo of the request.
    pub buffer_target: u32,
    /// What it was immediately before this change, so a caller can narrate the delta
    /// without a second request.
    pub previous_buffer_target: u32,
    pub source: String,
    pub justification: String,
}

/// What to record when no reason was dictated.
///
/// Not a cheerful placeholder: the trail exists to answer "why is the target 10?", and
/// "nobody said" is a truthful answer where "voice command" alone would imply a reason was
/// given. The source is already its own column, so this adds the part the column cannot.
pub fn justification_or_default(justification: Option<&str>, source: &str) -> String {
    match justification.map(str::trim) {
        Some(j) if !j.is_empty() => j.to_string(),
        _ => format!("{source} command; no reason given"),
    }
}

/// Resolve the requested change to an absolute target, or explain why it cannot be.
///
/// Split out from the handler so the whole decision table is testable without a store,
/// a socket or a fixture.
pub fn resolve_target(req: &BufferRequest, current: u32) -> ApiResult<u32> {
    let wanted = match (req.bump, req.target) {
        (Some(_), Some(_)) => {
            return Err(ApiError::Unprocessable(
                "send either bump or target, not both".into(),
            ));
        }
        (None, None) => {
            return Err(ApiError::BadRequest(
                "one of bump or target is required".into(),
            ));
        }
        (Some(0), None) => {
            return Err(ApiError::Unprocessable(
                "bump must be at least 1; a bump of 0 changes nothing".into(),
            ));
        }
        (Some(b), None) if b > MAX_BUMP => {
            return Err(ApiError::Unprocessable(format!(
                "bump is {b}, but the most one call may add is {MAX_BUMP}"
            )));
        }
        // Saturating, so a bump near the ceiling is refused by the range check below with
        // a message about the ceiling rather than panicking or wrapping to something small.
        (Some(b), None) => current.saturating_add(b),
        (None, Some(t)) => t,
    };

    if wanted < litrpg_config::MIN_BUFFER_TARGET {
        return Err(ApiError::Unprocessable(format!(
            "target would be {wanted}, but the minimum is {}: below it the listener runs \
             dry while the next chapter renders",
            litrpg_config::MIN_BUFFER_TARGET
        )));
    }
    if wanted > MAX_BUFFER_TARGET {
        return Err(ApiError::Unprocessable(format!(
            "target would be {wanted}, but the ceiling is {MAX_BUFFER_TARGET}"
        )));
    }
    Ok(wanted)
}

/// Validate the parts that do not depend on the current target.
pub fn validate_request(req: &BufferRequest) -> ApiResult<()> {
    if !NOTE_SOURCES.contains(&req.source.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "source must be one of {NOTE_SOURCES:?}, got {:?}",
            req.source
        )));
    }
    if let Some(j) = &req.justification
        && j.len() > MAX_JUSTIFICATION_BYTES
    {
        return Err(ApiError::BadRequest(format!(
            "justification exceeds {MAX_JUSTIFICATION_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Returns `201 Created`.
///
/// Read-resolve-write happens under one lock. A bump computed before acquiring it could be
/// resolved against a target that another caller had already changed, which is exactly the
/// stale-snapshot bug in miniature.
pub async fn post_buffer(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BufferRequest>,
) -> ApiResult<impl IntoResponse> {
    validate_request(&req)?;

    let store = state.store.lock().await;
    let previous = store
        .buffer_target()?
        .unwrap_or(state.config.buffer_target);
    let target = resolve_target(&req, previous)?;
    let justification = justification_or_default(req.justification.as_deref(), &req.source);
    let change = store.record_buffer_target(target, &req.source, &justification)?;
    // Read back rather than trusting what was just written: this response is the one the
    // caller will narrate aloud, and the entire point of the table is that nobody has to
    // take a second process's word for the effective value.
    let effective = store
        .buffer_target()?
        .unwrap_or(state.config.buffer_target);
    drop(store);

    Ok((
        StatusCode::CREATED,
        Json(BufferChanged {
            id: change.id,
            buffer_target: effective,
            previous_buffer_target: previous,
            source: change.source,
            justification: change.justification,
        }),
    ))
}
