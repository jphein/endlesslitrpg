//! Daemon configuration.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use litrpg_config::{ConfigError, expand_tilde};

/// Default bind address. Port 8093 was verified free on `familiar` (spec §9.1).
///
/// `0.0.0.0` rather than a loopback because the watch reaches this daemon across
/// the `/24` at a literal IP — there is no DNS and no reverse proxy in front.
///
/// Only a fallback now: the authority is `litrpg_config::Config::bind_addr`.
pub const DEFAULT_BIND: &str = "0.0.0.0:8093";

// ── Env override names ──────────────────────────────────────────────────────
// Kept even though `litrpg-config` owns the file: environment variables are how
// systemd units and tests express one-off overrides, and losing them would force a
// temp config file for every such case.
pub const ENV_BIND: &str = "LITRPG_BIND";
pub const ENV_DB: &str = "LITRPG_DB";
pub const ENV_MEDIA_ROOT: &str = "LITRPG_MEDIA_ROOT";
pub const ENV_TITLE: &str = "LITRPG_TITLE";
pub const ENV_DESCRIPTION: &str = "LITRPG_DESCRIPTION";
pub const ENV_PROTAGONIST: &str = "LITRPG_PROTAGONIST";
pub const ENV_BASE_URL: &str = "LITRPG_BASE_URL";
pub const ENV_LANGUAGE: &str = "LITRPG_LANGUAGE";

/// Story-level metadata served by `/api/story` and used as the RSS channel header.
///
/// Deliberately **daemon-local** rather than part of `litrpg-config`: every field here
/// is about how this story is *published* (feed identity, absolute enclosure base,
/// feed language), not about how the system runs. `litrpg-config` is shared with the
/// engine and CLI, and neither has any use for an RSS channel description. `base_url`
/// in particular cannot be inferred — the daemon does not know its own externally
/// reachable address — so it is deployment configuration by nature.
///
/// `title` and `protagonist` also exist as columns in the `story` table, and
/// `Store::story()` is now the **authority** for both — these two are only the
/// bootstrap fallback used before `litrpg init` has written a row (see
/// `chapters::get_story` and `state::get_protagonist`). They are kept rather than
/// removed so a fresh deployment serves something sensible instead of empty strings.
#[derive(Debug, Clone)]
pub struct StoryConfig {
    pub title: String,
    pub description: String,
    /// Default `subject` for the watch's character/stats screens (spec §9.4.1).
    pub protagonist: String,
    /// Absolute base for RSS enclosure URLs, e.g. `http://10.0.6.107:8093`.
    ///
    /// RSS enclosures must be absolute (podcast clients resolve nothing), and the
    /// daemon cannot infer its own reachable address behind any hop, so this is
    /// deployment configuration rather than something to guess from a `Host` header.
    pub base_url: String,
    pub language: String,
}

impl Default for StoryConfig {
    fn default() -> Self {
        Self {
            title: "Endless LitRPG".to_string(),
            description: "An endlessly generated LitRPG serial.".to_string(),
            protagonist: String::new(),
            base_url: format!("http://{DEFAULT_BIND}"),
            language: "en-us".to_string(),
        }
    }
}

/// Resolved daemon configuration.
///
/// The shared fields (`bind`, `db_path`, `media_root`) originate in
/// [`litrpg_config::Config`] so the daemon, engine and CLI cannot disagree about where
/// the database and media live. [`Config::load_layered`] applies env overrides on top.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// SQLite database. From `litrpg_config::Config::db_path`.
    pub db_path: PathBuf,
    /// Root holding `NNNN.pcm` / `NNNN.mp3`, 4-digit zero-padded. From
    /// `litrpg_config::Config::media_dir`.
    pub media_root: PathBuf,
    pub story: StoryConfig,
}

