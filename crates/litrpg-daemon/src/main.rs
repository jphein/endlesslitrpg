//! `litrpg-daemon` — the HTTP surface.
//!
//! Configuration is environment-driven so the unit under test stays the router, not
//! an argument parser:
//!
//! | Variable | Default |
//! |---|---|
//! | `LITRPG_BIND` | `0.0.0.0:8093` |
//! | `LITRPG_DB` | `litrpg.db` |
//! | `LITRPG_MEDIA_ROOT` | `media` |
//! | `LITRPG_TITLE` | `Endless LitRPG` |
//! | `LITRPG_DESCRIPTION` | (see `StoryConfig::default`) |
//! | `LITRPG_PROTAGONIST` | empty |
//! | `LITRPG_BASE_URL` | `http://<bind>` |

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use litrpg_daemon::config::{Config, DEFAULT_BIND, StoryConfig};
use litrpg_daemon::{AppState, router, version};
use litrpg_store::Store;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Stamp process start before any request can observe it.
    version::init();

    let bind: SocketAddr = env_or("LITRPG_BIND", DEFAULT_BIND).parse()?;
    let db = PathBuf::from(env_or("LITRPG_DB", "litrpg.db"));
    let media_root = PathBuf::from(env_or("LITRPG_MEDIA_ROOT", "media"));

    let story = StoryConfig {
        title: env_or("LITRPG_TITLE", "Endless LitRPG"),
        description: env_or("LITRPG_DESCRIPTION", "An endlessly generated LitRPG serial."),
        protagonist: env_or("LITRPG_PROTAGONIST", ""),
        base_url: env_or("LITRPG_BASE_URL", &format!("http://{bind}")),
        language: env_or("LITRPG_LANGUAGE", "en-us"),
    };

    let store = Store::open(&db)?;
    let state = Arc::new(AppState::new(
        store,
        Config::new(bind, media_root.clone()).with_story(story),
    ));

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;

    println!("litrpg-daemon listening on http://{bind}");
    println!("  db         {}", db.display());
    println!("  media root {}", media_root.display());

    axum::serve(listener, app).await?;
    Ok(())
}
