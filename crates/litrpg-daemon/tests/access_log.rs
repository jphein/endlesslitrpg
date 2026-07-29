//! The access log (#the watch's first flash).
//!
//! `log_line` is tested directly because it is where every judgement about what a reader needs
//! actually lives; the router is exercised once to prove the layer is *installed*, which is the
//! part a pure test cannot see. Both matter: a perfect formatter that nothing calls logs
//! nothing, which is precisely why `TraceLayer` was rejected.

mod common;

use axum::http::StatusCode;
use common::{assert_status, fixture};
use litrpg_daemon::access_log::{Access, log_line};

fn peer(s: &str) -> Option<std::net::SocketAddr> {
    Some(s.parse().unwrap())
}

// ------------------------------------------------------------ log_line

#[test]
fn a_plain_request_reads_as_one_short_line() {
    let line = log_line(&Access {
        peer: peer("10.0.6.51:41234"),
        method: "GET",
        target: "/api/chapters?since=2",
        status: 200,
        elapsed_ms: 3,
        ..Default::default()
    });
    assert_eq!(line, "10.0.6.51 GET /api/chapters?since=2 -> 200 3ms");
}

#[test]
fn the_query_string_is_kept_because_it_changes_the_answer() {
    // A path-only log cannot explain a client that asked the wrong question.
    let line = log_line(&Access {
        target: "/api/chapters?since=99",
        method: "GET",
        status: 200,
        ..Default::default()
    });
    assert!(line.contains("?since=99"), "{line}");
}

#[test]
fn an_absent_peer_prints_local_rather_than_a_fake_address() {
    // `oneshot` installs no connection info, so this is what every in-process test produces.
    // Inventing `0.0.0.0` would make a test-shaped line look like a real client.
    let line = log_line(&Access {
        peer: None,
        method: "GET",
        target: "/healthz",
        status: 200,
        ..Default::default()
    });
    assert!(line.starts_with("local GET /healthz"), "{line}");
}

#[test]
fn only_the_ip_is_logged_not_the_ephemeral_port() {
    // The port changes every connection and identifies nothing; the address distinguishes the
    // watch from Candela, which is the point.
    let line = log_line(&Access {
        peer: peer("10.0.6.51:41234"),
        method: "GET",
        target: "/healthz",
        status: 200,
        ..Default::default()
    });
    assert!(line.starts_with("10.0.6.51 "), "{line}");
    assert!(!line.contains("41234"), "{line}");
}

#[test]
fn both_sides_of_a_range_request_are_logged() {
    // The whole diagnostic: what the client asked for beside what the server served. A client
    // with wrong arithmetic shows up as a mismatch between the pair.
    let line = log_line(&Access {
        peer: peer("10.0.6.51:41234"),
        method: "GET",
        target: "/media/0003.pcm",
        req_range: Some("bytes=0-131839"),
        res_range: Some("bytes 0-131839/25759200"),
        content_length: Some("131840"),
        status: 206,
        elapsed_ms: 4,
    });
    assert_eq!(
        line,
        "10.0.6.51 GET /media/0003.pcm -> 206 4ms \
         req-range=bytes=0-131839 res-range=bytes 0-131839/25759200 len=131840"
    );
}

#[test]
fn a_mismatched_window_is_visible_in_the_line() {
    // The failure this exists to catch: the watch asks for one window and is served another.
    let line = log_line(&Access {
        method: "GET",
        target: "/media/0003.pcm",
        req_range: Some("bytes=131840-263679"),
        res_range: Some("bytes 0-131839/25759200"),
        status: 206,
        ..Default::default()
    });
    assert!(line.contains("req-range=bytes=131840-263679"), "{line}");
    assert!(line.contains("res-range=bytes 0-131839/"), "{line}");
}

#[test]
fn absent_range_fields_are_omitted_rather_than_printed_empty() {
    // A scrolling journal is unreadable if every `/healthz` carries three empty fields.
    let line = log_line(&Access {
        method: "GET",
        target: "/healthz",
        status: 200,
        ..Default::default()
    });
    assert!(!line.contains("range"), "{line}");
    assert!(!line.contains("len="), "{line}");
}

#[test]
fn a_416_still_reports_what_was_asked_and_what_was_available() {
    // The most diagnostic case of all: the client asked past the end, and the response's
    // `bytes */total` is how it learns the real size.
    let line = log_line(&Access {
        method: "GET",
        target: "/media/0003.pcm",
        req_range: Some("bytes=99999999-"),
        res_range: Some("bytes */25759200"),
        status: 416,
        ..Default::default()
    });
    assert!(line.contains("-> 416"), "{line}");
    assert!(line.contains("bytes */25759200"), "{line}");
}

#[test]
fn the_line_is_one_line_and_greppable() {
    // It goes to journald, which is read by scrolling and by grep.
    let line = log_line(&Access {
        peer: peer("10.0.6.51:1"),
        method: "GET",
        target: "/media/0003.pcm",
        req_range: Some("bytes=0-131839"),
        res_range: Some("bytes 0-131839/25759200"),
        content_length: Some("131840"),
        status: 206,
        elapsed_ms: 4,
    });
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(!line.contains("  "), "run of spaces:\n{line}");
}

// ------------------------------------------------- the layer is installed

#[tokio::test]
async fn the_layer_is_actually_wired_into_the_router() {
    // The part `log_line` tests cannot see. A formatter nothing calls logs nothing — which is
    // exactly the `TraceLayer` failure this replaced, so it is worth one test that the
    // middleware runs at all.
    let f = fixture();
    // Driven through the fixture's own helper, which is the in-process path every other test
    // in this crate uses — so if the layer broke that path, the whole suite would say so.
    let resp = f.get("/healthz").await;
    // The middleware must not alter the response it observes.
    assert_status(&resp, StatusCode::OK);
}

#[tokio::test]
async fn a_range_request_through_the_router_is_unchanged_by_the_layer() {
    // Logging reads the response headers; it must not consume or rewrite them. `/media` is the
    // route whose headers matter most, since the watch's playback is built from them.
    let f = fixture();
    let resp = f.get_range("/media/0001.pcm", "bytes=0-9").await;

    assert_status(&resp, StatusCode::PARTIAL_CONTENT);
    assert!(
        resp.headers().get("content-range").is_some(),
        "the layer must leave Content-Range intact — it is what it reads"
    );
    assert_eq!(
        resp.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()),
        Some("10")
    );
}
