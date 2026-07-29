//! `litrpg listened [N]` — the playback cursor.
//!
//! The cursor exists so buffer depth has a real baseline: "3 rendered ahead" is only
//! meaningful relative to where the listener actually is. So recording a position
//! reports what it means for generation, not just that it was stored.

use litrpg_store::Store;
use serde::Serialize;

use crate::status::{BufferView, buffer_view};
use crate::{CliError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BufferState {
    Below,
    At,
    Above,
}

impl BufferState {
    fn of(playable: usize, target: u32) -> Self {
        match playable.cmp(&(target as usize)) {
            std::cmp::Ordering::Less => Self::Below,
            std::cmp::Ordering::Equal => Self::At,
            std::cmp::Ordering::Greater => Self::Above,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Below => "below",
            Self::At => "at",
            Self::Above => "above",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListenedReport {
    pub position: u32,
    /// Where the cursor was before this command. `None` when only reading.
    pub previous: Option<u32>,
    /// The cursor was moved to an earlier chapter. Valid, but worth saying.
    pub moved_backwards: bool,
    pub latest_chapter: u32,
    pub buffer: BufferView,
    pub buffer_state: BufferState,
}

impl ListenedReport {
    pub fn changed(&self) -> bool {
        self.previous.is_some_and(|p| p != self.position)
    }
}

fn report(store: &Store, previous: Option<u32>, buffer_target: u32) -> Result<ListenedReport> {
    let buffer = buffer_view(store, buffer_target)?;
    let position = buffer.consumed_through;
    Ok(ListenedReport {
        position,
        moved_backwards: previous.is_some_and(|p| position < p),
        previous,
        latest_chapter: store.latest_number()?,
        buffer_state: BufferState::of(buffer.playable_ahead, buffer_target),
        buffer,
    })
}

/// Read the current position without changing it.
pub fn show(store: &Store, buffer_target: u32) -> Result<ListenedReport> {
    report(store, None, buffer_target)
}

/// Record a new position.
///
/// `0` is allowed and means "I have not started" — it is a legitimate position, not a
/// missing chapter. Any other number must name a chapter that exists, so a typo
/// cannot silently park the cursor beyond the story and make the buffer look full.
///
/// Moving backwards is allowed: re-listening is normal, and a cursor that only
/// ratchets is a high-water mark rather than a position. It is reported, because it
/// makes the engine consider the buffer fuller than it was and the operator should
/// know that was intentional.
pub fn set(store: &Store, chapter: u32, buffer_target: u32) -> Result<ListenedReport> {
    let latest = store.latest_number()?;
    if chapter != 0 {
        match store.chapter(chapter) {
            Ok(_) => {}
            Err(litrpg_store::StoreError::ChapterNotFound(_)) => {
                if latest == 0 {
                    return Err(CliError::NoChapters);
                }
                return Err(CliError::NoSuchChapter {
                    wanted: chapter,
                    latest,
                });
            }
            Err(e) => return Err(e.into()),
        }
    }

    let previous = store.consumed_through()?;
    store.set_consumed_through(chapter)?;
    report(store, Some(previous), buffer_target)
}

fn position_word(n: u32) -> String {
    if n == 0 {
        "nothing yet".to_string()
    } else {
        format!("chapter {n}")
    }
}

pub fn render_text(r: &ListenedReport) -> String {
    let mut out = String::new();

    match r.previous {
        None => out.push_str(&format!("Listened through {}\n", position_word(r.position))),
        Some(prev) if prev == r.position => out.push_str(&format!(
            "Listened through {} (unchanged)\n",
            position_word(r.position)
        )),
        Some(prev) => out.push_str(&format!(
            "Listened through {} (was {})\n",
            position_word(r.position),
            position_word(prev)
        )),
    }

    let b = &r.buffer;
    out.push_str(&format!(
        "\n  playable ahead  {} of {} — {} target\n",
        b.playable_ahead,
        b.buffer_target,
        r.buffer_state.word()
    ));
    out.push_str(&format!("  latest chapter  {}\n", r.latest_chapter));

    if b.has_gap() {
        out.push_str(&format!(
            "\n  !! {} further rendered chapter(s) sit past an unrendered gap, so\n\
             \x20    playing straight through will stall before reaching them.\n",
            b.chapters_ahead - b.playable_ahead
        ));
    }

    if r.moved_backwards {
        // Valid, but it makes the engine see a fuller buffer than a moment ago, so
        // an accidental backwards move would quietly stall generation.
        out.push_str(
            "\n  !! The cursor moved backwards. That is allowed — re-listening is normal —\n\
             \x20    but the buffer now looks fuller to the engine, so it will generate less\n\
             \x20    until you catch up. If that was a typo, set it again.\n",
        );
    }

    match r.buffer_state {
        BufferState::Below => out.push_str(&format!(
            "\n  {} more chapter(s) needed to reach the buffer target; the engine\n  should be generating.\n",
            b.shortfall()
        )),
        BufferState::At => out.push_str("\n  Buffer is exactly at target.\n"),
        BufferState::Above => out.push_str("\n  Buffer is ahead of target; the engine can idle.\n"),
    }

    out
}
