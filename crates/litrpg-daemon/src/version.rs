//! `GET /api/version` — realm-sigil compatible.
//!
//! CLAUDE.md mandates a realm-sigil `/api/version` on anything with an HTTP
//! presence. `realm-sigil/rust/` exists but is a **`no_std`, dependency-free name
//! generator** (`name_for_hex`, `seed_from_id`, word tables) — the one-line HTTP
//! handler (`sigil.Handler`) is only in the Go/Python/JS bindings. There is nothing
//! to mount, so this module hand-rolls a response matching the Go `Version` struct
//! field-for-field, keeping every consumer (status.realm.watch, the `<Sigil />`
//! badge) working unchanged.
//!
//! Two reasons the crate is not taken as a dependency:
//!
//! 1. It is not in `workspace.dependencies`, and adding it there means editing the
//!    workspace root — outside this crate's remit. An absolute `path =` to
//!    `~/Projects/realm-sigil/rust` would build only on this machine.
//! 2. Its own docs flag that the themed word tables **diverge** from Go/Python/JS
//!    (regenerated against lexicon 2026-05-07): the same commit hash yields
//!    `Blazing Jewel` there and `Draconic Monolith` here. Two names for one commit is
//!    a silent, plausible-looking wrong answer.
//!
//! Consequence: the decorative generated build *name* is absent (the `name` field
//! carries the service name, exactly as Go does — `NewVersion(name, ...)` sets it
//! from its argument, not from `GenerateName`), so the JSON schema is complete. If
//! the magical name is wanted later, add the dependency deliberately and pick a
//! realm; do not reimplement the word tables.

use std::sync::OnceLock;
use std::time::SystemTime;

use axum::Json;
use axum::http::HeaderMap;
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL, CONTENT_TYPE};
use axum::response::IntoResponse;
use serde::Serialize;

/// Build metadata, injected at compile time. The Rust analogue of Go's `-ldflags -X`
/// is a build-time environment variable read via `option_env!`, so an ordinary
/// `cargo build` still produces a valid response rather than failing to compile.
const HASH: &str = match option_env!("LITRPG_GIT_HASH") {
    Some(v) => v,
    None => "dev",
};
const BRANCH: &str = match option_env!("LITRPG_GIT_BRANCH") {
    Some(v) => v,
    None => "dev",
};
const DIRTY: &str = match option_env!("LITRPG_GIT_DIRTY") {
    Some(v) => v,
    None => "false",
};
const BUILT: &str = match option_env!("LITRPG_BUILT") {
    Some(v) => v,
    None => "unknown",
};
const REPO: &str = match option_env!("LITRPG_REPO") {
    Some(v) => v,
    None => "https://github.com/jphein/endlesslitrpg",
};

/// The sigil realm this service names its builds from.
const REALM: &str = "fantasy";

/// Process start, captured once so `uptime` and `started` agree with each other.
fn started_at() -> &'static (SystemTime, std::time::Instant) {
    static STARTED: OnceLock<(SystemTime, std::time::Instant)> = OnceLock::new();
    STARTED.get_or_init(|| (SystemTime::now(), std::time::Instant::now()))
}

/// Call once during startup so `started` reflects process start rather than the
/// first `/api/version` request.
pub fn init() {
    let _ = started_at();
}

/// Field order and `serde` names mirror realm-sigil's Go `Version` struct exactly.
#[derive(Debug, Clone, Serialize)]
pub struct Version {
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
    pub hash: &'static str,
    pub branch: &'static str,
    pub dirty: bool,
    pub built: &'static str,
    pub started: String,
    pub uptime: i64,
    pub realm: &'static str,
    pub runtime: String,
    pub os: String,
    pub host: String,
    pub pid: u32,
    pub repo: &'static str,
    pub commit_url: String,
}

fn hostname() -> String {
    // No `hostname` crate: Linux exposes this as a file, and this daemon is
    // Linux-only (it runs on `familiar`).
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

impl Version {
    pub fn current() -> Self {
        let (start_wall, start_mono) = started_at();
        Self {
            name: env!("CARGO_PKG_NAME"),
            description: env!("CARGO_PKG_DESCRIPTION"),
            version: env!("CARGO_PKG_VERSION"),
            hash: HASH,
            branch: BRANCH,
            dirty: DIRTY == "true" || DIRTY == "1",
            built: BUILT,
            started: crate::datetime::rfc3339_utc(crate::datetime::unix_secs(*start_wall)),
            // Monotonic, so a wall-clock step (NTP) cannot make uptime go backwards.
            uptime: start_mono.elapsed().as_secs() as i64,
            realm: REALM,
            runtime: format!("rust{}", env!("CARGO_PKG_RUST_VERSION")),
            os: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            host: hostname(),
            pid: std::process::id(),
            repo: REPO,
            commit_url: if HASH == "dev" {
                String::new()
            } else {
                format!("{REPO}/commit/{HASH}")
            },
        }
    }
}

/// Headers match the Go handler: JSON, uncached, CORS-open (the `<Sigil />` badge is
/// fetched cross-origin from status dashboards).
pub async fn get_version() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    headers.insert(CACHE_CONTROL, "no-cache".parse().unwrap());
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
    (headers, Json(Version::current()))
}
