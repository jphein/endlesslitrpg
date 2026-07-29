//! `litrpg play [N]` — hand a chapter's audio to whatever player is installed.
//!
//! # Players are paired with the format they can actually decode
//!
//! The obvious implementation — resolve `mp3_path` and pass it to the first
//! available player — is wrong for half the candidate list. `paplay` decodes what
//! libsndfile handles (wav, flac, ogg) and `aplay` handles wav; **neither can decode
//! mp3**. Handing either an `.mp3` fails, or worse, plays the compressed bytes as
//! though they were samples.
//!
//! Both *can* play the `.pcm` artifact, which spec §7.1 fixes as headerless 16 kHz
//! mono s16le, given explicit format flags. So each candidate declares the source it
//! wants, and the rate comes from `litrpg_core::manifest::SAMPLE_RATE_HZ` rather than
//! a literal, so it cannot drift from the contract the renderer honours.
//!
//! One consequence worth knowing: §8 prunes `.pcm` outside the buffer window, so the
//! ALSA/PulseAudio candidates have no source for an older chapter and are skipped for
//! it. `mpv`/`ffplay` are unaffected because `.mp3` is permanent.

use std::path::{Path, PathBuf};
use std::process::Command;

use litrpg_core::manifest::SAMPLE_RATE_HZ;
use litrpg_store::Store;
use serde::Serialize;

use crate::read::resolve_number;
use crate::{CliError, Result};

/// Which artifact a player needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Compressed, permanent (spec §8).
    Mp3,
    /// Headerless 16 kHz mono s16le; pruned outside the buffer window.
    RawPcm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    /// Command plus its fixed arguments. The file path is appended last.
    pub argv: Vec<String>,
    pub source: Source,
}

impl Player {
    fn new(source: Source, argv: &[&str]) -> Self {
        Self {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            source,
        }
    }

    pub fn command(&self) -> &str {
        &self.argv[0]
    }
}

/// Candidates in preference order: mpv, ffplay, paplay, aplay.
///
/// The raw-PCM flags are built from `SAMPLE_RATE_HZ` so they track §7.1 rather than
/// repeating `16000` in two dialects.
pub fn players() -> Vec<Player> {
    let rate = SAMPLE_RATE_HZ.to_string();
    vec![
        Player::new(Source::Mp3, &["mpv", "--no-video", "--really-quiet"]),
        Player::new(
            Source::Mp3,
            &["ffplay", "-nodisp", "-autoexit", "-loglevel", "error"],
        ),
        Player::new(
            Source::RawPcm,
            &[
                "paplay",
                "--raw",
                "--format=s16le",
                &format!("--rate={rate}"),
                "--channels=1",
            ],
        ),
        Player::new(
            Source::RawPcm,
            &["aplay", "-q", "-f", "S16_LE", "-r", &rate, "-c", "1"],
        ),
    ]
}

/// Whether `cmd` is an executable reachable from `path_env`.
///
/// A name containing a separator is treated as a literal path, so an injected
/// absolute path (tests, or a future `$LITRPG_PLAYER`) works without a PATH lookup.
pub fn on_path(cmd: &str, path_env: Option<&str>) -> Option<PathBuf> {
    #[cfg(unix)]
    fn is_exec(p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        p.is_file()
            && p.metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    fn is_exec(p: &Path) -> bool {
        p.is_file()
    }

    if cmd.contains(std::path::MAIN_SEPARATOR) {
        let p = PathBuf::from(cmd);
        return is_exec(&p).then_some(p);
    }
    path_env?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(cmd))
        .find(|p| is_exec(p))
}

/// What `play` would do. Produced without spawning anything, so `--print-command`
/// and the real invocation cannot disagree about the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlayPlan {
    pub chapter: u32,
    pub title: String,
    pub duration_ms: u32,
    pub source: Source,
    pub path: PathBuf,
    /// Full argv including the file path as the final element.
    pub argv: Vec<String>,
}

