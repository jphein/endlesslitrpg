//! `litrpg-engine` — the process that actually writes the story.
//!
//! Thin on purpose: argument parsing, wiring, and a sleep. Every decision worth testing lives
//! in the library, because a binary is the one place a test cannot reach.
//!
//! ```text
//! litrpg-engine --once                 one cycle, print the outcome, exit
//! litrpg-engine                        loop until SIGINT/SIGTERM
//! litrpg-engine --chapters 3           stop after three chapters
//! litrpg-engine --drain                ignore the buffer (see below)
//! ```
//!
//! Configuration comes from `litrpg-config` — `Config::load()`, honouring `$LITRPG_CONFIG`.
//! There is deliberately no second config path; env vars override individual fields, which is
//! what systemd wants, but the file is the source of truth.
//!
//! # Failing at startup rather than mid-serial
//!
//! A missing story row, an unreachable Ember, no TTS backend, an unrenderable voice — all of
//! these are checked before the first cycle and exit non-zero with a message. The alternative
//! is a process that runs happily and produces chapters with no audio forever, which is the
//! silent degradation this whole design is built to avoid. Once running, the same conditions
//! are transient and handled by backoff.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use litrpg_engine::{
    BufferCursor, CycleOutcome, EmberGenerator, Engine, EngineConfig, FsArtifacts,
    RegistryRenderer, StoreLibrary, voices::plan_voices,
};
use litrpg_store::Store;
use litrpg_tts::{TtsBackend, TtsRegistry, azure::AzureBackend};
use tracing::{error, info, warn};

/// Long enough that an idle process is free, short enough that a consumed chapter is noticed.
/// A cycle takes 1–3 minutes, so polling faster buys nothing.
const DEFAULT_POLL_SECS: u64 = 45;

/// Startup problem: config, database, story row, Ember, TTS.
const EXIT_STARTUP: u8 = 2;

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("litrpg-engine: {msg}\n\n{USAGE}");
            return ExitCode::from(EXIT_STARTUP);
        }
    };

    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    init_logging();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "could not start the async runtime");
            return ExitCode::from(EXIT_STARTUP);
        }
    };

    match runtime.block_on(run(args)) {
        Ok(code) => code,
        Err(e) => {
            error!(error = %e, "startup failed");
            ExitCode::from(EXIT_STARTUP)
        }
    }
}

const USAGE: &str = "\
litrpg-engine — generate the endless serial

USAGE:
    litrpg-engine [OPTIONS]

OPTIONS:
    --once               Run one cycle, report the outcome, exit.
    --chapters <N>       Stop once N chapters have been produced this run.
    --poll-secs <S>      Seconds to sleep between cycles (default 45).
                         Also LITRPG_POLL_SECS.
    --consumed-through <N>
                         Override the stored playback cursor for this run. By
                         default the cursor is read from the database every
                         cycle, so the daemon notices when someone listens.
    --drain              Ignore the cursor: treat the buffer as always empty so
                         generation never idles. For backfilling a story, or
                         filling a buffer ahead of a listener.
    -h, --help           Show this message.

ENVIRONMENT:
    LITRPG_CONFIG              Path to config.toml.
    RUST_LOG                   Log filter (default: info).
    LITRPG_NARRATOR_VOICE      Override config's narrator voice.
    LITRPG_SYSTEM_VOICE        Voice for [SYSTEM] blocks.
    LITRPG_CHARACTER_VOICES    Comma-separated character voice pool.
    LITRPG_POLL_SECS           Seconds between cycles.
";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    once: bool,
    help: bool,
    drain: bool,
    chapters: Option<u32>,
    poll_secs: Option<u64>,
    consumed_through: Option<u32>,
}

