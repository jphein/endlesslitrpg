//! `litrpg status` — buffer health, the validation-gate drift signal, and whether
//! a prompt edit is waiting to take effect.
//!
//! Spec §6.2 makes the rejection rate the early warning that the prompt or the
//! state format is slipping. It is reported as a rate with its top codes, not
//! buried under the chapter counts.

use std::path::PathBuf;

use litrpg_core::hash::content_hash;
use litrpg_store::Store;
use serde::Serialize;

use crate::{CliError, Result};

/// Rate at or above which the text renderer marks the rejection line as a
/// warning. A heuristic, not a spec value — see [`StatusReport::drift_warning`].
pub const DRIFT_WARN_RATE: f64 = 0.05;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectionCount {
    pub code: String,
    pub count: i64,
}

/// Whether the prompt on disk matches the one the engine is actually using.
///
/// `story.prompt_hash` is the prompt **currently in effect**: §9.3 reloads only at
/// chapter boundaries, so after an edit it deliberately lags the file. That lag is
/// not a defect to hide — comparing it against the file is the only way to tell the
/// operator "your edit is real but has not been picked up yet", which nothing else
/// surfaces.
///
/// The file hashed is the one `story.prompt_path` names, not `config.prompt_path()`.
/// The row records where the in-effect prompt came from, so if the config's
/// `story_dir` has since moved, the row's view is the honest comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PromptSync {
    /// No story row at all — nothing has been initialised.
    NotInitialised,
    /// The story row names a prompt file that is not on disk.
    Missing { path: PathBuf, in_effect: String },
    /// Disk and in-effect agree; there is nothing to say.
    InSync { hash: String },
    /// An edit exists on disk that the engine has not loaded yet.
    Pending {
        path: PathBuf,
        in_effect: String,
        on_disk: String,
    },
}

