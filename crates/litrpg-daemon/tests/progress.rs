//! `/api/progress` — the playback cursor, as the watch and Candela will use it.

mod common;

use axum::http::StatusCode;
use common::{assert_status, body_json, fixture, fixture_progress};

#[tokio::test]
async fn get_progress_starts_at_zero() {
    // Two chapters, one with audio, no story row yet.
    let f = fixture();
    let v = body_json(f.get("/api/progress").await).await;

    assert_eq!(v["consumed_through"], 0, "not started");
    assert_eq!(v["latest_chapter"], 2);
    assert_eq!(v["chapters_ahead"], 2);
    // Chapter 2 has no audio, so only one is actually playable.
    assert_eq!(v["ready_ahead"], 1);
    assert_eq!(v["initialised"], false);
}

/// The distinction a watch showing "3 ready" depends on: a text-only chapter counts as
/// ahead but not as playable.
#[tokio::test]
async fn ready_ahead_excludes_chapters_without_audio() {
    let f = fixture();
    let v = body_json(f.get("/api/progress").await).await;

    assert_eq!(v["chapters_ahead"], 2);
    assert_eq!(v["ready_ahead"], 1);
    assert_ne!(
        v["chapters_ahead"], v["ready_ahead"],
        "a text-only chapter must not be reported as playable"
    );
    // Next existing chapter is 1; next *playable* is also 1 here.
    assert_eq!(v["next_chapter"], 1);
    assert_eq!(v["next_playable"], 1);
    assert_eq!(v["next_pcm_url"], "http://10.0.6.107:8093/media/0001.pcm");
}

#[tokio::test]
async fn buffer_health_compares_ready_against_target() {
    let f = fixture();
    let v = body_json(f.get("/api/progress").await).await;

    let target = v["buffer_target"].as_u64().unwrap();
    let ready = v["ready_ahead"].as_u64().unwrap();
    assert!(
        target >= 2,
        "litrpg-config enforces a floor of 2 (spec §6.0)"
    );
    assert_eq!(v["buffer_healthy"], ready >= target);
}

#[tokio::test]
async fn put_progress_advances_and_echoes_full_state() {
    let f = fixture_progress(5, 5);
    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 3}))
        .await;
    assert_status(&resp, StatusCode::OK);

    let v = body_json(resp).await;
    assert_eq!(v["consumed_through"], 3);
    // The write returns the same shape as GET, so no follow-up read is needed.
    assert_eq!(v["latest_chapter"], 5);
    assert_eq!(v["chapters_ahead"], 2);
    assert_eq!(v["ready_ahead"], 2);
    assert_eq!(v["next_chapter"], 4);

    // And it persisted.
    let after = body_json(f.get("/api/progress").await).await;
    assert_eq!(after["consumed_through"], 3);
}

/// `0` legitimately means "not started" and must be accepted — a client needs a way to
/// undo a mistaken mark-as-listened.
#[tokio::test]
async fn zero_is_allowed() {
    let f = fixture_progress(5, 5);
    f.put_json("/api/progress", serde_json::json!({"consumed_through": 4}))
        .await;

    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 0}))
        .await;
    assert_status(&resp, StatusCode::OK);
    assert_eq!(body_json(resp).await["consumed_through"], 0);
}

/// Re-listening is legitimate, so the cursor is a position and not a ratchet.
#[tokio::test]
async fn going_backwards_is_allowed() {
    let f = fixture_progress(5, 5);
    f.put_json("/api/progress", serde_json::json!({"consumed_through": 5}))
        .await;

    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 2}))
        .await;
    assert_status(&resp, StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["consumed_through"], 2);
    assert_eq!(
        v["chapters_ahead"], 3,
        "derived counts must follow the rewind"
    );
}

/// Beyond the end is rejected, and the message names the latest chapter — the client
/// cannot guess the bound unless told.
#[tokio::test]
async fn beyond_latest_is_rejected_naming_the_latest() {
    let f = fixture_progress(5, 5);
    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 6}))
        .await;
    assert_status(&resp, StatusCode::BAD_REQUEST);

    let msg = body_json(resp).await["error"].as_str().unwrap().to_string();
    assert!(msg.contains('6'), "must echo the rejected value: {msg}");
    assert!(msg.contains('5'), "must name the latest chapter: {msg}");
}

/// A rejected write must not have moved the cursor.
#[tokio::test]
async fn rejected_write_leaves_the_cursor_untouched() {
    let f = fixture_progress(5, 5);
    f.put_json("/api/progress", serde_json::json!({"consumed_through": 2}))
        .await;

    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 99}))
        .await;
    assert_status(&resp, StatusCode::BAD_REQUEST);

    assert_eq!(
        body_json(f.get("/api/progress").await).await["consumed_through"],
        2,
        "a rejected update must not partially apply"
    );
}

/// Setting the cursor to the last chapter empties the buffer view rather than
/// underflowing it.
#[tokio::test]
async fn cursor_at_the_end_reports_nothing_ahead() {
    let f = fixture_progress(5, 5);
    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 5}))
        .await;

    let v = body_json(resp).await;
    assert_eq!(v["chapters_ahead"], 0);
    assert_eq!(v["ready_ahead"], 0);
    assert_eq!(v["buffer_healthy"], false);
    assert!(v["next_chapter"].is_null());
    assert!(v["next_playable"].is_null());
    assert!(v["next_pcm_url"].is_null());
}

/// Without a story row the cursor has nowhere to live. The request is well-formed, so
/// this is a state precondition (409), not a malformed-request 400 and not a 500.
#[tokio::test]
async fn put_without_a_story_row_is_conflict() {
    let f = fixture(); // no story row
    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": 1}))
        .await;
    assert_status(&resp, StatusCode::CONFLICT);

    let msg = body_json(resp).await["error"].as_str().unwrap().to_string();
    assert!(
        msg.to_lowercase().contains("story"),
        "must say what is missing: {msg}"
    );
}

/// `GET` must still work before init — a client polling progress should not need the
/// deployment to be initialised first.
#[tokio::test]
async fn get_works_before_init() {
    let f = fixture();
    let resp = f.get("/api/progress").await;
    assert_status(&resp, StatusCode::OK);
    assert_eq!(body_json(resp).await["initialised"], false);
}

#[tokio::test]
async fn malformed_body_is_rejected() {
    let f = fixture_progress(5, 5);

    // Wrong type.
    let resp = f
        .put_json(
            "/api/progress",
            serde_json::json!({"consumed_through": "three"}),
        )
        .await;
    assert!(resp.status().is_client_error(), "got {}", resp.status());

    // Missing field.
    let resp = f.put_json("/api/progress", serde_json::json!({})).await;
    assert!(resp.status().is_client_error(), "got {}", resp.status());

    // Negative — `u32` cannot represent it.
    let resp = f
        .put_json("/api/progress", serde_json::json!({"consumed_through": -1}))
        .await;
    assert!(resp.status().is_client_error(), "got {}", resp.status());
}

/// The body limit must reject **before** buffering, so an unauthenticated LAN caller
/// cannot make the daemon read a large payload just to discard it.
#[tokio::test]
async fn oversized_body_is_refused() {
    let f = fixture_progress(5, 5);
    let padding = "x".repeat(litrpg_daemon::progress::MAX_PROGRESS_BODY_BYTES * 4);
    let resp = f
        .put_json(
            "/api/progress",
            serde_json::json!({"consumed_through": 1, "pad": padding}),
        )
        .await;

    assert_eq!(
        resp.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "expected the DefaultBodyLimit layer to reject, got {}",
        resp.status()
    );
}
