//! Configuration for endless-litrpg — one loader shared by the CLI, the daemon
//! and (later) the engine, so there is exactly one definition of where the
//! database, media and story files live.
//!
//! Resolution order: `$LITRPG_CONFIG` if set, else
//! `~/.config/endlesslitrpg/config.toml`, else built-in defaults. A missing file
//! is not an error — it means "use defaults". A *malformed* file is an error,
//! because silently falling back would point the daemon at the wrong database.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Environment variable that overrides the config file location.
pub const CONFIG_ENV: &str = "LITRPG_CONFIG";

/// Path under the user's config dir, used when `$LITRPG_CONFIG` is unset.
pub const DEFAULT_CONFIG_RELPATH: &str = "endlesslitrpg/config.toml";

/// Buffer target is the number of rendered-ahead chapters (spec §6.0). Two is the
/// floor: with one, the watch runs dry during the render of the next chapter.
pub const MIN_BUFFER_TARGET: u32 = 2;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{field} must not be empty")]
    EmptyPath { field: &'static str },
    #[error("buffer_target is {got}, but the minimum is {min} (spec §6.0)")]
    BufferTargetTooLow { got: u32, min: u32 },
    #[error("bind_addr {got:?} is not a valid socket address")]
    BadBindAddr {
        got: String,
        #[source]
        source: std::net::AddrParseError,
    },
    #[error("target_words must be greater than zero")]
    ZeroTargetWords,
    #[error("ember_url must not be empty")]
    EmptyEmberUrl,
    #[error("ember_model must not be empty")]
    EmptyEmberModel,
    #[error("narrator_voice must not be empty")]
    EmptyNarratorVoice,
}

pub type Result<T> = core::result::Result<T, ConfigError>;

fn default_db_path() -> PathBuf {
    PathBuf::from("~/.local/share/endlesslitrpg/story.db")
}
fn default_media_dir() -> PathBuf {
    PathBuf::from("~/.local/share/endlesslitrpg/media")
}
fn default_story_dir() -> PathBuf {
    PathBuf::from("~/.local/share/endlesslitrpg/story")
}
fn default_ember_url() -> String {
    "http://familiar:8091".into()
}
fn default_ember_model() -> String {
    "qwen36-coder".into()
}
fn default_bind_addr() -> String {
    "0.0.0.0:8093".into()
}
fn default_buffer_target() -> u32 {
    3
}
fn default_target_words() -> u32 {
    2000
}
fn default_narrator_voice() -> String {
    "sherpa:piper-en_GB-cori-high:0".into()
}

/// The RPG-terminal voice for `[SYSTEM]` stat blocks. A neutral speaker; the robotic
/// character comes from a post-render filter chain, not from the model (spec §7.4).
fn default_system_voice() -> String {
    "sherpa:kokoro-multi-lang-v1_0:11".into()
}

/// The pool characters are drawn from, on first appearance, and kept.
///
/// Breadth matters more than it looks: once the pool is exhausted the assigner wraps
/// and two characters share a voice — a defect only ever discovered by *listening*, by
/// which point the cast table has made it permanent. These are Kokoro's labelled
/// English speakers, interleaved by gender and accent so a growing cast stays distinct
/// rather than exhausting one group first: Am-male, Am-female, Br-male, Br-female.
fn default_character_voices() -> Vec<String> {
    [18, 3, 26, 21, 11, 9, 27, 20, 13, 0, 24, 22, 16, 7, 25, 23]
        .iter()
        .map(|sid| format!("sherpa:kokoro-multi-lang-v1_0:{sid}"))
        .collect()
}

/// Seconds to wait before re-checking the buffer when there is nothing to do.
fn default_poll_interval_secs() -> u64 {
    45
}

/// Every field carries a `serde` default, so a partial config file is valid and
/// missing keys fall back rather than failing the load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_media_dir")]
    pub media_dir: PathBuf,
    /// Holds `prompt.md`, the git-tracked source of truth for the story prompt (§9.3).
    #[serde(default = "default_story_dir")]
    pub story_dir: PathBuf,
    #[serde(default = "default_ember_url")]
    pub ember_url: String,
    #[serde(default = "default_ember_model")]
    pub ember_model: String,
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    #[serde(default = "default_buffer_target")]
    pub buffer_target: u32,
    #[serde(default = "default_target_words")]
    pub target_words: u32,
    #[serde(default = "default_narrator_voice")]
    pub narrator_voice: String,
    #[serde(default = "default_system_voice")]
    pub system_voice: String,
    #[serde(default = "default_character_voices")]
    pub character_voices: Vec<String>,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            media_dir: default_media_dir(),
            story_dir: default_story_dir(),
            ember_url: default_ember_url(),
            ember_model: default_ember_model(),
            bind_addr: default_bind_addr(),
            buffer_target: default_buffer_target(),
            target_words: default_target_words(),
            narrator_voice: default_narrator_voice(),
            system_voice: default_system_voice(),
            character_voices: default_character_voices(),
            poll_interval_secs: default_poll_interval_secs(),
        }
    }
}

/// Expand a leading `~` using the home directory. Only a leading `~/` (or a bare
/// `~`) is expanded — a tilde anywhere else is a legal filename character, and
/// rewriting it would corrupt paths rather than help.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if s != "~" && !s.starts_with("~/") {
        return path.to_path_buf();
    }
    let Some(home) = dirs::home_dir() else {
        return path.to_path_buf();
    };
    match s.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => home,
    }
}

