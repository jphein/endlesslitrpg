//! `litrpg rewind <N>` — destructive. Deactivate ledger rows past chapter N.
//!
//! The plan is computed with the same predicate the mutation uses
//! (`Store::rewind_preview` / `Store::rewind`), so the confirmation prompt cannot
//! describe something different from what happens.

use std::io::BufRead;

use litrpg_store::Store;
use serde::Serialize;

use crate::Result;

/// The only accepted affirmative. Not `y` — this drops state.
pub const CONFIRM_WORD: &str = "yes";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RewindPlan {
    pub through_chapter: u32,
    pub ledger_rows: usize,
    /// Distinct chapters holding rows that would be deactivated.
    pub chapters: Vec<u32>,
}

impl RewindPlan {
    pub fn is_noop(&self) -> bool {
        self.ledger_rows == 0
    }
}

pub fn plan(store: &Store, through_chapter: u32) -> Result<RewindPlan> {
    let (ledger_rows, chapters) = store.rewind_preview(through_chapter)?;
    Ok(RewindPlan {
        through_chapter,
        ledger_rows,
        chapters,
    })
}

pub fn execute(store: &Store, through_chapter: u32) -> Result<usize> {
    Ok(store.rewind(through_chapter)?)
}

/// Whether to proceed. `force` skips the prompt; otherwise the reader must supply
/// exactly `yes` (case-insensitive, surrounding whitespace ignored).
///
/// Takes a `BufRead` rather than reading stdin directly so the decision is
/// testable without a terminal.
pub fn confirmed(input: &mut impl BufRead, force: bool) -> std::io::Result<bool> {
    if force {
        return Ok(true);
    }
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        // EOF — a piped invocation with nothing to say is not consent.
        return Ok(false);
    }
    Ok(line.trim().eq_ignore_ascii_case(CONFIRM_WORD))
}

pub fn render_plan(p: &RewindPlan) -> String {
    if p.is_noop() {
        return format!(
            "Nothing to rewind: no active ledger rows after chapter {}.\n",
            p.through_chapter
        );
    }
    let chapters = p
        .chapters
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Rewind through chapter {through}\n\n  ledger rows to deactivate   {rows}\n  chapters affected           {chapters}\n\nThis is reversible only by hand: the rows are marked inactive, not deleted,\nbut their reason is overwritten. Chapter text and audio are left alone.\n",
        through = p.through_chapter,
        rows = p.ledger_rows,
    )
}

pub fn render_prompt(p: &RewindPlan) -> String {
    format!(
        "Type {CONFIRM_WORD:?} to deactivate {} ledger row(s): ",
        p.ledger_rows
    )
}

pub fn render_done(through_chapter: u32, rows: usize) -> String {
    format!(
        "Rewound through chapter {through_chapter}: {rows} ledger row(s) deactivated.\nThe snapshot is now the fold of what remains.\n"
    )
}

pub fn render_aborted() -> String {
    "Aborted. Nothing changed.\n".to_string()
}