impl Args {
    fn parse(it: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut a = Args::default();
        let mut it = it.peekable();

        while let Some(arg) = it.next() {
            let mut value = |name: &str| -> Result<String, String> {
                it.next().ok_or_else(|| format!("{name} needs a value"))
            };
            match arg.as_str() {
                "--once" => a.once = true,
                "--drain" => a.drain = true,
                "-h" | "--help" => a.help = true,
                "--chapters" => {
                    let v = value("--chapters")?;
                    a.chapters = Some(
                        v.parse()
                            .map_err(|_| format!("--chapters {v:?} is not a number"))?,
                    );
                }
                "--poll-secs" => {
                    let v = value("--poll-secs")?;
                    a.poll_secs = Some(
                        v.parse()
                            .map_err(|_| format!("--poll-secs {v:?} is not a number"))?,
                    );
                }
                "--consumed-through" => {
                    let v = value("--consumed-through")?;
                    a.consumed_through = Some(
                        v.parse()
                            .map_err(|_| format!("--consumed-through {v:?} is not a number"))?,
                    );
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }

        Ok(a)
    }

    /// `--poll-secs`, then `$LITRPG_POLL_SECS`, then the config file, then the built-in.
    fn poll_interval(&self, from_config: Option<u64>) -> Duration {
        let secs = self
            .poll_secs
            .or_else(|| std::env::var("LITRPG_POLL_SECS").ok()?.parse().ok())
            .or(from_config)
            .unwrap_or(DEFAULT_POLL_SECS);
        Duration::from_secs(secs.max(1))
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    // A §10 degradation explains itself in a `warn!`. Without a subscriber that explanation is
    // destroyed, and `journalctl` cannot answer "why does chapter 12 have no audio".
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

async fn run(args: Args) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let config = litrpg_config::Config::load()?;
    config.validate()?;
    info!(
        db = %config.db_path.display(),
        media = %config.media_dir.display(),
        ember = %config.ember_url,
        buffer_target = config.buffer_target,
        "configuration loaded"
    );

    // ---- store + story row -------------------------------------------------
    let store = Arc::new(Mutex::new(Store::open(&config.db_path)?));
    let library = StoreLibrary::new(Arc::clone(&store), &config.story_dir);
    // Fails with "run `litrpg init` first" when the story row or prompt file is missing.
    let story = litrpg_engine::Library::story(&library)?;
    info!(
        title = %story.title,
        protagonist = %story.protagonist,
        target_words = story.target_words,
        "story loaded"
    );

    // ---- TTS ---------------------------------------------------------------
    let mut registry = TtsRegistry::new();
    match AzureBackend::from_default_config() {
        Ok(azure) => {
            info!(
                voices = TtsBackend::voices(&azure).len(),
                "azure backend ready"
            );
            registry.register(Box::new(azure))?;
        }
        Err(e) => warn!(error = %e, "azure backend unavailable"),
    }
    register_sherpa(&mut registry);

    let ready: Vec<String> = registry
        .availability()
        .into_iter()
        .filter(|(_, a)| a.is_ready())
        .map(|(id, _)| id)
        .collect();
    if ready.is_empty() {
        return Err(
            "no TTS backend is available: install sherpa models or configure Azure \
                    credentials in ~/.config/speech-to-cli/config.json"
                .into(),
        );
    }
    info!(backends = ?ready, "tts registry ready");

    // ---- voices ------------------------------------------------------------
    let base = EngineConfig::from_config(&config);
    let requested_narrator = env_or("LITRPG_NARRATOR_VOICE", &base.narrator_voice);
    let requested_system = env_or("LITRPG_SYSTEM_VOICE", &base.system_voice);
    let requested_pool = match std::env::var("LITRPG_CHARACTER_VOICES") {
        Ok(v) => v
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => base.character_voices.clone(),
    };

    let plan = plan_voices(
        &requested_narrator,
        &requested_system,
        &requested_pool,
        &ready,
        &registry.all_voices(),
    )?;
    for note in &plan.notes {
        warn!("{note}");
    }
    info!(
        narrator = %plan.narrator,
        system = %plan.system,
        characters = plan.characters.len(),
        "voice plan"
    );

    // Gender metadata comes from the registry's own catalogue, so a gender hint from pass 2
    // is matched against what the plugins actually advertise rather than against a guess.
    let voice_genders: std::collections::BTreeMap<String, litrpg_tts::Gender> = registry
        .all_voices()
        .into_iter()
        .map(|v| (v.voice_ref, v.gender))
        .collect();

    let engine_config = EngineConfig {
        narrator_voice: plan.narrator,
        system_voice: plan.system,
        character_voices: plan.characters,
        voice_genders,
        // So a `cast` row naming an unloaded backend is substituted rather than costing the
        // chapter's audio.
        registered_backends: ready.clone(),
        ..base
    };

    // The cast owns an established voice and config only seeds new ones, so a config edit against
    // an existing cast row does nothing. Correct, but silent — so say it once, at startup.
    let cast: Vec<(String, String)> = store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cast()?
        .into_iter()
        .map(|c| (c.speaker, c.voice_ref))
        .collect();
    for note in litrpg_engine::voices::config_cast_divergence(
        &cast,
        &engine_config.narrator_voice,
        &engine_config.system_voice,
    ) {
        warn!("{note}");
    }

    // ---- Ember -------------------------------------------------------------
    let generator = EmberGenerator::from_config(&config)?;
    let models = generator.client().models().await.map_err(|e| {
        format!(
            "Ember is unreachable at {}: {e}. Is qwen3-coder.service running?",
            config.ember_url
        )
    })?;
    if !models.contains(&config.ember_model) {
        return Err(format!(
            "Ember at {} does not serve model {:?}; it offers {models:?}",
            config.ember_url, config.ember_model
        )
        .into());
    }
    info!(model = %config.ember_model, "ember reachable");

    // ---- go ----------------------------------------------------------------
    let engine = Engine::with_shared_store(
        Arc::clone(&store),
        generator,
        RegistryRenderer::new(registry),
        library,
        FsArtifacts::new(&config.media_dir),
        engine_config,
    );

    if args.once {
        let outcome = one_cycle(&engine, &args).await?;
        info!(?outcome, "single cycle complete");
        return Ok(ExitCode::SUCCESS);
    }

    Ok(loop_until_signal(&engine, &args, config.poll_interval_secs).await)
}

/// Register sherpa when the feature is compiled in *and* its models pass preflight.
///
/// Gated on preflight rather than merely on the feature because sherpa-onnx calls `exit()` on
/// a missing asset instead of returning an error — a broken install would take the whole
/// daemon down mid-chapter rather than degrading.
#[cfg(feature = "sherpa")]
fn register_sherpa(registry: &mut TtsRegistry) {
    use litrpg_tts::sherpa::SherpaBackend;
    use litrpg_tts::sherpa::SherpaConfig;

    let cfg = SherpaConfig::default();
    let broken = cfg.preflight();
    if !broken.is_empty() {
        warn!(
            ?broken,
            "sherpa models incomplete; not registering the backend"
        );
        return;
    }
    if cfg.ready_models().is_empty() {
        warn!("no sherpa models installed; not registering the backend");
        return;
    }

    let backend = SherpaBackend::new(cfg);
    match registry.register(Box::new(backend)) {
        Ok(()) => info!("sherpa backend ready"),
        Err(e) => warn!(error = %e, "could not register the sherpa backend"),
    }
}

#[cfg(not(feature = "sherpa"))]
fn register_sherpa(_registry: &mut TtsRegistry) {
    info!("built without the `sherpa` feature; azure only");
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

/// The engine with its production ports bound.
type LiveEngine = Engine<EmberGenerator, RegistryRenderer, StoreLibrary, FsArtifacts>;

async fn one_cycle(
    engine: &LiveEngine,
    args: &Args,
) -> Result<CycleOutcome, Box<dyn std::error::Error>> {
    Ok(engine.run_cycle(cursor(args)).await?)
}

/// Which buffer baseline this run uses.
///
/// [`BufferCursor::Stored`] is the default, and the engine re-reads it **every cycle** — so
/// marking a chapter consumed while the daemon is running takes effect at the next turn rather
/// than at the next restart. `--drain` and `--consumed-through` override it.
fn cursor(args: &Args) -> BufferCursor {
    if args.drain {
        // The stronger claim of the two: ignore the cursor entirely.
        BufferCursor::Drain
    } else if let Some(n) = args.consumed_through {
        BufferCursor::At(n)
    } else {
        BufferCursor::Stored
    }
}

async fn loop_until_signal(engine: &LiveEngine, args: &Args, config_poll_secs: u64) -> ExitCode {
    let interval = args.poll_interval(Some(config_poll_secs));
    info!(
        poll_secs = interval.as_secs(),
        chapters = ?args.chapters,
        cursor = ?cursor(args),
        "entering the chapter loop; SIGINT or SIGTERM to stop"
    );

    let mut produced = 0u32;

    loop {
        // Checked *between* cycles, never during one. Every stage is idempotent by chapter
        // number, so stopping here is always safe; stopping mid-publish would leave a chapter
        // for the resume path to pick up, which works but is needless churn.
        if shutdown_requested().await {
            info!(
                produced,
                "shutdown signal received between cycles; stopping cleanly"
            );
            return ExitCode::SUCCESS;
        }

        let outcome = match one_cycle(engine, args).await {
            Ok(o) => o,
            Err(e) => {
                // A store or library failure is not a chapter failure; log and keep going
                // rather than taking the serial down.
                error!(error = %e, "cycle failed");
                if sleep_or_shutdown(interval).await {
                    return ExitCode::SUCCESS;
                }
                continue;
            }
        };

        match &outcome {
            CycleOutcome::Produced {
                chapter,
                has_audio,
                state_dirty,
                ..
            } => {
                produced += 1;
                info!(
                    chapter,
                    has_audio, state_dirty, produced, "chapter produced"
                );
            }
            CycleOutcome::ResumedRender { chapter, .. } => info!(chapter, "render resumed"),
            CycleOutcome::Idle { buffer_depth } => info!(buffer_depth, "idle"),
            CycleOutcome::Abandoned {
                chapter,
                reason,
                backoff,
            } => {
                warn!(chapter, %reason, backoff, "cycle abandoned")
            }
        }

        if let Some(limit) = args.chapters
            && produced >= limit
        {
            info!(produced, "reached the requested chapter count; stopping");
            return ExitCode::SUCCESS;
        }

        // An idle cycle is two SQL queries, so sleeping is what keeps an idle process free.
        if sleep_or_shutdown(interval).await {
            info!(produced, "shutdown signal received; stopping cleanly");
            return ExitCode::SUCCESS;
        }
    }
}

/// True if a shutdown signal is already pending, without waiting for one.
async fn shutdown_requested() -> bool {
    tokio::time::timeout(Duration::from_millis(0), signal())
        .await
        .is_ok()
}

/// Sleep, returning `true` if a shutdown signal arrived instead.
async fn sleep_or_shutdown(d: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(d) => false,
        () = signal() => true,
    }
}

/// Resolves when SIGINT or SIGTERM arrives. systemd sends SIGTERM.
async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal as unix_signal};
        let mut term = match unix_signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "cannot listen for SIGTERM; only SIGINT will stop this");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_means_loop_forever_against_the_stored_cursor() {
        let a = parse(&[]).unwrap();
        assert!(!a.once && !a.drain && a.chapters.is_none());
        assert_eq!(a.consumed_through, None);
        assert_eq!(cursor(&a), BufferCursor::Stored);
    }

