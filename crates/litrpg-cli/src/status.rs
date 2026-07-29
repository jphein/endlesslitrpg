//! `litrpg status` — buffer health plus the validation-gate drift signal.
//!
//! Spec §6.2 makes the rejection rate the early warning that the prompt or the
//! state format is slipping. It is reported as a rate with its top codes, not
//! buried under the chapter counts.

use litrpg_store::{Result, Store};
use serde::Serialize;

/// Rate at or above which the text renderer marks the rejection line as a
/// warning. A heuristic, not a spec value — see [`StatusReport::drift_warning`].
pub const DRIFT_WARN_RATE: f64 = 0.05;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectionCount {
    pub code: String,
    pub count: i64,
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

    Ok(StatusReport {
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
pub fn render_text(r: &StatusReport) -> String {
    let mut out = String::new();

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
