//! `litrpg render <N>` — regenerate a chapter's audio from its recorded segment plan.
//!
//! # What it fixes
//!
//! Clearing `has_audio` puts the chapter back in front of the engine's resume path, which
//! rebuilds the render from the persisted `segments` rows — keeping `idx`, `speaker`,
//! `kind` and `text`, because that content is already published, while **re-deriving
//! `voice_ref` from the cast** (`litrpg_engine::cycle::Engine::replan_from_store`).
//!
//! So this fixes:
//!
//! - a render that failed, leaving prose with no audio
//! - a corrupt or truncated media file
//! - a stale coarse manifest — it re-splits to the current granularity, which took one
//!   chapter from a single 570-second segment to 83
//! - **a wrong voice**, including one a past substitution made permanent, and a deliberate
//!   `litrpg cast set` that existing audio predates
//!
//! That last one was not true before #15: the resume path used to copy each row's
//! `voice_ref` verbatim, so a chapter rendered by an Azure-only build stayed Azure even
//! under a build that had sherpa. Chapters 3 and 4 of the live story were exactly that.
//!
//! Because a re-render now *changes* voices rather than preserving them, the command
//! reports which segments will change before it queues them — four segments of narration
//! silently changing speaker is not something to discover by listening.
//!
//! # What this command is not
//!
//! It does not render. It clears a flag and reports what will happen.
//!
//! Whether the engine is running is answered from `Store::engine_heartbeat` — a signal the
//! engine writes about itself. Probing the daemon's port would have reported on the wrong
//! process, since the engine and daemon are separate binaries. With no heartbeat the report
//! degrades to stating the dependency rather than guessing at it.

use litrpg_core::VoiceRef;
use litrpg_store::Store;
use serde::Serialize;

use crate::cast::{VoiceDivergence, voice_divergence};
use crate::engine::EngineStatus;
use crate::play::media_path;
use crate::read::resolve_number;
use crate::{CliError, Result};

/// What happened to one chapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// `has_audio` was set and is now clear; the engine will re-render.
    Queued,
    /// Already awaiting a render, so nothing changed. Worth saying, not an error.
    AlreadyQueued,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChapterOutcome {
    pub chapter: u32,
    pub title: String,
    pub outcome: Outcome,
    /// Duration of the audio being replaced, when there was any.
    pub replacing_ms: u32,
    /// Whether the existing media is still on disk. It is left in place: the engine
    /// overwrites it on success, and until then it remains playable.
    pub media_on_disk: bool,
    /// Recorded segment voices that disagree with the cast. Since #15 a re-render
    /// re-derives from the cast, so these are the voices this command **will change**.
    pub voice_divergence: Vec<VoiceDivergence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderReport {
    pub chapters: Vec<ChapterOutcome>,
    pub poll_interval_secs: u64,
    pub engine: EngineStatus,
    /// Backends the cast references that the running engine does not provide. A
    /// re-render would substitute for these again rather than restoring them.
    pub missing_backends: Vec<String>,
}

impl RenderReport {
    pub fn queued(&self) -> usize {
        self.chapters
            .iter()
            .filter(|c| c.outcome == Outcome::Queued)
            .count()
    }
}

/// Which chapters to queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Explicit numbers, in the order given.
    These(Vec<u32>),
    /// Inclusive range.
    Range { from: u32, to: u32 },
    /// Every chapter that currently has audio.
    All,
}

impl Selection {
    /// Parse a positional argument list: `3`, `3 5 7`, or `3..7`.
    ///
    /// A range must be spelled `from..to`; `--all` is a separate flag rather than a
    /// magic value, because "re-render the whole serial" should not be what a mistyped
    /// range does.
    pub fn parse(args: &[String]) -> Result<Self> {
        if args.len() == 1
            && let Some((a, b)) = args[0].split_once("..")
        {
            let from = parse_number(a.trim_end_matches('='))?;
            let to = parse_number(b)?;
            if from > to {
                return Err(CliError::BadRange {
                    got: args[0].clone(),
                    why: "the range starts after it ends".to_string(),
                });
            }
            return Ok(Self::Range { from, to });
        }
        let mut numbers = Vec::new();
        for a in args {
            numbers.push(parse_number(a)?);
        }
        Ok(Self::These(numbers))
    }
}

fn parse_number(s: &str) -> Result<u32> {
    s.trim().parse::<u32>().map_err(|_| CliError::BadRange {
        got: s.to_string(),
        why: "not a chapter number".to_string(),
    })
}

