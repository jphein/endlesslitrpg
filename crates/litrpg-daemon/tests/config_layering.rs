//! Config precedence: defaults → config file → environment → explicit argument.
//!
//! Environment variables are process-global and `cargo test` runs a file's tests on
//! parallel threads, so several of these cases would otherwise race over the same
//! `LITRPG_DB`. [`EnvGuard`] serializes them on one lock and restores prior values on
//! drop, so a failing assertion cannot leak state into the next test.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use litrpg_daemon::config::{Config, StoryConfig};

/// Serializes env mutation across this file's tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Sets env vars for its lifetime and restores them on drop.
///
/// Holds the lock for as long as the guard lives, so a test body runs with exclusive
/// ownership of the process environment. Poisoning is tolerated: one failing test must
/// not cascade into "all env tests panic on a poisoned lock", which would hide the
/// original failure behind noise.
struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&str, &str)]) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            // SAFETY: the lock above guarantees no other test thread is reading or
            // writing the environment for the guard's lifetime.
            unsafe { std::env::set_var(k, v) };
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prior) in &self.saved {
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// A base that differs from `litrpg-config`'s defaults in every field we map, so a
/// test cannot pass by accidentally reading a default.
fn shared_base() -> litrpg_config::Config {
    litrpg_config::Config {
        db_path: PathBuf::from("/tmp/base-litrpg.db"),
        media_dir: PathBuf::from("/tmp/base-media"),
        bind_addr: "127.0.0.1:19999".to_string(),
        ..litrpg_config::Config::default()
    }
}

/// Layer 2: the shared config file is the authority when no env var is set.
#[test]
fn shared_config_supplies_db_media_and_bind() {
    let cfg = Config::from_shared(&shared_base()).expect("from_shared");

    assert_eq!(cfg.db_path, PathBuf::from("/tmp/base-litrpg.db"));
    assert_eq!(cfg.media_root, PathBuf::from("/tmp/base-media"));
    assert_eq!(cfg.bind.to_string(), "127.0.0.1:19999");
    // base_url derives from the resolved bind, not a hardcoded default.
    assert_eq!(cfg.story.base_url, "http://127.0.0.1:19999");
}

/// The daemon must not silently invent its own default database path — that is exactly
/// the duplication that consuming `litrpg-config` removes.
#[test]
fn direct_construction_still_uses_the_shared_default_db() {
    let cfg = Config::new("0.0.0.0:8093".parse().unwrap(), "/tmp/m");
    assert_eq!(cfg.db_path, litrpg_config::Config::default().db_path);
}

/// A bad `bind_addr` must surface as `ConfigError::BadBindAddr`, naming the value.
#[test]
fn invalid_bind_addr_is_reported_with_the_offending_value() {
    let bad = litrpg_config::Config {
        bind_addr: "not-a-socket".to_string(),
        ..litrpg_config::Config::default()
    };
    let err = Config::from_shared(&bad).expect_err("must reject");
    assert!(
        err.to_string().contains("not-a-socket"),
        "error must name the bad value, got: {err}"
    );
}

/// Layer 3: environment beats the config file.
#[test]
fn env_overrides_beat_the_config_file() {
    let _env = EnvGuard::set(&[
        ("LITRPG_BIND", "10.0.6.107:8093"),
        ("LITRPG_DB", "/tmp/env-litrpg.db"),
        ("LITRPG_MEDIA_ROOT", "/tmp/env-media"),
        ("LITRPG_PROTAGONIST", "Kael"),
    ]);

    let cfg = Config::layer_env_onto(shared_base()).expect("layered");

    assert_eq!(cfg.bind.to_string(), "10.0.6.107:8093");
    assert_eq!(cfg.db_path, PathBuf::from("/tmp/env-litrpg.db"));
    assert_eq!(cfg.media_root, PathBuf::from("/tmp/env-media"));
    assert_eq!(cfg.story.protagonist, "Kael");
    // base_url tracks the *overridden* bind, so RSS enclosures stay reachable.
    assert_eq!(cfg.story.base_url, "http://10.0.6.107:8093");
}

/// An empty env var means "unset", not "use the empty string".
#[test]
fn empty_env_var_does_not_override() {
    let _env = EnvGuard::set(&[("LITRPG_LANGUAGE", "   ")]);
    let cfg = Config::layer_env_onto(shared_base()).expect("layered");
    assert_eq!(
        cfg.story.language,
        StoryConfig::default().language,
        "whitespace-only env value must fall through to the default"
    );
}

/// An invalid env bind must fail loudly rather than falling back to the file's value —
/// silently ignoring an explicit override is worse than refusing to start.
#[test]
fn invalid_env_bind_is_an_error_not_a_fallback() {
    let _env = EnvGuard::set(&[("LITRPG_BIND", "port-eight-thousand")]);
    let err = Config::layer_env_onto(shared_base()).expect_err("must reject");
    assert!(err.to_string().contains("port-eight-thousand"));
}

/// Layer 4: explicit argument wins over everything.
#[test]
fn explicit_argument_beats_env_and_file() {
    let _env = EnvGuard::set(&[("LITRPG_DB", "/tmp/env-wins.db")]);
    let cfg = Config::layer_env_onto(shared_base())
        .expect("layered")
        .with_db_path("/tmp/explicit-wins.db");
    assert_eq!(cfg.db_path, PathBuf::from("/tmp/explicit-wins.db"));
}

/// A leading `~` in an env path must expand, or a shell that did not expand it would
/// create a literal `./~` directory.
#[test]
fn tilde_in_env_paths_is_expanded() {
    let _env = EnvGuard::set(&[("LITRPG_DB", "~/litrpg-tilde.db")]);
    let cfg = Config::layer_env_onto(shared_base()).expect("layered");
    let s = cfg.db_path.to_string_lossy().to_string();
    assert!(!s.starts_with('~'), "tilde must be expanded, got {s}");
    if let Ok(home) = std::env::var("HOME") {
        assert!(s.starts_with(&home), "expected {s} under {home}");
    }
}
