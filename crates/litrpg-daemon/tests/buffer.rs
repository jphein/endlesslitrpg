//! `POST /api/buffer` — the honest lever for "write another chapter".

mod common;

use axum::http::StatusCode;
use common::{assert_status, body_json, fixture, fixture_progress};

/// The seed the fixture starts from, read rather than hardcoded so these tests keep
/// meaning what they say if the configured default ever moves.
async fn current_target(f: &common::Fixture) -> u64 {
    body_json(f.get("/api/progress").await).await["buffer_target"]
        .as_u64()
        .unwrap()
}

#[tokio::test]
async fn bump_raises_the_target_and_reports_the_live_value() {
    let f = fixture();
    let before = current_target(&f).await;

    let resp = f
        .post_json(
            "/api/buffer",
            serde_json::json!({"bump": 1, "source": "voice", "justification": "cast ember continue"}),
        )
        .await;
    assert_status(&resp, StatusCode::CREATED);

    let v = body_json(resp).await;
    assert_eq!(v["buffer_target"], before + 1);
    assert_eq!(v["previous_buffer_target"], before);
    assert_eq!(v["source"], "voice");
    assert_eq!(v["justification"], "cast ember continue");
    assert!(v["id"].as_i64().unwrap() > 0);
}

/// **The two-snapshot test.** Before schema 008 the daemon served its own startup copy of
/// `buffer_target` while the engine used a different one, so a bump could change what was
/// *reported* without changing what was *used*. Both now read the same store row, so the
/// write endpoint and the read endpoint must never disagree — and neither is a restart
/// away from the truth.
#[tokio::test]
async fn the_write_and_the_read_endpoints_agree_after_a_bump() {
    let f = fixture();

    let written = body_json(
        f.post_json(
            "/api/buffer",
            serde_json::json!({"bump": 2, "source": "cli", "justification": "filling ahead"}),
        )
        .await,
    )
    .await;

    let read = body_json(f.get("/api/progress").await).await;

    assert_eq!(
        written["buffer_target"], read["buffer_target"],
        "the value POST reports and the value GET reports must be the same row, not two snapshots"
    );
    // And the derived flag must be recomputed against the new target rather than the seed.
    let ready = read["ready_ahead"].as_u64().unwrap();
    let target = read["buffer_target"].as_u64().unwrap();
    assert_eq!(read["buffer_healthy"], ready >= target);
}

/// Bumps resolve against the stored value, not against whatever the caller last saw — the
/// property that lets a fixed, arithmetic-free voice body be correct twice in a row.
#[tokio::test]
async fn successive_bumps_compound_server_side() {
    let f = fixture();
    let before = current_target(&f).await;

    for _ in 0..2 {
        let resp = f
            .post_json(
                "/api/buffer",
                serde_json::json!({"bump": 1, "source": "voice"}),
            )
            .await;
        assert_status(&resp, StatusCode::CREATED);
    }

    assert_eq!(current_target(&f).await, before + 2);
}

#[tokio::test]
async fn an_absolute_target_is_accepted_for_tools() {
    let f = fixture();
    let v = body_json(
        f.post_json(
            "/api/buffer",
            serde_json::json!({"target": 7, "source": "cli", "justification": "verify the extractor"}),
        )
        .await,
    )
    .await;
    assert_eq!(v["buffer_target"], 7);
    assert_eq!(current_target(&f).await, 7);
}

/// A spoken command may carry no dictated reason. The record says so rather than
/// inventing one — "why is the target 10?" must not be answered by a placeholder that
/// reads like a reason.
#[tokio::test]
async fn an_absent_justification_is_recorded_honestly() {
    let f = fixture();
    let v = body_json(
        f.post_json(
            "/api/buffer",
            serde_json::json!({"bump": 1, "source": "voice"}),
        )
        .await,
    )
    .await;
    assert_eq!(v["justification"], "voice command; no reason given");
}

#[tokio::test]
async fn a_blank_justification_is_treated_as_absent() {
    let f = fixture();
    let v = body_json(
        f.post_json(
            "/api/buffer",
            serde_json::json!({"bump": 1, "source": "voice", "justification": "   "}),
        )
        .await,
    )
    .await;
    assert_eq!(v["justification"], "voice command; no reason given");
}

// ---------------------------------------------------------------------------
// Guard rails
// ---------------------------------------------------------------------------

/// Well-formed but impossible → 422, distinct from the 400 a malformed body gets.
#[tokio::test]
async fn nonsense_amounts_are_unprocessable() {
    let f = fixture();
    for body in [
        serde_json::json!({"bump": 0, "source": "cli"}),
        serde_json::json!({"bump": 4, "source": "cli"}),
        serde_json::json!({"bump": 1, "target": 5, "source": "cli"}),
        serde_json::json!({"target": 1, "source": "cli"}),
        serde_json::json!({"target": 25, "source": "cli"}),
        serde_json::json!({"target": 0, "source": "cli"}),
    ] {
        let resp = f.post_json("/api/buffer", body.clone()).await;
        assert_status(&resp, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

/// A bump that would clear the ceiling is refused rather than silently clamped: a caller
/// told `201` has been told the target moved, and it must have moved by what it asked.
#[tokio::test]
async fn a_bump_past_the_ceiling_is_refused_not_clamped() {
    let f = fixture();
    f.post_json(
        "/api/buffer",
        serde_json::json!({"target": 24, "source": "cli", "justification": "at the ceiling"}),
    )
    .await;

    let resp = f
        .post_json(
            "/api/buffer",
            serde_json::json!({"bump": 1, "source": "voice"}),
        )
        .await;
    assert_status(&resp, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(current_target(&f).await, 24, "a refused bump must not write");
}

#[tokio::test]
async fn a_missing_amount_is_a_bad_request() {
    let f = fixture();
    let resp = f
        .post_json("/api/buffer", serde_json::json!({"source": "cli"}))
        .await;
    assert_status(&resp, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn source_is_whitelisted() {
    let f = fixture();
    for bad in ["web", "", "CLI", "voice; drop"] {
        let resp = f
            .post_json(
                "/api/buffer",
                serde_json::json!({"bump": 1, "source": bad}),
            )
            .await;
        assert_status(&resp, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn an_oversized_justification_is_rejected() {
    let f = fixture();
    let huge = "x".repeat(litrpg_daemon::buffer::MAX_JUSTIFICATION_BYTES + 1);
    let resp = f
        .post_json(
            "/api/buffer",
            serde_json::json!({"bump": 1, "source": "cli", "justification": huge}),
        )
        .await;
    assert_status(&resp, StatusCode::BAD_REQUEST);
}

/// The whole reason this route exists instead of overloading `/api/progress`. Advancing
/// the cursor would *also* start the engine, by lying about how far the listener has got.
#[tokio::test]
async fn the_playback_cursor_is_never_touched() {
    let f = fixture_progress(5, 5);
    f.put_json("/api/progress", serde_json::json!({"consumed_through": 3}))
        .await;

    f.post_json(
        "/api/buffer",
        serde_json::json!({"bump": 3, "source": "voice", "justification": "more please"}),
    )
    .await;

    let v = body_json(f.get("/api/progress").await).await;
    assert_eq!(
        v["consumed_through"], 3,
        "changing the buffer target must not move the listener's position"
    );
}