impl PromptSync {
    pub fn edit_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

/// Compare the in-effect prompt hash against the file the story row names.
pub fn prompt_sync(store: &Store) -> Result<PromptSync> {
    let Some(row) = store.story()? else {
        return Ok(PromptSync::NotInitialised);
    };
    let path = PathBuf::from(&row.prompt_path);
    match std::fs::read_to_string(&path) {
        Ok(body) => {
            let on_disk = content_hash(&body);
            if on_disk == row.prompt_hash {
                Ok(PromptSync::InSync { hash: on_disk })
            } else {
                Ok(PromptSync::Pending {
                    path,
                    in_effect: row.prompt_hash,
                    on_disk,
                })
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PromptSync::Missing {
            path,
            in_effect: row.prompt_hash,
        }),
        // An unreadable-but-present prompt (permissions, a directory in its place)
        // is a real failure, not a "missing" — do not flatten the two.
        Err(source) => Err(CliError::Io { path, source }),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    pub latest_chapter: u32,
    pub total_chapters: usize,
    pub chapters_with_audio: usize,
    /// Contiguous run of rendered chapters counting back from the latest — the
    /// closest thing to "how much is ready to play next".
    ///
    /// This is **not** true buffer depth. The schema has no playback cursor, so
    /// nothing records what has been consumed; a real depth needs that cursor.
    pub rendered_tail: usize,
    pub buffer_target: u32,
    pub buffer_ok: bool,
    /// Chapters whose pass 2 failed, so deltas were never extracted (§6.0).
    pub dirty_chapters: Vec<u32>,
    pub applied_deltas: i64,
    pub rejected_deltas: i64,
    /// `rejected / (applied + rejected)`; 0.0 when no deltas exist at all.
    pub rejection_rate: f64,
    pub top_rejections: Vec<RejectionCount>,
    pub prompt: PromptSync,
    /// Duplicates `prompt`'s `Pending` tag on purpose: a script wants a stable
    /// top-level boolean, not a match on the enum tag.
    pub prompt_edit_pending: bool,
}

impl StatusReport {
    pub fn total_deltas(&self) -> i64 {
        self.applied_deltas + self.rejected_deltas
    }

    /// Whether the rejection rate deserves attention.
    ///
    /// Caveat worth stating: §6.2's signal is a *rising* rate, and a single
    /// snapshot cannot show a trend. This flags an absolute level, which is a
    /// proxy. Sampling `status --json` over time is what actually shows drift.
    pub fn drift_warning(&self) -> bool {
        self.total_deltas() > 0 && self.rejection_rate >= DRIFT_WARN_RATE
    }
}

pub fn status(store: &Store, buffer_target: u32) -> Result<StatusReport> {
    let chapters = store.chapters_since(0)?;
    let latest_chapter = store.latest_number()?;
    let chapters_with_audio = chapters.iter().filter(|c| c.has_audio).count();

    // `chapters_since` orders ascending, so the tail is the trailing run.
    let rendered_tail = chapters.iter().rev().take_while(|c| c.has_audio).count();

    let applied_deltas = store.applied_count()?;
    let rejected_deltas = store.rejected_count()?;
    let total = applied_deltas + rejected_deltas;
    let rejection_rate = if total == 0 {
        0.0
    } else {
        rejected_deltas as f64 / total as f64
    };

    let top_rejections = store
        .rejection_reasons()?
        .into_iter()
        .map(|(code, count)| RejectionCount { code, count })
        .collect();

    let prompt = prompt_sync(store)?;

    Ok(StatusReport {
        prompt_edit_pending: prompt.edit_pending(),
        prompt,
        latest_chapter,
        total_chapters: chapters.len(),
        chapters_with_audio,
        rendered_tail,
        buffer_target,
        buffer_ok: rendered_tail >= buffer_target as usize,
        dirty_chapters: store.dirty_chapters()?,
        applied_deltas,
        rejected_deltas,
        rejection_rate,
        top_rejections,
    })
}

/// Human-readable rendering. The rejection block comes first among the delta
/// lines and is prefixed when it warrants attention, per §6.2.
/// Prompt-sync block. `InSync` renders nothing — a status command that says
/// "everything is fine" about every subsystem trains you to stop reading it.
fn render_prompt_sync(out: &mut String, r: &StatusReport) {
    match &r.prompt {
        PromptSync::InSync { .. } => {}

        PromptSync::NotInitialised => {
            out.push_str(
                "!! Not initialised — there is no story row.\n\
                 !! Run `litrpg init` before anything else.\n\n",
            );
        }

        PromptSync::Missing { path, in_effect } => {
            out.push_str(&format!(
                "!! The story prompt is missing from disk:\n\
                 !!   {}\n\
                 !! The engine still holds {in_effect} as the prompt in effect, so\n\
                 !! chapters keep generating — but the file of record is gone.\n\
                 !! Restore it from git, or run `litrpg prompt` to start a new one.\n\n",
                path.display()
            ));
        }

        PromptSync::Pending {
            path,
            in_effect,
            on_disk,
        } => {
            out.push_str(&format!(
                "Prompt edit pending\n\
                 \x20 on disk    {on_disk}\n\
                 \x20 in effect  {in_effect}\n\
                 \x20 file       {}\n\n\
                 \x20 {} has been edited but not loaded. It takes effect at the next\n\
                 \x20 chapter boundary; the chapter generating now keeps the old prompt (§9.3).\n\n",
                path.display(),
                path.file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "The prompt".to_string()),
            ));
        }
    }
}

pub fn render_text(r: &StatusReport) -> String {
    let mut out = String::new();
    render_prompt_sync(&mut out, r);

    out.push_str("Chapters\n");
    out.push_str(&format!("  latest            {}\n", r.latest_chapter));
    out.push_str(&format!("  total             {}\n", r.total_chapters));
    out.push_str(&format!(
        "  with audio        {} of {}\n",
        r.chapters_with_audio, r.total_chapters
    ));

    let flag = if r.buffer_ok { "ok" } else { "BELOW TARGET" };
    out.push_str(&format!(
        "  rendered ahead    {} (target {}) — {flag}\n",
        r.rendered_tail, r.buffer_target
    ));

    if !r.dirty_chapters.is_empty() {
        let list = r
            .dirty_chapters
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("  state_dirty       {list}\n"));
    }

    out.push_str("\nValidation gate (spec §6.2 — a rising reject rate means prompt drift)\n");
    if r.total_deltas() == 0 {
        out.push_str("  no deltas recorded yet\n");
        return out;
    }

    let marker = if r.drift_warning() { "  ** " } else { "     " };
    out.push_str(&format!(
        "{marker}reject rate    {:.1}%  ({} rejected / {} total)\n",
        r.rejection_rate * 100.0,
        r.rejected_deltas,
        r.total_deltas()
    ));
    out.push_str(&format!("     applied        {}\n", r.applied_deltas));

    if !r.top_rejections.is_empty() {
        out.push_str("\n  Top rejection reasons\n");
        for rc in &r.top_rejections {
            out.push_str(&format!("    {:>5}  {}\n", rc.count, rc.code));
        }
    }

    if r.drift_warning() {
        out.push_str(&format!(
            "\n  ** reject rate is at or above {:.0}% — check the prompt and the\n     state-format instructions before this becomes canon.\n",
            DRIFT_WARN_RATE * 100.0
        ));
    }

    out
}
