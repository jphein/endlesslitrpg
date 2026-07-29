//! `litrpg story [--protagonist NAME] [--title TITLE]` — read or change story metadata.
//!
//! # Why this exists (issue #13)
//!
//! `init --force` was the only route to the `story` row, and it rewrites `prompt.md` to
//! the starter template on the way. So the only supported way to fix a protagonist name
//! destroyed the premise — which my own protagonist-mismatch warning originally
//! recommended, before smoke-testing caught it. A command that can only be reached
//! through a destructive one is not reachable.
//!
//! # Single-field writes, not read-modify-write
//!
//! Each change is one `UPDATE` via the store's single-field setters rather than a
//! `upsert_story` round-trip. With the serial running under systemd the engine writes
//! `prompt_hash` at chapter boundaries, so reading the row, changing one field and
//! writing it all back has a real window in which it would **silently revert the
//! engine's write**. One statement per field has no such window.

use litrpg_store::Store;
use serde::Serialize;

use crate::naming::{self, ProtagonistCheck};
use crate::{CliError, Result};

/// What `litrpg story` was asked to change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryEdit {
    pub protagonist: Option<String>,
    pub title: Option<String>,
}

impl StoryEdit {
    pub fn is_empty(&self) -> bool {
        self.protagonist.is_none() && self.title.is_none()
    }
}

/// One field's before/after, recorded only when it actually changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldChange {
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoryReport {
    pub title: String,
    pub protagonist: String,
    pub target_words: u32,
    pub consumed_through: u32,
    pub prompt_path: String,
    pub prompt_hash: String,
    /// Empty when only reading, or when the supplied values already matched.
    pub changes: Vec<FieldChange>,
    /// Re-run after a `--protagonist` change: a command that can *create* the
    /// prompt/protagonist mismatch should say so immediately rather than leaving it for
    /// the next `status`.
    pub protagonist_check: ProtagonistCheck,
}

impl StoryReport {
    pub fn changed(&self) -> bool {
        !self.changes.is_empty()
    }
}

/// Read the row, applying `edit` first if it asks for anything.
///
/// A blank `--protagonist ""` is refused rather than clearing the field: the protagonist
/// seeds the known-subject set, so emptying it would make every protagonist delta fail
/// as `UnknownSubject` — a large silent consequence for an argument that looks like a
/// no-op. `--title ""` is likewise refused; an untitled story has `DEFAULT_TITLE`.
pub fn story(store: &Store, edit: &StoryEdit, story_dir: &std::path::Path) -> Result<StoryReport> {
    let before = store
        .story()?
        .ok_or(CliError::Store(litrpg_store::StoreError::NoStoryRow))?;

    let mut changes = Vec::new();

    if let Some(name) = &edit.protagonist {
        let name = name.trim();
        if name.is_empty() {
            return Err(CliError::BlankStoryField {
                field: "protagonist",
                why: "it seeds the known-subject set, so clearing it would make every \
                      protagonist stat change fail as UnknownSubject",
            });
        }
        if name != before.protagonist {
            store.set_protagonist(name)?;
            changes.push(FieldChange {
                field: "protagonist",
                from: before.protagonist.clone(),
                to: name.to_string(),
            });
        }
    }

    if let Some(title) = &edit.title {
        let title = title.trim();
        if title.is_empty() {
            return Err(CliError::BlankStoryField {
                field: "title",
                why: "an untitled story should carry the placeholder title rather than an \
                      empty one",
            });
        }
        if title != before.title {
            store.set_title(title)?;
            changes.push(FieldChange {
                field: "title",
                from: before.title.clone(),
                to: title.to_string(),
            });
        }
    }

    // Re-read rather than patching the in-memory copy, so what is reported is what the
    // database holds — including anything the engine wrote concurrently.
    let after = store
        .story()?
        .ok_or(CliError::Store(litrpg_store::StoreError::NoStoryRow))?;

    let protagonist_check = naming::check_protagonist_file(
        &after.protagonist,
        &litrpg_config::resolve_path(std::path::Path::new(&after.prompt_path), story_dir),
    )?;

    Ok(StoryReport {
        title: after.title,
        protagonist: after.protagonist,
        target_words: after.target_words,
        consumed_through: after.consumed_through,
        prompt_path: after.prompt_path,
        prompt_hash: after.prompt_hash,
        changes,
        protagonist_check,
    })
}

pub fn render_text(r: &StoryReport) -> String {
    let mut out = String::new();

    for c in &r.changes {
        out.push_str(&format!("{}: {:?} -> {:?}\n", c.field, c.from, c.to));
    }
    if !r.changes.is_empty() {
        out.push('\n');
    }

    out.push_str(&format!("  title             {:?}\n", r.title));
    out.push_str(&format!(
        "  protagonist       {}\n",
        if r.protagonist.is_empty() {
            "(unset)".to_string()
        } else {
            format!("{:?}", r.protagonist)
        }
    ));
    out.push_str(&format!("  target words      {}\n", r.target_words));
    out.push_str(&format!(
        "  listened through  {}\n",
        if r.consumed_through == 0 {
            "nothing yet".to_string()
        } else {
            r.consumed_through.to_string()
        }
    ));
    out.push_str(&format!("  prompt            {}\n", r.prompt_path));
    out.push_str(&format!("  prompt hash       {}\n", r.prompt_hash));

    if let Some(w) = naming::warning(&r.protagonist_check, &r.protagonist) {
        out.push('\n');
        out.push_str(&w);
    }

    if r.changes.iter().any(|c| c.field == "protagonist") {
        out.push_str(
            "\nExisting ledger entries keep the old name — the ledger is append-only, so\n\
             this changes what *future* deltas are accepted under, not what is already\n\
             recorded. `litrpg state` flags a character split across two names.\n",
        );
    }
    out
}
