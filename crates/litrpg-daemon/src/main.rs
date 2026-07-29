//! `litrpg-daemon` — the HTTP surface.
//!
//! # Configuration
//!
//! Shared settings come from **`litrpg-config`** (its TOML file, with per-field serde
//! defaults) so the daemon, engine and CLI cannot disagree about where the database and
//! media live. Environment variables layer on top — they are how systemd units and
//! tests express one-off overrides without writing a config file.
//!
//! Precedence, lowest first: **defaults → config file → environment → explicit
//! argument**. See [`litrpg_daemon::config::Config::load_layered`].
//!
//! | Variable | Overrides |
//! |---|---|
//! | `LITRPG_CONFIG` | which config file to read (honoured by `litrpg-config`) |
//! | `LITRPG_BIND` | `bind_addr` |
//! | `LITRPG_DB` | `db_path` |
//! | `LITRPG_MEDIA_ROOT` | `media_dir` |
//! | `LITRPG_TITLE` / `LITRPG_DESCRIPTION` / `LITRPG_PROTAGONIST` / `LITRPG_BASE_URL` / `LITRPG_LANGUAGE` | daemon-local `StoryConfig` |
//! | `AZURE_SPEECH_KEY` / `AZURE_SPEECH_REGION` | Azure TTS credentials (read by `litrpg-tts`) |

use std::sync::Arc;

use litrpg_daemon::config::Config;
use litrpg_daemon::{AppState, router, version};
use litrpg_store::Store;
use litrpg_tts::TtsRegistry;
use litrpg_tts::azure::{AzureBackend, AzureConfig};
use litrpg_tts::sherpa::SherpaConfig;

/// Build the registry for `GET /api/voices` and (later) rendering.
///
/// Azure is registered even when **unconfigured**: an `AzureConfig` with an empty key
/// reports `Availability::Missing` from `available()` while still advertising the
/// DragonHD catalog. That is strictly more useful to a cast-selection UI than omitting
/// the backend, which would read as "Azure does not exist" rather than "Azure needs a
/// key". `default_voice` is set to a real DragonHD name so the synthesized config
/// contributes no placeholder entry.
///
/// sherpa is registered only under the `sherpa` feature — with the default feature set
/// there is no `SherpaBackend` compiled at all, and its catalog reaches `/api/voices`
/// through `AppState::sherpa` instead (see `voices.rs`).
fn build_registry(sherpa: &SherpaConfig) -> TtsRegistry {
    let azure_config = AzureConfig::load().unwrap_or_else(|e| {
        eprintln!("  azure     unconfigured ({e}); voices listed but not assignable");
        AzureConfig {
            key: String::new(),
            region: litrpg_tts::azure::DEFAULT_REGION.to_string(),
            default_voice: "en-US-Ava:DragonHDLatestNeural".to_string(),
        }
    });

    let registry = TtsRegistry::new().with(Box::new(AzureBackend::new(azure_config)));

    #[cfg(feature = "sherpa")]
    let registry = registry.with(Box::new(litrpg_tts::sherpa::SherpaBackend::new(
        sherpa.clone(),
    )));
    #[cfg(not(feature = "sherpa"))]
    let _ = sherpa;

    registry
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Stamp process start before any request can observe it.
    version::init();

    let config = Config::load_layered()?;
    let sherpa = SherpaConfig::default();

    let store = Store::open(&config.db_path)?;
    let bind = config.bind;

    println!("litrpg-daemon listening on http://{bind}");
    println!("  config    {:?}", litrpg_config::config_path());
    println!("  db        {}", config.db_path.display());
    println!("  media     {}", config.media_root.display());
    match sherpa.availability() {
        litrpg_tts::Availability::Ready => {
            println!(
                "  sherpa    models present ({} voices)",
                sherpa.voice_descs().len()
            )
        }
        litrpg_tts::Availability::Missing { reason } => println!("  sherpa    {reason}"),
    }

    let state = Arc::new(
        AppState::new(store, config)
            .with_tts(build_registry(&sherpa))
            .with_sherpa(sherpa),
    );

    let listener = tokio::net::TcpListener::bind(bind).await?;
    // `into_make_service_with_connect_info` is what puts the peer address in each request's
    // extensions — without it every access-log line would read `local` and the watch would be
    // indistinguishable from Candela and from a curl on this box, which is most of the value
    // of having the log at all.
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
