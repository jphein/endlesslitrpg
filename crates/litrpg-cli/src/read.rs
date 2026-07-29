//! `litrpg read [N]` — print a chapter's prose.
//!
//! The stored markdown is the canonical artifact (spec §8), so that is the default
//! output and it is printed verbatim. `--segments` renders the cast breakdown
//! instead, which is the view that makes a mis-attributed speaker obvious.

use litrpg_core::manifest::{BYTES_PER_MS, Segment, SpeakerKind};
use litrpg_store::Store;
use serde::Serialize;

use crate::{CliError, Result};

/// Resolve an optional chapter argument: `None` means "the latest".
///
/// Kept separate because `read`, `play` and any future chapter command must agree
/// on what "no argument" means and on how a bad number is reported.
pub fn resolve_number(store: &Store, wanted: Option<u32>) -> Result<u32> {
    let latest = store.latest_number()?;
    if latest == 0 {
        return Err(CliError::NoChapters);
    }
    match wanted {
        None => Ok(latest),
        Some(0) => Err(CliError::NoSuchChapter { wanted: 0, latest }),
        Some(n) => {
            // Ask the store rather than assuming `n <= latest` implies existence:
            // a rewind or a failed write can leave a gap in the sequence.
            match store.chapter(n) {
                Ok(_) => Ok(n),
                Err(litrpg_store::StoreError::ChapterNotFound(_)) => {
                    Err(CliError::NoSuchChapter { wanted: n, latest })
                }
                Err(e) => Err(e.into()),
            }
        }
    }
}

fn kind_str(k: SpeakerKind) -> &'static str {
    match k {
        SpeakerKind::Narrator => "narrator",
        SpeakerKind::Character => "character",
        SpeakerKind::System => "system",
    }
}

/// A segment as the CLI reports it. `litrpg-core`'s `Segment` is `Serialize`, but
/// its byte offsets are methods; spec §8.1 precomputes them in the manifest on
/// purpose, so they are materialised here too rather than left for the reader to
/// multiply by 32.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentView {
    pub idx: u32,
    pub speaker: String,
    pub kind: &'static str,
    pub voice_ref: String,
    /// `voice_ref` split on the first colon only.
    pub backend: Option<String>,
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
    pub duration_ms: u32,
    pub start_byte: u64,
    pub end_byte: u64,
}

impl From<&Segment> for SegmentView {
    fn from(s: &Segment) -> Self {
        Self {
            idx: s.idx,
            speaker: s.speaker.clone(),
            kind: kind_str(s.kind),
            backend: s.voice().ok().map(|v| v.backend),
            voice_ref: s.voice_ref.clone(),
            text: s.text.clone(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            duration_ms: s.duration_ms(),
            start_byte: s.start_byte(),
            end_byte: s.end_byte(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChapterView {
    pub number: u32,
    pub title: String,
    pub text_md: String,
    pub prompt_hash: String,
    pub pcm_path: Option<String>,
    pub mp3_path: Option<String>,
    pub duration_ms: u32,
    pub has_audio: bool,
    pub state_dirty: bool,
    pub words: usize,
    pub segments: Vec<SegmentView>,
}

pub fn read(store: &Store, wanted: Option<u32>) -> Result<ChapterView> {
    let number = resolve_number(store, wanted)?;
    let row = store.chapter(number)?;
    let segments = store.segments(number)?;
    Ok(ChapterView {
        number: row.number,
        words: row.text_md.split_whitespace().count(),
        title: row.title,
        text_md: row.text_md,
        prompt_hash: row.prompt_hash,
        pcm_path: row.pcm_path,
        mp3_path: row.mp3_path,
        duration_ms: row.duration_ms,
        has_audio: row.has_audio,
        state_dirty: row.state_dirty,
        segments: segments.iter().map(SegmentView::from).collect(),
    })
}

fn ms(total: u32) -> String {
    let secs = total / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The canonical artifact, printed verbatim after a short header.
pub fn render_prose(c: &ChapterView) -> String {
    let mut out = format!("Chapter {} — {}\n", c.number, c.title);
    let audio = if c.has_audio {
        format!("{} audio", ms(c.duration_ms))
    } else {
        "no audio".to_string()
    };
    out.push_str(&format!("{} words · {audio}\n", c.words));
    if c.state_dirty {
        out.push_str(
            "state_dirty: pass 2 failed for this chapter, so its deltas were never\nextracted — its events are not in the ledger (spec §6.0).\n",
        );
    }
    out.push('\n');
    out.push_str(&c.text_md);
    if !c.text_md.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// One line per segment: the view that makes a mis-cast speaker obvious.
pub fn render_segments(c: &ChapterView) -> String {
    let mut out = format!("Chapter {} — {}\n", c.number, c.title);

    if c.segments.is_empty() {
        out.push_str(
            "\nNo segments. Segments are written when audio is attached, so a chapter\nwhose text shipped without a render has none (spec §10).\n",
        );
        return out;
    }

    out.push_str(&format!(
        "{} segments · {} · {} B/ms\n\n",
        c.segments.len(),
        ms(c.duration_ms),
        BYTES_PER_MS
    ));

    let speaker_w = c
        .segments
        .iter()
        .map(|s| s.speaker.chars().count())
        .max()
        .unwrap_or(8)
        .max(7);
    let voice_w = c
        .segments
        .iter()
        .map(|s| s.voice_ref.chars().count())
        .max()
        .unwrap_or(9)
        .max(9);

    out.push_str(&format!(
        "{:>3}  {:<speaker_w$}  {:<9}  {:<voice_w$}  {:>16}  TEXT\n",
        "IDX", "SPEAKER", "KIND", "VOICE_REF", "MS"
    ));
    for s in &c.segments {
        out.push_str(&format!(
            "{:>3}  {:<speaker_w$}  {:<9}  {:<voice_w$}  {:>16}  {}\n",
            s.idx,
            s.speaker,
            s.kind,
            s.voice_ref,
            format!("{}-{}", s.start_ms, s.end_ms),
            s.text
        ));
    }

    // Defensive: `Store::attach_audio` rejects a non-contiguous manifest, so this
    // cannot arise through the public API. It can still arise from a hand-edited
    // database, and the consequence — byte offsets that no longer address one
    // continuous stream, so seeking lands in the wrong segment — is invisible
    // except to a listener. Cheap to surface, expensive to diagnose otherwise.
    if let Some(gap) = first_discontinuity(&c.segments) {
        out.push_str(&format!(
            "\n!! Segments are not contiguous: segment {} ends at {} ms but {} starts at {} ms.\n\
             !! Byte offsets assume one continuous PCM stream, so seeking will be wrong.\n",
            gap.0, gap.1, gap.2, gap.3
        ));
    }
    out
}

/// `(prev_idx, prev_end_ms, next_idx, next_start_ms)` of the first join that does
/// not meet exactly, or `None` when the chapter is contiguous.
///
/// Public so the predicate can be tested directly: the store now rejects a
/// non-contiguous manifest on write, so this state is unreachable through
/// `attach_audio` and could not otherwise be covered.
pub fn first_discontinuity(segments: &[SegmentView]) -> Option<(u32, u32, u32, u32)> {
    if let Some(first) = segments.first()
        && first.start_ms != 0
    {
        return Some((first.idx, 0, first.idx, first.start_ms));
    }
    segments
        .windows(2)
        .find(|w| w[0].end_ms != w[1].start_ms)
        .map(|w| (w[0].idx, w[0].end_ms, w[1].idx, w[1].start_ms))
}
