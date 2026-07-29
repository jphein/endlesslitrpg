//! `litrpg note "<text>"` — queue a director note (§6.4).

use litrpg_store::Store;
use serde::Serialize;

use crate::{CliError, Result};

/// `notes.source` for anything queued from this CLI (§6.0 allows
/// `cli` | `watch` | `candela`).
pub const SOURCE_CLI: &str = "cli";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteAdded {
    pub id: i64,
    pub body: String,
    pub source: String,
    pub pending: usize,
}

/// Queue a note. The body is trimmed; whitespace-only input is refused rather
/// than stored, since a blank note would be injected into the prompt as noise.
pub fn add(store: &Store, body: &str) -> Result<NoteAdded> {
    let body = body.trim();
    if body.is_empty() {
        return Err(CliError::EmptyNote);
    }
    let id = store.insert_note(body, SOURCE_CLI)?;
    Ok(NoteAdded {
        id,
        body: body.to_string(),
        source: SOURCE_CLI.to_string(),
        pending: store.pending_notes()?.len(),
    })
}

pub fn render_text(n: &NoteAdded) -> String {
    format!(
        "Queued note {} ({} pending).\n\nConsumed at the next chapter boundary; the engine never waits for one.\n",
        n.id, n.pending
    )
}