impl PlayPlan {
    /// Shell-ready rendering for `--print-command`.
    pub fn command_line(&self) -> String {
        self.argv
            .iter()
            .map(|a| {
                if a.contains(' ') || a.contains('\'') {
                    format!("{a:?}")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Resolve chapter, artifact and player without running anything.
///
/// `candidates` and `path_env` are injected so this is testable — and so the player
/// can be substituted rather than executed.
pub fn plan(
    store: &Store,
    wanted: Option<u32>,
    candidates: &[Player],
    path_env: Option<&str>,
) -> Result<PlayPlan> {
    let number = resolve_number(store, wanted)?;
    let row = store.chapter(number)?;

    if !row.has_audio {
        return Err(CliError::ChapterHasNoAudio { chapter: number });
    }

    let mp3 = row.mp3_path.as_deref().map(PathBuf::from);
    let pcm = row.pcm_path.as_deref().map(PathBuf::from);

    // `has_audio` is a database flag; the files are on a filesystem that can be
    // pruned, moved or unmounted underneath it. Distinguish "not rendered" from
    // "rendered but the file is gone" — they call for different actions.
    let mp3 = mp3.filter(|p| p.is_file());
    let pcm = pcm.filter(|p| p.is_file());
    if mp3.is_none() && pcm.is_none() {
        let recorded: Vec<String> = [row.mp3_path, row.pcm_path].into_iter().flatten().collect();
        return Err(CliError::AudioFileMissing {
            chapter: number,
            // Two distinct states, and "looked for: " with nothing after it helps
            // no one: paths recorded but pruned means restore the media; no paths
            // at all means the attach never completed and the row is inconsistent.
            looked: if recorded.is_empty() {
                "no media paths are recorded on the chapter row".to_string()
            } else {
                format!("no file at {}", recorded.join(" or "))
            },
        });
    }

    let mut tried = Vec::new();
    for p in candidates {
        let source = match p.source {
            Source::Mp3 => mp3.as_ref(),
            Source::RawPcm => pcm.as_ref(),
        };
        // Record the attempt only when the artifact exists, so "tried: aplay" does
        // not imply aplay was missing when really the .pcm had been pruned.
        let Some(path) = source else { continue };
        tried.push(p.command().to_string());
        if on_path(p.command(), path_env).is_none() {
            continue;
        }
        let mut argv = p.argv.clone();
        argv.push(path.display().to_string());
        return Ok(PlayPlan {
            chapter: number,
            title: row.title,
            duration_ms: row.duration_ms,
            source: p.source,
            path: path.clone(),
            argv,
        });
    }

    Err(CliError::NoPlayer {
        tried: if tried.is_empty() {
            "none applicable to the available artifacts".to_string()
        } else {
            tried.join(", ")
        },
    })
}

/// Run the plan. Blocks until the player exits.
///
/// A player that is missing at this point (it was on PATH during `plan`, so this is
/// a race) is reported as such rather than silently skipped; a player that runs and
/// *fails* is reported with its status, because "exited 1" and "not installed" call
/// for different responses.
pub fn spawn(plan: &PlayPlan) -> Result<()> {
    let (cmd, args) = plan.argv.split_first().expect("argv is never empty");
    match Command::new(cmd).args(args).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(CliError::PlayerFailed {
            cmd: plan.command_line(),
            status: status
                .code()
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "terminated by signal".to_string()),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(CliError::NoPlayer { tried: cmd.clone() })
        }
        Err(source) => Err(CliError::Io {
            path: PathBuf::from(cmd),
            source,
        }),
    }
}

fn ms(total: u32) -> String {
    let secs = total / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn render_plan(p: &PlayPlan) -> String {
    format!(
        "Chapter {} — {}\n  {} · {}\n  {}\n",
        p.chapter,
        p.title,
        ms(p.duration_ms),
        match p.source {
            Source::Mp3 => "mp3",
            Source::RawPcm => "raw pcm",
        },
        p.path.display()
    )
}
