//! One line per request, so a client can be debugged from the server side.
//!
//! Written because the watch's first flash produced "working but buggy" and there was **no
//! way to see what it had asked for**. The daemon logged its startup banner and nothing
//! else, so every client-side question — did it request the right chapter, is its Range
//! arithmetic sound, is it retrying — was unanswerable from here. A service the watch,
//! Candela and a podcast app all talk to needs to be able to say what it was asked.
//!
//! # Why not `TraceLayer`
//!
//! `tower-http` is already a dependency with its `trace` feature on, but `TraceLayer` emits
//! through `tracing`, and this binary installs no subscriber — so it would have logged
//! nothing while looking correct. Adding `tracing_subscriber` would also change the format
//! of a running service's output for one debugging need. `println!` matches what `main.rs`
//! already does.
//!
//! # What is logged, and what deliberately is not
//!
//! Method, path, status, client address, elapsed ms, and the `Range` header when present —
//! Range is the whole point, since the watch's playback is Range arithmetic and a wrong
//! window is invisible in a 200. Query strings are included because `?since=N` changes the
//! answer.
//!
//! **Both sides of the range, not just the request.** `req-range` is what the client asked
//! for; `res-range` is the `Content-Range` the server actually served, and `len` the bytes it
//! promised. A client whose arithmetic is wrong produces a *mismatch between the pair*, which
//! is visible at a glance — where a request-only log leaves the reader to recompute the window
//! by hand from a file size they do not have. Diagnosing "working but buggy" playback is
//! exactly reading those two against each other.
//!
//! # `elapsed_ms` is time to the response, not time to transfer it
//!
//! `/media` streams its body (`Body::from_stream`), so the middleware resumes once the headers
//! and the stream are set up — not when the last byte reaches the watch. For a 25 MB PCM range
//! that difference is the entire transfer. The field is still worth having (it separates a slow
//! handler from a slow network) but it must not be read as "how long the request took", and
//! measuring the true transfer would mean wrapping every response body — real risk, added to a
//! live service, for a diagnostic.
//!
//! Not logged: request or response bodies. A director note is the listener's own words and
//! has no business in the journal.

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::http::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use axum::middleware::Next;
use axum::response::Response;

/// Everything one line reports. A struct rather than eight positional arguments, which would
/// trip `clippy::too_many_arguments` and read worse at the call site.
#[derive(Debug, Clone, Default)]
pub struct Access<'a> {
    /// `None` when the router is driven in-process — `tower::ServiceExt::oneshot` installs no
    /// connection info — which is exactly how this crate's tests run.
    pub peer: Option<SocketAddr>,
    pub method: &'a str,
    /// Path *and* query.
    pub target: &'a str,
    /// The client's `Range` request header.
    pub req_range: Option<&'a str>,
    /// The server's `Content-Range` response header.
    pub res_range: Option<&'a str>,
    /// The server's `Content-Length`, as sent.
    pub content_length: Option<&'a str>,
    pub status: u16,
    pub elapsed_ms: u128,
}

/// Format one access-log line. Split out from the middleware so it is testable without a
/// router, a socket or a clock.
///
/// Printing `local` for an absent peer rather than a fake address keeps a test-shaped line
/// honest. Absent range fields are omitted entirely rather than printed empty, so a line about
/// `/api/chapters` stays short enough to read in a scrolling journal.
pub fn log_line(a: &Access<'_>) -> String {
    let who = a
        .peer
        .map(|p| p.ip().to_string())
        .unwrap_or_else(|| "local".to_string());
    let mut out = format!(
        "{who} {} {} -> {} {}ms",
        a.method, a.target, a.status, a.elapsed_ms
    );
    if let Some(r) = a.req_range {
        out.push_str(&format!(" req-range={r}"));
    }
    if let Some(r) = a.res_range {
        out.push_str(&format!(" res-range={r}"));
    }
    if let Some(l) = a.content_length {
        out.push_str(&format!(" len={l}"));
    }
    out
}

/// Log every request after it completes, so the status is real rather than predicted.
pub async fn access_log(req: Request, next: Next) -> Response {
    // Read from extensions rather than taking `ConnectInfo` as an extractor: the extractor
    // form would make connection info *required*, and the in-process test path does not
    // install it, so every test would start failing on a logging change.
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let method = req.method().to_string();
    // Path *and* query — `?since=N` changes the response, so a path-only log cannot explain
    // a client that asked the wrong question.
    let target = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let started = Instant::now();
    let res = next.run(req).await;

    // Read from the response so the pair can be compared. A wrong client window shows up as a
    // `res-range` that does not match the `req-range`, which is the whole diagnostic.
    let res_range = res
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let content_length = res
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    println!(
        "{}",
        log_line(&Access {
            peer,
            method: &method,
            target: &target,
            req_range: range.as_deref(),
            res_range: res_range.as_deref(),
            content_length: content_length.as_deref(),
            status: res.status().as_u16(),
            elapsed_ms: started.elapsed().as_millis(),
        })
    );
    res
}