/// Pure resolution rule behind [`config_path`], split out so precedence is
/// testable without mutating process environment — `set_var` is `unsafe` in
/// edition 2024 and would race against parallel tests in the same binary.
///
/// An empty `$LITRPG_CONFIG` is treated as unset rather than as the path `""`,
/// because `LITRPG_CONFIG= litrpg status` should mean "use the default", not
/// "fail on an empty path".
pub fn resolve_config_path(
    env_value: Option<&std::ffi::OsStr>,
    user_config_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    match env_value {
        Some(p) if !p.is_empty() => Some(expand_tilde(Path::new(p))),
        _ => user_config_dir.map(|d| d.join(DEFAULT_CONFIG_RELPATH)),
    }
}

/// Where `Config::load` will look, given the current environment.
pub fn config_path() -> Option<PathBuf> {
    resolve_config_path(std::env::var_os(CONFIG_ENV).as_deref(), dirs::config_dir())
}

impl Config {
    /// Load from `$LITRPG_CONFIG`, else the user config dir, else defaults.
    pub fn load() -> Result<Self> {
        match config_path() {
            Some(p) => Self::load_from(&p),
            None => {
                let mut c = Self::default();
                c.expand_paths();
                c.validate()?;
                Ok(c)
            }
        }
    }

    /// Load a specific file. A missing file yields validated defaults; a malformed
    /// one is an error.
    pub fn load_from(path: &Path) -> Result<Self> {
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Self>(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        config.expand_paths();
        config.validate()?;
        Ok(config)
    }

    fn expand_paths(&mut self) {
        self.db_path = expand_tilde(&self.db_path);
        self.media_dir = expand_tilde(&self.media_dir);
        self.story_dir = expand_tilde(&self.story_dir);
    }

    /// Reject configurations that would fail later in a harder-to-diagnose place.
    pub fn validate(&self) -> Result<()> {
        for (field, p) in [
            ("db_path", &self.db_path),
            ("media_dir", &self.media_dir),
            ("story_dir", &self.story_dir),
        ] {
            if p.as_os_str().is_empty() {
                return Err(ConfigError::EmptyPath { field });
            }
        }
        if self.ember_url.trim().is_empty() {
            return Err(ConfigError::EmptyEmberUrl);
        }
        if self.ember_model.trim().is_empty() {
            return Err(ConfigError::EmptyEmberModel);
        }
        if self.narrator_voice.trim().is_empty() {
            return Err(ConfigError::EmptyNarratorVoice);
        }
        if self.buffer_target < MIN_BUFFER_TARGET {
            return Err(ConfigError::BufferTargetTooLow {
                got: self.buffer_target,
                min: MIN_BUFFER_TARGET,
            });
        }
        if self.target_words == 0 {
            return Err(ConfigError::ZeroTargetWords);
        }
        self.parsed_bind_addr()?;
        Ok(())
    }

    /// `bind_addr` as a real `SocketAddr`, so the daemon never re-parses a string.
    pub fn parsed_bind_addr(&self) -> Result<SocketAddr> {
        self.bind_addr
            .parse()
            .map_err(|source| ConfigError::BadBindAddr {
                got: self.bind_addr.clone(),
                source,
            })
    }

    /// `story_dir/prompt.md` — the git-tracked prompt source of truth (§9.3).
    pub fn prompt_path(&self) -> PathBuf {
        self.story_dir.join("prompt.md")
    }

    /// Create a commented starter config if none exists. Returns `true` when a
    /// file was written, `false` when one was already there (never overwrites).
    pub fn write_default_if_absent(path: &Path) -> Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(path, STARTER_CONFIG).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(true)
    }
}

/// Written by `write_default_if_absent`. Values here must match the `default_*`
/// functions above; a test parses this string and compares it to `Config::default`
/// so the two cannot drift apart.
pub const STARTER_CONFIG: &str = r#"# endless-litrpg configuration
#
# Loaded from $LITRPG_CONFIG if set, otherwise
# ~/.config/endlesslitrpg/config.toml. Every key is optional — anything omitted
# falls back to the built-in default shown here. A leading ~ is expanded.

# SQLite database. The only file that holds state; back this up.
db_path = "~/.local/share/endlesslitrpg/story.db"

# Chapter artifacts: NNNN.md, NNNN.json, NNNN.mp3, NNNN.pcm (spec §8).
media_dir = "~/.local/share/endlesslitrpg/media"

# Holds prompt.md, the git-tracked story prompt. Reloaded at chapter
# boundaries only, never mid-chapter (spec §9.3).
story_dir = "~/.local/share/endlesslitrpg/story"

# Ember, the local model that writes the prose.
ember_url = "http://familiar:8091"
ember_model = "qwen36-coder"

# Daemon listen address. Plain HTTP, no TLS — a hard requirement inherited
# from the watch (spec §9.1).
bind_addr = "0.0.0.0:8093"

# Rendered-ahead chapters to keep. Minimum 2: with one, the watch runs dry
# while the next chapter renders (spec §6.0).
buffer_target = 3

# Target chapter length. ~2000 words is roughly 13 minutes of narration.
target_words = 2000

# Default narrator voice, as backend:model:speaker_id (spec §7.3).
narrator_voice = "sherpa:piper-en_GB-cori-high:0"
"#;
