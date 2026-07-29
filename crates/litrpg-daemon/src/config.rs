//! Daemon configuration.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Default bind address. Port 8093 was verified free on `familiar` (spec §9.1).
///
/// `0.0.0.0` rather than a loopback because the watch reaches this daemon across
/// the `/24` at a literal IP — there is no DNS and no reverse proxy in front.
pub const DEFAULT_BIND: &str = "0.0.0.0:8093";

/// Story-level metadata served by `/api/story` and used as the RSS channel header.
///
/// TODO(litrpg-store): the `story` table already holds `title`, `protagonist`,
/// `prompt_path`, `prompt_hash`, `arc_outline_md` and `target_words`, but
/// `litrpg-store` exposes no accessor for it and `Store::conn` is `pub(crate)`, so
/// the daemon cannot read it. Once a `Store::story()` accessor exists it should
/// become the authority for `title`/`protagonist` and these fields should narrow to
/// deployment-only concerns (`base_url`, `description`, `language`). Reported to the
/// lead rather than fixed here — `litrpg-store` is owned by another agent.
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

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// Root holding `NNNN.pcm` / `NNNN.mp3`, 4-digit zero-padded.
    pub media_root: PathBuf,
    pub story: StoryConfig,
}

impl Config {
    pub fn new(bind: SocketAddr, media_root: impl Into<PathBuf>) -> Self {
        Self {
            bind,
            media_root: media_root.into(),
            story: StoryConfig::default(),
        }
    }

    pub fn with_story(mut self, story: StoryConfig) -> Self {
        self.story = story;
        self
    }
}