/// Resolve a selection to chapter numbers, refusing any that do not exist.
fn resolve(store: &Store, selection: &Selection) -> Result<Vec<u32>> {
    match selection {
        Selection::These(ns) => {
            let mut out = Vec::new();
            for n in ns {
                // Same treatment as `read`: a chapter that does not exist is refused
                // with the latest named, rather than silently doing nothing.
                out.push(resolve_number(store, Some(*n))?);
            }
            Ok(out)
        }
        Selection::Range { from, to } => {
            let mut out = Vec::new();
            for n in *from..=*to {
                out.push(resolve_number(store, Some(n))?);
            }
            Ok(out)
        }
        Selection::All => Ok(store
            .chapters_since(0)?
            .into_iter()
            .filter(|c| c.has_audio)
            .map(|c| c.number)
            .collect()),
    }
}

/// Backends the cast's voice refs name, deduplicated.
fn cast_backends(store: &Store) -> Result<Vec<String>> {
    let mut out: Vec<String> = store
        .cast()?
        .iter()
        .filter_map(|c| VoiceRef::parse(&c.voice_ref).ok())
        .map(|v| v.backend)
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn render(
    store: &Store,
    selection: &Selection,
    media_dir: &std::path::Path,
    poll_interval_secs: u64,
    engine: EngineStatus,
) -> Result<RenderReport> {
    let numbers = resolve(store, selection)?;
    let mut chapters = Vec::new();
    for number in numbers {
        let row = store.chapter(number)?;
        let cleared = store.clear_audio(number)?;
        chapters.push(ChapterOutcome {
            chapter: number,
            title: row.title,
            outcome: if cleared {
                Outcome::Queued
            } else {
                Outcome::AlreadyQueued
            },
            replacing_ms: row.duration_ms,
            media_on_disk: media_path(media_dir, number, "mp3").is_file(),
            voice_divergence: voice_divergence(store, number)?,
        });
    }
    let missing_backends = engine.missing_backends(&cast_backends(store)?);
    Ok(RenderReport {
        chapters,
        poll_interval_secs,
        engine,
        missing_backends,
    })
}

fn ms(total: u32) -> String {
    let secs = total / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn render_text(r: &RenderReport) -> String {
    if r.chapters.is_empty() {
        return "No chapters have audio, so there is nothing to re-render.\n".to_string();
    }

    let mut out = String::new();
    for c in &r.chapters {
        match c.outcome {
            Outcome::Queued => out.push_str(&format!(
                "Chapter {} — {} · queued (replacing {} of audio)\n",
                c.chapter,
                c.title,
                ms(c.replacing_ms)
            )),
            Outcome::AlreadyQueued => out.push_str(&format!(
                "Chapter {} — {} · already awaiting a render, unchanged\n",
                c.chapter, c.title
            )),
        }
    }

    if r.queued() == 0 {
        return out;
    }

    out.push_str(&format!(
        "\n{} chapter(s) queued — this command clears a flag and renders nothing itself.\n",
        r.queued()
    ));
    out.push_str(&crate::engine::describe(&r.engine, r.poll_interval_secs));

    // The voices a re-render will change. Reported per chapter and before queueing,
    // because audio silently changing voice is the failure this whole area exists around.
    let diverged: Vec<&ChapterOutcome> = r
        .chapters
        .iter()
        .filter(|c| !c.voice_divergence.is_empty())
        .collect();
    if !diverged.is_empty() {
        out.push_str(
            "\n   Re-rendering re-derives voices from the cast, so these segments will\n\
             \x20  change voice — the cast value is what comes back:\n",
        );
        for c in diverged {
            for d in &c.voice_divergence {
                out.push_str(&format!(
                    "     chapter {} · {}: {} -> {}\n",
                    c.chapter, d.speaker, d.recorded, d.cast_says
                ));
            }
        }
    }

    if !r.missing_backends.is_empty() {
        out.push_str(&format!(
            "\n!! The cast references {} which the running engine does not provide, so any\n\
             !! segment naming it would be substituted again on this engine. Start an engine\n\
             !! built with that backend before re-rendering.\n",
            r.missing_backends
                .iter()
                .map(|b| format!("`{b}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        ));
    }

    out.push_str(
        "\nRe-rendering **replaces** the audio, and the manifest is regenerated at\n\
         whatever granularity the current engine produces — which is the point if the old\n\
         one is coarse, but it does mean existing timings change.\n",
    );

    if r.chapters.iter().any(|c| c.media_on_disk) {
        out.push_str(
            "\nThe existing audio is left on disk and stays playable until the engine\n\
             overwrites it, so a queued chapter is not a silent one.\n",
        );
    }
    out
}
