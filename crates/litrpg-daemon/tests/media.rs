//! `/media/{n}.pcm` and `.mp3` — status codes, headers, byte-exact payloads.

mod common;

use axum::http::StatusCode;
use common::{CH1_MP3_LEN, CH1_PCM_LEN, assert_status, body_bytes, fixture, header};

/// The fixture writes `byte[i] = i % 256`, so this is the expected content at `start`.
fn expected(start: u64, len: u64) -> Vec<u8> {
    (start..start + len).map(|i| (i % 256) as u8).collect()
}

#[tokio::test]
async fn full_get_advertises_accept_ranges() {
    let f = fixture();
    let resp = f.get("/media/0001.pcm").await;

    assert_status(&resp, StatusCode::OK);
    // Advertised on the plain 200 too: a client that has not yet asked for a range
    // still needs to learn that it may.
    assert_eq!(header(&resp, "accept-ranges").as_deref(), Some("bytes"));
    assert_eq!(
        header(&resp, "content-length").as_deref(),
        Some(CH1_PCM_LEN.to_string().as_str())
    );
    assert_eq!(
        header(&resp, "content-type").as_deref(),
        Some("audio/L16; rate=16000; channels=1")
    );
    assert!(header(&resp, "content-range").is_none());

    let body = body_bytes(resp).await;
    assert_eq!(body.len() as u64, CH1_PCM_LEN);
    assert_eq!(body, expected(0, CH1_PCM_LEN));
}

#[tokio::test]
async fn exact_range_returns_206_with_correct_bytes() {
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=1000-1099").await;

    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes 1000-1099/96000")
    );
    assert_eq!(header(&resp, "content-length").as_deref(), Some("100"));
    assert_eq!(header(&resp, "accept-ranges").as_deref(), Some("bytes"));

    let body = body_bytes(resp).await;
    assert_eq!(body.len(), 100);
    // Proves the seek landed on the right offset, not merely the right length.
    assert_eq!(body, expected(1000, 100));
}

#[tokio::test]
async fn open_ended_range_streams_to_eof() {
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=95000-").await;

    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes 95000-95999/96000")
    );
    assert_eq!(header(&resp, "content-length").as_deref(), Some("1000"));

    let body = body_bytes(resp).await;
    assert_eq!(body, expected(95_000, 1000));
}

#[tokio::test]
async fn suffix_range_returns_tail() {
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=-1000").await;

    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes 95000-95999/96000")
    );
    assert_eq!(body_bytes(resp).await, expected(95_000, 1000));
}

#[tokio::test]
async fn single_byte_range() {
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=0-0").await;

    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert_eq!(header(&resp, "content-length").as_deref(), Some("1"));
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes 0-0/96000")
    );
    assert_eq!(body_bytes(resp).await, vec![0u8]);
}

#[tokio::test]
async fn range_past_eof_is_416_with_size_hint() {
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=96000-").await;

    assert_status(&resp, StatusCode::RANGE_NOT_SATISFIABLE);
    // The size hint is what lets a client correct itself instead of guessing.
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes */96000")
    );
    assert!(body_bytes(resp).await.is_empty());
}

#[tokio::test]
async fn end_past_eof_clamps_to_last_byte() {
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=95990-999999").await;

    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes 95990-95999/96000")
    );
    assert_eq!(body_bytes(resp).await, expected(95_990, 10));
}

#[tokio::test]
async fn malformed_range_serves_full_body() {
    let f = fixture();
    for bad in ["bytes=500-100", "bytes=abc", "items=0-99", "bytes=0-1,5-6"] {
        let resp = f.get_range("/media/0001.pcm", bad).await;
        assert_status(&resp, StatusCode::OK);
        assert_eq!(
            body_bytes(resp).await.len() as u64,
            CH1_PCM_LEN,
            "expected {bad:?} to be ignored and serve the full body"
        );
    }
}

