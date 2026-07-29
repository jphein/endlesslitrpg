//! `GET /media/{n}.pcm` and `/media/{n}.mp3` — Range-capable, never buffered.
//!
//! This is the load-bearing route. A ~13 minute chapter is ~25 MB of PCM and the
//! watch has 512 KB of SRAM with no PSRAM, so it *cannot* hold a chapter — it plays
//! by pulling byte ranges lifted straight from the manifest (512 B window, ~2.4 KB
//! peak; spec §9.4). The daemon must therefore seek and stream: reading 25 MB into a
//! `Vec` would pass every functional test here and still be a bug, because four
//! concurrent listeners would be 100 MB of resident memory on a box already hosting
//! a 20 GB llama.cpp process.

use std::io::SeekFrom;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::range::{RangeOutcome, content_range, content_range_unsatisfied, parse_range};

/// `audio/L16` is the registered type for linear 16-bit PCM (RFC 2586). Stating rate
/// and channels inline means a client needs no side-channel to interpret the bytes —
/// which matters because the watch has no decoder and no container parser at all.
pub const PCM_CONTENT_TYPE: &str = "audio/L16; rate=16000; channels=1";
pub const MP3_CONTENT_TYPE: &str = "audio/mpeg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Pcm,
    Mp3,
}

impl MediaKind {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Pcm => "pcm",
            Self::Mp3 => "mp3",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Pcm => PCM_CONTENT_TYPE,
            Self::Mp3 => MP3_CONTENT_TYPE,
        }
    }
}

/// Parse `NNNN.pcm` / `NNNN.mp3` into a chapter number and kind.
///
/// **This is the path-traversal boundary.** The request's path segment is never used
/// to build a filename; only the parsed `u32` is, via `format!("{n:04}.{ext}")`. So
/// `../../etc/passwd`, `0000.pcm/../../x`, a percent-encoded separator, or a NUL byte
/// all fail here at `parse::<u32>()` rather than being sanitised — rejecting
/// non-numeric input outright is a much smaller thing to get right than filtering
/// hostile paths.
pub fn parse_media_name(name: &str) -> ApiResult<(u32, MediaKind)> {
    let (stem, ext) = name
        .rsplit_once('.')
        .ok_or_else(|| ApiError::BadRequest(format!("media name has no extension: {name}")))?;

    let kind = match ext {
        "pcm" => MediaKind::Pcm,
        "mp3" => MediaKind::Mp3,
        other => {
            return Err(ApiError::BadRequest(format!(
                "unsupported media extension: {other}"
            )));
        }
    };

    // `u32::from_str` accepts a leading `+` ("+1" parses as 1), so parsing alone would
    // make `/media/+1.pcm` a second URL for chapter 1. Harmless for traversal — the
    // filename is still rebuilt from the integer — but it breaks the URL→file mapping
    // being one-to-one, which caching and ETags would later depend on. Require plain
    // ASCII digits first.
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::BadRequest(format!(
            "media name is not a chapter number: {stem}"
        )));
    }

    let n: u32 = stem
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("media name is not a chapter number: {stem}")))?;

    // Insist on the canonical zero-padded spelling, so exactly one URL addresses each
    // chapter: `0001.pcm` yes, `1.pcm` and `00001.pcm` no. (`{n:04}` widens naturally
    // past chapter 9999, so this stays correct as the serial grows.)
    if format!("{n:04}") != stem {
        return Err(ApiError::BadRequest(format!(
            "media name must be zero-padded to 4 digits: expected {n:04}, got {stem}"
        )));
    }

    Ok((n, kind))
}

/// Single handler for both extensions.
///
/// Routed as `/media/{name}` rather than `/media/{n}.pcm` deliberately: matching a
/// literal suffix inside a dynamic segment depends on router internals, and doing the
/// split here keeps the traversal guard and the extension whitelist in one auditable
/// place.
pub async fn serve_media(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (n, kind) = parse_media_name(&name)?;
    let path = state
        .config
        .media_root
        .join(format!("{n:04}.{}", kind.ext()));

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| ApiError::AudioNotFound(n))?;
    if !meta.is_file() {
        return Err(ApiError::AudioNotFound(n));
    }
    let total = meta.len();

    let outcome = parse_range(headers.get(RANGE).and_then(|v| v.to_str().ok()), total);

    match outcome {
        // `Accept-Ranges` is advertised here too, not only on 206: a client that has
        // not yet asked for a range still needs to learn that it may.
        RangeOutcome::Full => Ok(stream(&path, 0, total, total, kind, None).await?),

        RangeOutcome::Partial { start, end } => {
            let len = end - start + 1;
            Ok(stream(
                &path,
                start,
                len,
                total,
                kind,
                Some(content_range(start, end, total)),
            )
            .await?)
        }

        // 416 must carry `Content-Range: bytes */total` so the client can learn the
        // real size and retry correctly instead of guessing.
        RangeOutcome::Unsatisfiable => {
            let mut out = HeaderMap::new();
            out.insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            out.insert(
                CONTENT_RANGE,
                content_range_unsatisfied(total).parse().unwrap(),
            );
            Ok((StatusCode::RANGE_NOT_SATISFIABLE, out).into_response())
        }
    }
}

/// Seek to `start` and stream exactly `len` bytes.
///
/// `File::take(len)` bounds the read so a concurrent append cannot lengthen the body
/// past the `Content-Length` we already promised — a mismatch there is a protocol
/// error, not merely surplus data.
async fn stream(
    path: &std::path::Path,
    start: u64,
    len: u64,
    _total: u64,
    kind: MediaKind,
    content_range_value: Option<String>,
) -> ApiResult<Response> {
    let mut file = File::open(path).await?;
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }

    // The whole point: a bounded reader lifted into a body. Constant memory
    // regardless of file size — nothing here ever allocates `len` bytes.
    let body = Body::from_stream(ReaderStream::new(file.take(len)));

    let mut out = HeaderMap::new();
    out.insert(CONTENT_TYPE, kind.content_type().parse().unwrap());
    out.insert(ACCEPT_RANGES, "bytes".parse().unwrap());
    out.insert(CONTENT_LENGTH, len.to_string().parse().unwrap());

    let status = if let Some(cr) = content_range_value {
        out.insert(CONTENT_RANGE, cr.parse().unwrap());
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    Ok((status, out, body).into_response())
}
