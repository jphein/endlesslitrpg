//! Inject realm-sigil build metadata, the Rust analogue of Go's `-ldflags -X`.
//!
//! `src/version.rs` reads these through `option_env!` with fallbacks, so this script is
//! **allowed to fail**. That is not defensive habit, it is a requirement:
//! `tools/build-on-familiar.sh` rsyncs the source with `--exclude '.git/'` on purpose, so
//! every build on the fast machine runs with no repository present. A build script that
//! demanded git would break the primary build path, and reporting `hash: dev` is a correct
//! answer for a tree with no git history rather than an error.
//!
//! Consequence worth knowing: a binary built on familiar reports `dev`, one built on katana
//! reports the real commit. That is honest — they are different builds — but it means
//! `status.realm.watch` showing `dev` tells you *where* the running binary came from, not
//! that the wiring is broken.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Re-run when HEAD moves, so the hash does not go stale across commits. Without this
    // cargo only re-runs the script when a source file changes, and a commit that touches
    // nothing in this crate would leave the old hash baked in — a version endpoint
    // confidently reporting the wrong commit, which is worse than reporting `dev`.
    if let Some(git_dir) = find_git_dir() {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
            && let Some(reference) = head.strip_prefix("ref: ").map(str::trim)
        {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }

    // Only emit a var when git actually answered. An unset var leaves `option_env!` on its
    // fallback; an empty one would render as an empty string in the JSON, which reads as a
    // populated field that happens to be blank.
    if let Some(hash) = git(&["rev-parse", "--short=12", "HEAD"]) {
        println!("cargo:rustc-env=LITRPG_GIT_HASH={hash}");
    }
    if let Some(branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        println!("cargo:rustc-env=LITRPG_GIT_BRANCH={branch}");
    }
    // `--porcelain` prints one line per change, so any output at all means dirty. Reported
    // only when the command succeeded: "not dirty" and "could not tell" are different
    // claims, and defaulting an unknown to `false` would assert a clean build we never
    // verified.
    if let Some(status) = git(&["status", "--porcelain"]) {
        println!(
            "cargo:rustc-env=LITRPG_GIT_DIRTY={}",
            !status.trim().is_empty()
        );
    }

    println!("cargo:rustc-env=LITRPG_BUILT={}", utc_now_rfc3339());
}

/// Walk up from this crate looking for `.git`. Handles both a real directory and the
/// `gitdir:` file a worktree uses, since agents here work in worktrees.
fn find_git_dir() -> Option<PathBuf> {
    let mut dir: PathBuf = std::env::var("CARGO_MANIFEST_DIR").ok()?.into();
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            // A worktree's `.git` is a file containing `gitdir: /path/to/real`.
            let contents = std::fs::read_to_string(&candidate).ok()?;
            let path = contents.strip_prefix("gitdir:")?.trim();
            return Some(Path::new(path).to_path_buf());
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// `2026-07-29T17:21:38Z` — realm-sigil's format, matching `src/datetime.rs`.
///
/// Hand-rolled rather than pulling `chrono` into a build dependency for one timestamp.
/// Civil-from-days is Howard Hinnant's algorithm, and it is exact for any date after 1970.
fn utc_now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Shift the era so March is month 0, which makes the leap day the last day of a year
    // and removes every special case from the arithmetic.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = era * 400 + yoe + i64::from(m <= 2);

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}
