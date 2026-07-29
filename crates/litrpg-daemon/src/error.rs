//! Error type and its HTTP projection.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("chapter {0} not found")]
    ChapterNotFound(u32),

    #[error("no audio for chapter {0}")]
    AudioNotFound(u32),

    #[error("bad request: {0}")]
    BadRequest(String),

    /// The request is well-formed but the server's state cannot satisfy it — e.g.
    /// recording playback progress before `litrpg init` has written a story row.
    #[error("conflict: {0}")]
    Conflict(String),

    /// A route whose backing store method does not exist yet. Distinct from a 500
    /// so it can never be mistaken for a transient failure — see `notes.rs`.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("store: {0}")]
    Store(#[from] litrpg_store::StoreError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::ChapterNotFound(_) | Self::AudioNotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            Self::Conflict(_) => StatusCode::CONFLICT,
            // A missing chapter surfaces from the store as `ChapterNotFound`; map it
            // rather than letting it read as a server fault.
            Self::Store(litrpg_store::StoreError::ChapterNotFound(_)) => StatusCode::NOT_FOUND,
            // `NoStoryRow` means the deployment was never initialised. The request is
            // well-formed, so this is a *state* precondition (409), not a 400 and
            // certainly not the 500 it would otherwise fall through to.
            Self::Store(litrpg_store::StoreError::NoStoryRow) => StatusCode::CONFLICT,
            Self::Store(_) | Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