/// Walking a whole chapter in windows must reassemble to exactly the original bytes —
/// this is literally the watch's playback loop.
#[tokio::test]
async fn sequential_windows_reassemble_the_whole_file() {
    let f = fixture();
    let window = 512u64; // the watch's actual window size (spec §9.4)
    let mut assembled = Vec::new();
    let mut start = 0u64;

    while start < CH1_PCM_LEN {
        let end = (start + window - 1).min(CH1_PCM_LEN - 1);
        let resp = f
            .get_range("/media/0001.pcm", &format!("bytes={start}-{end}"))
            .await;
        assert_status(&resp, StatusCode::PARTIAL_CONTENT);
        assembled.extend_from_slice(&body_bytes(resp).await);
        start = end + 1;
    }

    assert_eq!(assembled.len() as u64, CH1_PCM_LEN);
    assert_eq!(assembled, expected(0, CH1_PCM_LEN));
}

#[tokio::test]
async fn mp3_served_with_audio_mpeg_and_ranges() {
    let f = fixture();
    let resp = f.get("/media/0001.mp3").await;

    assert_status(&resp, StatusCode::OK);
    assert_eq!(header(&resp, "content-type").as_deref(), Some("audio/mpeg"));
    assert_eq!(header(&resp, "accept-ranges").as_deref(), Some("bytes"));
    assert_eq!(body_bytes(resp).await.len() as u64, CH1_MP3_LEN);

    // Podcast clients seek, so ranges matter on mp3 as well.
    let resp = f.get_range("/media/0001.mp3", "bytes=0-15").await;
    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header(&resp, "content-range").as_deref(),
        Some("bytes 0-15/512")
    );
}

#[tokio::test]
async fn missing_media_is_404() {
    let f = fixture();
    // Chapter 2 exists in the store but has no audio rendered.
    assert_status(&f.get("/media/0002.pcm").await, StatusCode::NOT_FOUND);
    assert_status(&f.get("/media/0999.pcm").await, StatusCode::NOT_FOUND);
}

/// The filename is built from a parsed `u32`, never from the raw path segment, so
/// every one of these fails at `parse::<u32>()` before touching the filesystem.
#[tokio::test]
async fn path_traversal_is_rejected() {
    let f = fixture();
    for bad in [
        "/media/..%2f..%2fetc%2fpasswd",
        "/media/....pcm",
        "/media/-1.pcm",
        "/media/+1.pcm",
        "/media/1e3.pcm",
        "/media/%2e%2e%2f0001.pcm",
        "/media/0001.pcm%00.txt",
        "/media/0001.exe",
        "/media/0001",
        "/media/etc-passwd.pcm",
    ] {
        let resp = f.get(bad).await;
        assert!(
            resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND,
            "traversal/garbage {bad:?} returned {} — must never be served",
            resp.status()
        );
    }
}

/// Exactly one URL must address each chapter.
///
/// Regression test for a real bug this suite caught: `"+1".parse::<u32>()` succeeds in
/// Rust, so `/media/+1.pcm` originally served chapter 1 — a second name for the same
/// resource. Non-canonical spellings are now rejected outright.
#[tokio::test]
async fn only_the_canonical_zero_padded_name_is_accepted() {
    let f = fixture();
    assert_status(&f.get("/media/0001.pcm").await, StatusCode::OK);

    for alias in [
        "/media/1.pcm",     // unpadded
        "/media/+1.pcm",    // `+` accepted by u32::from_str
        "/media/00001.pcm", // over-padded
        "/media/001.pcm",   // under-padded
    ] {
        assert_status(&f.get(alias).await, StatusCode::BAD_REQUEST);
    }
}

/// `..%2f..%2fetc%2fpasswd` style attempts must not read a real file even if the
/// router decodes them into a single segment.
#[tokio::test]
async fn traversal_cannot_read_outside_media_root() {
    let f = fixture();
    let resp = f.get("/media/..%2F..%2F..%2Fetc%2Fpasswd").await;
    assert_ne!(resp.status(), StatusCode::OK);
    let body = body_bytes(resp).await;
    let text = String::from_utf8_lossy(&body);
    assert!(
        !text.contains("root:"),
        "response leaked /etc/passwd content"
    );
}
