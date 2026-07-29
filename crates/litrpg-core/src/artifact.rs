//! Chapter artifact naming.
//!
//! §8 gives every chapter four files — `NNNN.md`, `NNNN.json`, `NNNN.mp3`, `NNNN.pcm` —
//! and migration 004 made those names the *only* record of where a chapter's media
//! lives, since the path columns that duplicated them could disagree with reality and
//! did.
//!
//! That makes the naming convention load-bearing, and it was written out **six times**:
//! the engine's `path_for`, five literals across the daemon's chapter, feed and
//! progress handlers, and the CLI's `media_path`. Nothing forced them to agree. This is
//! the fourth instance of one pattern in this project — two places computing a value
//! that must match, with nothing making it so — after config paths, the prompt hash,
//! and the prompt's legal-field list. Each was fixed the same way: give the value one
//! owner and make the others ask.
//!
//! Lives in `litrpg-core` because the watch firmware resolves these names too, so the
//! convention has three consumers across two languages' worth of build targets.

use alloc::format;
use alloc::string::String;

/// Minimum digits in a chapter's zero-padded stem.
///
/// A **minimum, not a width**: chapter 12345 is `12345`, not `2345`. Truncating would
/// make two chapters share a filename, and the failure would land 12,000 chapters in —
/// long after anyone would think to check.
pub const CHAPTER_DIGITS: usize = 4;

/// The zero-padded stem for a chapter: `1` → `0001`, `12345` → `12345`.
pub fn chapter_stem(chapter: u32) -> String {
    format!("{chapter:0width$}", width = CHAPTER_DIGITS)
}

/// A chapter artifact's filename: `media_name(1, "mp3")` → `0001.mp3`.
///
/// The extension is passed rather than enumerated so this stays useful for the `.md`
/// and `.json` artifacts, which have no path columns and never did.
pub fn media_name(chapter: u32, ext: &str) -> String {
    format!("{}.{}", chapter_stem(chapter), ext)
}

/// `NNNN.pcm` — raw 16 kHz mono s16le, streamed to the watch, pruned outside the
/// buffer window.
pub fn pcm_name(chapter: u32) -> String {
    media_name(chapter, "pcm")
}

/// `NNNN.mp3` — the permanent archive, and what the podcast feed and Candela fetch.
pub fn mp3_name(chapter: u32) -> String {
    media_name(chapter, "mp3")
}

/// `NNNN.md` — the canonical chapter text.
pub fn text_name(chapter: u32) -> String {
    media_name(chapter, "md")
}

/// `NNNN.json` — the manifest driving Range requests and sentence highlighting.
pub fn manifest_name(chapter: u32) -> String {
    media_name(chapter, "json")
}
