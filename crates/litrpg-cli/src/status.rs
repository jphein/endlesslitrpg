//! `litrpg status` — buffer health, the validation-gate drift signal, and whether
//! a prompt edit is waiting to take effect.
//!
//! Spec §6.2 makes the rejection rate the early warning that the prompt or the
//! state format is slipping. It is reported as a rate with its top codes, not
//! buried under the chapter counts.

use std::path::{Path, PathBuf};

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

/// Buffer depth measured from the playback cursor.
///
/// Before the cursor existed this was a proxy — the contiguous rendered run counting
/// back from the latest chapter — because nothing recorded what had been consumed.
/// With `story.consumed_through` there is a real baseline, so the numbers are now
/// measured from it and the baseline is reported alongside them.
///
/// Two numbers, not one, because they answer different questions and their
/// disagreement is itself a signal: `chapters_ahead` is what a buffer-fill decision
/// counts, while `playable_ahead` is what a listener gets before hitting an unrendered
/// gap. If chapters 6 and 8 are rendered but 7 is not, "2 ahead" overstates the
/// listening experience by exactly the gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BufferView {
    pub consumed_through: u32,
    pub chapters_ahead: usize,
    pub playable_ahead: usize,
    pub buffer_target: u32,
    pub buffer_ok: bool,
}

impl BufferView {
    /// True when rendered-but-unreachable chapters exist past a gap.
    pub fn has_gap(&self) -> bool {
        self.chapters_ahead > self.playable_ahead
    }

    pub fn shortfall(&self) -> u32 {
        (self.buffer_target as usize)
            .saturating_sub(self.playable_ahead)
            .try_into()
            .unwrap_or(u32::MAX)
    }
}

pub fn buffer_view(store: &Store, buffer_target: u32) -> Result<BufferView> {
    let consumed_through = store.consumed_through()?;
    let rows = store.chapters_since(consumed_through)?;

    let chapters_ahead = rows.iter().filter(|c| c.has_audio).count();

    // `chapters_since` is ordered ascending, so the playable run is the prefix whose
    // numbers are consecutive from the cursor and which all have audio.
    let mut playable_ahead = 0usize;
    let mut expected = consumed_through.saturating_add(1);
    for c in &rows {
        if c.number == expected && c.has_audio {
            playable_ahead += 1;
            expected = expected.saturating_add(1);
        } else {
            break;
        }
    }

    Ok(BufferView {
        consumed_through,
        chapters_ahead,
        playable_ahead,
        buffer_target,
        buffer_ok: playable_ahead >= buffer_target as usize,
    })
}

/// Whether the prompt on disk matches the one the engine is actually using.
///
/// `story.prompt_hash` is the prompt **currently in effect**: §9.3 reloads only at
/// chapter boundaries, so after an edit it deliberately lags the file. That lag is
/// not a defect to hide — comparing it against the file is the only way to tell the
/// operator "your edit is real but has not been picked up yet", which nothing else
/// surfaces.
///
/// Migration 004 made `story.prompt_path` **relative to `story_dir`**, so it is
/// resolved with `litrpg_config::resolve_path` — the same rule the config uses for its
/// own paths: expand `~`, then join against `story_dir` only if still relative.
///
/// Reusing that rule rather than a bare `join` buys two things. A row still holding an
/// absolute path from before 004 keeps working, so the migration cannot strand a
/// database. And if an operator ever does point the prompt somewhere else, an absolute
/// value is honoured instead of being mangled into `<story_dir>/home/jp/...`.
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
pub fn prompt_sync(store: &Store, story_dir: &Path) -> Result<PromptSync> {
    let Some(row) = store.story()? else {
        return Ok(PromptSync::NotInitialised);
    };
    let path = litrpg_config::resolve_path(Path::new(&row.prompt_path), story_dir);
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
    /// How far the listener has got. The explicit baseline every "ahead" number
    /// below is measured from.
    pub consumed_through: u32,
    /// Rendered chapters after the cursor, gaps included.
    pub chapters_ahead: usize,
    /// Contiguous rendered run starting at `consumed_through + 1` — what can
    /// actually be played before stalling. `buffer_ok` is measured on this.
    pub playable_ahead: usize,
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
    /// Re-checked here as well as at `init`, because editing `prompt.md` can introduce
    /// the mismatch long after setup.
    pub protagonist: String,
    pub protagonist_check: crate::naming::ProtagonistCheck,
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

pub fn status(store: &Store, buffer_target: u32, story_dir: &Path) -> Result<StatusReport> {
    let chapters = store.chapters_since(0)?;
    let latest_chapter = store.latest_number()?;
    let chapters_with_audio = chapters.iter().filter(|c| c.has_audio).count();
    let buffer = buffer_view(store, buffer_target)?;

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

    let prompt = prompt_sync(store, story_dir)?;
    let protagonist = store.story()?.map(|r| r.protagonist).unwrap_or_default();
    let protagonist_check = match &prompt {
        // Only meaningful against a prompt that exists; the prompt block already
        // reports the missing-file and not-initialised cases.
        PromptSync::InSync { .. } | PromptSync::Pending { .. } => {
            let row_path = store.story()?.map(|r| r.prompt_path).unwrap_or_default();
            crate::naming::check_protagonist_file(
                &protagonist,
                &litrpg_config::resolve_path(Path::new(&row_path), story_dir),
            )?
        }
        _ => crate::naming::ProtagonistCheck::Unset,
    };

    Ok(StatusReport {
        prompt_edit_pending: prompt.edit_pending(),
        prompt,
        protagonist,
        protagonist_check,
        latest_chapter,
        total_chapters: chapters.len(),
        chapters_with_audio,
        consumed_through: buffer.consumed_through,
        chapters_ahead: buffer.chapters_ahead,
        playable_ahead: buffer.playable_ahead,
        buffer_target,
        buffer_ok: buffer.buffer_ok,
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
fn render_protagonist(out: &mut String, r: &StatusReport) {
    if let Some(w) = crate::naming::warning(&r.protagonist_check, &r.protagonist) {
        out.push_str(&w);
        out.push('\n');
    }
}

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
    render_protagonist(&mut out, r);

    out.push_str("Chapters\n");
    out.push_str(&format!("  latest            {}\n", r.latest_chapter));
    out.push_str(&format!("  total             {}\n", r.total_chapters));
    out.push_str(&format!(
        "  with audio        {} of {}\n",
        r.chapters_with_audio, r.total_chapters
    ));

    out.push_str(&format!(
        "  listened through  {}\n",
        if r.consumed_through == 0 {
            "nothing yet".to_string()
        } else {
            r.consumed_through.to_string()
        }
    ));
    let flag = if r.buffer_ok { "ok" } else { "BELOW TARGET" };
    out.push_str(&format!(
        "  playable ahead    {} of {} (from chapter {}) — {flag}\n",
        r.playable_ahead,
        r.buffer_target,
        r.consumed_through + 1
    ));
    if r.chapters_ahead > r.playable_ahead {
        out.push_str(&format!(
            "  !! {} more chapter(s) are rendered but sit past an unrendered gap,\n\
             \x20    so they cannot be reached by playing straight through.\n",
            r.chapters_ahead - r.playable_ahead
        ));
    }

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