/// Read an env var, treating empty/whitespace as unset.
///
/// `LITRPG_BIND=""` in a systemd unit is far more likely to mean "I didn't set this"
/// than "bind to the empty string", and the latter can only fail anyway.
fn env_override(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_path(key: &str) -> Option<PathBuf> {
    // `expand_tilde` on env values too: `LITRPG_DB=~/litrpg.db` from a shell that did
    // not expand it would otherwise create a literal `./~` directory.
    env_override(key).map(|v| expand_tilde(Path::new(&v)))
}

impl Config {
    /// Direct construction, for tests and explicit wiring. Defaults `db_path` to the
    /// shared crate's default so it is never silently empty.
    pub fn new(bind: SocketAddr, media_root: impl Into<PathBuf>) -> Self {
        Self {
            bind,
            db_path: litrpg_config::Config::default().db_path,
            media_root: media_root.into(),
            story: StoryConfig::default(),
        }
    }

    pub fn with_story(mut self, story: StoryConfig) -> Self {
        self.story = story;
        self
    }

    pub fn with_db_path(mut self, db_path: impl Into<PathBuf>) -> Self {
        self.db_path = db_path.into();
        self
    }

    /// Build from an already-resolved shared config, with **no** env layer. The
    /// deterministic half of [`Config::load_layered`], so tests can exercise mapping
    /// without touching process environment.
    pub fn from_shared(shared: &litrpg_config::Config) -> Result<Self, ConfigError> {
        let bind = shared.parsed_bind_addr()?;
        Ok(Self {
            bind,
            db_path: expand_tilde(&shared.db_path),
            media_root: expand_tilde(&shared.media_dir),
            story: StoryConfig {
                base_url: format!("http://{bind}"),
                ..StoryConfig::default()
            },
        })
    }

    /// Full resolution order, lowest precedence first:
    ///
    /// 1. **defaults** — `litrpg_config::Config::default()`
    /// 2. **config file** — `litrpg-config`'s TOML, whatever it sets
    /// 3. **environment** — `LITRPG_BIND`, `LITRPG_DB`, `LITRPG_MEDIA_ROOT`, …
    /// 4. **explicit argument** — [`Config::with_story`] / [`Config::with_db_path`],
    ///    applied by the caller after this returns
    ///
    /// Steps 1–2 are `litrpg_config`'s own layering (every field has a serde default,
    /// so a partial file falls back rather than failing). This function adds step 3.
    ///
    /// `base_url` derives from the **final** bind address, so overriding the port via
    /// `LITRPG_BIND` keeps RSS enclosure URLs pointing at the right place instead of
    /// silently advertising the config file's port.
    pub fn load_layered() -> Result<Self, ConfigError> {
        let shared = litrpg_config::Config::load()?;
        Self::layer_env_onto(shared)
    }

    /// [`Config::load_layered`] against a caller-supplied base — the seam that makes
    /// precedence testable without a config file on disk.
    pub fn layer_env_onto(shared: litrpg_config::Config) -> Result<Self, ConfigError> {
        let mut cfg = Self::from_shared(&shared)?;

        if let Some(raw) = env_override(ENV_BIND) {
            cfg.bind = raw.parse().map_err(|source| ConfigError::BadBindAddr {
                got: raw.clone(),
                source,
            })?;
        }
        if let Some(p) = env_path(ENV_DB) {
            cfg.db_path = p;
        }
        if let Some(p) = env_path(ENV_MEDIA_ROOT) {
            cfg.media_root = p;
        }

        cfg.story = StoryConfig {
            title: env_override(ENV_TITLE).unwrap_or(cfg.story.title),
            description: env_override(ENV_DESCRIPTION).unwrap_or(cfg.story.description),
            protagonist: env_override(ENV_PROTAGONIST).unwrap_or(cfg.story.protagonist),
            // Derived from the resolved bind unless explicitly overridden.
            base_url: env_override(ENV_BASE_URL).unwrap_or(format!("http://{}", cfg.bind)),
            language: env_override(ENV_LANGUAGE).unwrap_or(cfg.story.language),
        };

        Ok(cfg)
    }
}
