//! `litrpg` — argument parsing and printing only. All behaviour lives in
//! `litrpg_cli`'s modules so it can be tested without spawning a process.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use litrpg_cli::{cast, init, note, play, prompt, read, render, rewind, state, status};
use litrpg_config::Config;
use litrpg_store::Store;

#[derive(Parser)]
#[command(
    name = "litrpg",
    about = "Control the endless LitRPG story engine",
    version
)]
struct Cli {
    /// Config file to use (default: $LITRPG_CONFIG, else ~/.config/endlesslitrpg/config.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Emit machine-readable JSON (supported by status, state and cast)
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create config, directories, database and story row. Idempotent.
    Init {
        /// Rewrite prompt.md and refresh the story row. A config.toml that loads is
        /// kept — replacing it would repoint this install at the default paths
        #[arg(long)]
        force: bool,
        /// Story title (only applied when the story row is created, or with --force)
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// Protagonist name — resolves /api/character with no subject
        #[arg(long, value_name = "NAME")]
        protagonist: Option<String>,
    },

    /// Edit the story prompt in $EDITOR; takes effect at the next chapter boundary
    Prompt,

    /// Queue a director note for the next chapter
    Note {
        /// The note text
        text: String,
    },

    /// Buffer health and validation-gate drift signal
    Status,

    /// List or override speaker voices
    Cast {
        #[command(subcommand)]
        action: Option<CastAction>,
    },

    /// Print the folded ledger snapshot
    State {
        /// Limit to one subject, showing equipment and appearance
        subject: Option<String>,
    },

    /// Deactivate ledger rows past chapter N (destructive)
    Rewind {
        /// Keep chapters up to and including this number
        chapter: u32,
        /// Skip the interactive confirmation
        #[arg(long)]
        force: bool,
    },

    /// Print a chapter's prose (default: the latest)
    Read {
        /// Chapter number; omit for the latest
        chapter: Option<u32>,
        /// One line per segment: speaker, kind, voice and timing
        #[arg(long)]
        segments: bool,
    },

    /// Play a chapter's audio (default: the latest)
    Play {
        /// Chapter number; omit for the latest
        chapter: Option<u32>,
        /// Print the command instead of running it
        #[arg(long)]
        print_command: bool,
    },

    /// Re-render audio for a chapter (not implemented yet)
    Render {
        /// Chapter number
        chapter: u32,
    },
}

#[derive(Subcommand)]
enum CastAction {
    /// Assign a voice to a speaker
    Set {
        speaker: String,
        voice_ref: String,
        /// Add a speaker who is not yet in the cast
        #[arg(long)]
        new: bool,
        /// Kind for a new cast member: narrator, character or system
        #[arg(long)]
        kind: Option<String>,
    },
}

fn load_config(explicit: Option<&PathBuf>) -> Result<Config> {
    match explicit {
        Some(p) => {
            Config::load_from(p).with_context(|| format!("loading config from {}", p.display()))
        }
        None => Config::load().context("loading config"),
    }
}