    #[test]
    fn the_cursor_is_chosen_by_the_flags() {
        // Default reads the database each cycle; the flags override it.
        assert_eq!(cursor(&parse(&[]).unwrap()), BufferCursor::Stored);
        assert_eq!(
            cursor(&parse(&["--consumed-through", "7"]).unwrap()),
            BufferCursor::At(7)
        );
        assert_eq!(cursor(&parse(&["--drain"]).unwrap()), BufferCursor::Drain);
        // `--drain` is the stronger claim, so it wins.
        assert_eq!(
            cursor(&parse(&["--consumed-through", "7", "--drain"]).unwrap()),
            BufferCursor::Drain
        );
    }

    #[test]
    fn flags_and_values_parse() {
        let a = parse(&["--once", "--chapters", "3", "--poll-secs", "10", "--drain"]).unwrap();
        assert!(a.once && a.drain);
        assert_eq!(a.chapters, Some(3));
        assert_eq!(a.poll_secs, Some(10));

        let a = parse(&["--consumed-through", "12"]).unwrap();
        assert_eq!(a.consumed_through, Some(12));
    }

    #[test]
    fn a_bad_value_is_an_error_not_a_silent_default() {
        // Silently defaulting `--chapters banana` to "forever" would be a nasty surprise on a
        // metered TTS backend.
        assert!(parse(&["--chapters", "banana"]).is_err());
        assert!(parse(&["--poll-secs", "-1"]).is_err());
        assert!(parse(&["--chapters"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
    }

    #[test]
    fn help_is_recognised_both_ways() {
        assert!(parse(&["-h"]).unwrap().help);
        assert!(parse(&["--help"]).unwrap().help);
    }

    #[test]
    fn the_poll_interval_is_never_zero() {
        // A zero interval would spin the CPU on a full buffer.
        let a = Args {
            poll_secs: Some(0),
            ..Args::default()
        };
        assert_eq!(a.poll_interval(None), Duration::from_secs(1));
        assert_eq!(a.poll_interval(Some(0)), Duration::from_secs(1));
    }

    #[test]
    fn the_default_poll_interval_is_sane() {
        assert_eq!(
            Args::default().poll_interval(None),
            Duration::from_secs(DEFAULT_POLL_SECS)
        );
        assert!((30..=60).contains(&DEFAULT_POLL_SECS));
        // The config file is consulted, but an explicit flag still wins.
        assert_eq!(
            Args::default().poll_interval(Some(90)),
            Duration::from_secs(90)
        );
        assert_eq!(
            Args {
                poll_secs: Some(5),
                ..Args::default()
            }
            .poll_interval(Some(90)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn usage_documents_every_flag_the_parser_accepts() {
        for flag in [
            "--once",
            "--chapters",
            "--poll-secs",
            "--consumed-through",
            "--drain",
            "--help",
        ] {
            assert!(USAGE.contains(flag), "{flag} is undocumented");
        }
    }
}