fn open_store(config: &Config) -> Result<Store> {
    if let Some(parent) = config.db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    Store::open(&config.db_path)
        .with_context(|| format!("opening database {}", config.db_path.display()))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

/// `init` is handled before the normal config load because it *creates* the config
/// it needs. Requiring a loadable config first would mean a malformed file blocks
/// the one command able to repair it (`init --force`).
fn run_init(cli: &Cli, opts: &init::InitOptions) -> Result<()> {
    let path = cli.config.clone().or_else(litrpg_config::config_path);
    let (_config, report) = init::init(path.as_deref(), opts)?;
    if cli.json {
        print_json(&report)?;
    } else {
        print!("{}", init::render_text(&report));
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Command::Init {
        force,
        title,
        protagonist,
    } = &cli.command
    {
        return run_init(
            &cli,
            &init::InitOptions {
                force: *force,
                title: title.clone(),
                protagonist: protagonist.clone(),
            },
        );
    }

    let config = load_config(cli.config.as_ref())?;

    match &cli.command {
        Command::Init { .. } => unreachable!("handled before the config load"),
        Command::Prompt => {
            let path = config.prompt_path();
            let editor = std::env::var("EDITOR").ok();
            let candidates = prompt::resolve_editor(editor.as_deref());
            let outcome = prompt::edit_prompt(&path, &candidates)?;
            if cli.json {
                print_json(&serde_json::json!({
                    "path": outcome.path,
                    "created": outcome.created,
                    "is_placeholder": outcome.is_placeholder,
                    "changed": outcome.changed,
                    "hash": outcome.hash,
                    "previous_hash": outcome.previous_hash,
                    "bytes": outcome.bytes,
                }))?;
            } else {
                print!("{}", prompt::render_text(&outcome));
            }
        }

        Command::Note { text } => {
            let store = open_store(&config)?;
            let added = note::add(&store, text)?;
            if cli.json {
                print_json(&added)?;
            } else {
                print!("{}", note::render_text(&added));
            }
        }

        Command::Status => {
            let store = open_store(&config)?;
            let report = status::status(&store, config.buffer_target)?;
            if cli.json {
                print_json(&report)?;
            } else {
                print!("{}", status::render_text(&report));
            }
        }

        Command::Cast { action } => {
            let store = open_store(&config)?;
            match action {
                None => {
                    let entries = cast::list(&store)?;
                    if cli.json {
                        print_json(&entries)?;
                    } else {
                        print!("{}", cast::render_list(&entries));
                    }
                }
                Some(CastAction::Set {
                    speaker,
                    voice_ref,
                    new,
                    kind,
                }) => {
                    let outcome = cast::set(&store, speaker, voice_ref, *new, kind.as_deref())?;
                    if cli.json {
                        print_json(&outcome)?;
                    } else {
                        print!("{}", cast::render_set(&outcome));
                    }
                }
            }
        }

        Command::State { subject } => {
            let store = open_store(&config)?;
            let report = state::state(&store, subject.as_deref())?;
            if cli.json {
                print_json(&report)?;
            } else if let Some(s) = subject {
                print!("{}", state::render_subject(&report, s));
            } else {
                print!("{}", state::render_all(&report));
            }
        }

        Command::Rewind { chapter, force } => {
            let store = open_store(&config)?;
            let plan = rewind::plan(&store, *chapter)?;
            print!("{}", rewind::render_plan(&plan));

            if plan.is_noop() {
                return Ok(());
            }

            if !*force {
                print!("{}", rewind::render_prompt(&plan));
                std::io::stdout().flush()?;
            }
            let mut stdin = std::io::stdin().lock();
            if !rewind::confirmed(&mut stdin, *force)? {
                print!("{}", rewind::render_aborted());
                return Ok(());
            }

            let rows = rewind::execute(&store, *chapter)?;
            print!("{}", rewind::render_done(*chapter, rows));
        }

        Command::Read { chapter, segments } => {
            let store = open_store(&config)?;
            let view = read::read(&store, *chapter)?;
            if cli.json {
                print_json(&view)?;
            } else if *segments {
                print!("{}", read::render_segments(&view));
            } else {
                print!("{}", read::render_prose(&view));
            }
        }

        Command::Play {
            chapter,
            print_command,
        } => {
            let store = open_store(&config)?;
            let path_env = std::env::var("PATH").ok();
            let plan = play::plan(&store, *chapter, &play::players(), path_env.as_deref())?;
            if cli.json {
                print_json(&plan)?;
            } else if *print_command {
                println!("{}", plan.command_line());
            } else {
                print!("{}", play::render_plan(&plan));
                play::spawn(&plan)?;
            }
        }

        Command::Render { chapter } => {
            let store = open_store(&config)?;
            let stub = render::render(&store, *chapter)?;
            if cli.json {
                print_json(&stub)?;
            } else {
                print!("{}", render::render_text(&stub));
            }
        }
    }

    Ok(())
}
