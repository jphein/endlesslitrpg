//! The tagged-prose parser: pass-1 output → segments.
//!
//! Pass 1 emits speaker-tagged prose. The canonical form is one tag per line:
//!
//! ```text
//! [narrator] The vale smelled of iron and wet ash.
//! [Kaelen]   "You brought a sword to a debt collection?"
//! [SYSTEM]   Quest updated — The Ashen Ledger: 1 of 3 seals broken.
//! ```
//!
//! **What Ember actually emits** (measured against `familiar:8091`, 2026-07-29) is the
//! *block* form: the tag sits alone on its line, its content follows on the next lines,
//! a blank line ends the block, and untagged paragraphs between blocks are narration.
//! Both forms parse here, because the model's real habits are not negotiable and the
//! parser is the cheaper place to be flexible.
//!
//! # The rules, in full
//!
//! 1. A tag counts **only at the start of a line**: `[` … `]` with a non-empty name.
//!    `The sign read [Ashen Vale] in chalk.` is prose.
//! 2. A tag with text after it emits that text. A tag alone on its line just changes
//!    speaker — it never emits an empty segment.
//! 3. An untagged line belongs to the **current** speaker, which starts as `narrator`.
//! 4. A **blank line ends the block** and resets the speaker to `narrator`. This is the
//!    rule that stops the narration paragraph after a `[SYSTEM]` block being read aloud
//!    in the robotic SYSTEM voice.
//! 5. `SYSTEM` and `narrator` are recognised case-insensitively and canonicalised, so
//!    `[system]` and `[SYSTEM]` do not mint two cast rows. Anything else is a character.
//! 6. Consecutive same-speaker text merges into one segment; comparison is
//!    case-insensitive and the first spelling wins.
//! 7. A **malformed tag is prose** — `[unclosed`, `]backwards[`, `[]`, `[Kae[len]`.
//!    Text is never dropped, because dropping it would lose story content silently.
//!
//! Rule 7 is the invariant worth defending: a parser that discards what it cannot
//! understand fails invisibly, and the failure only surfaces as a chapter that does not
//! quite make sense — months later, unattributable.
//!
//! [`ParsedSegment`] is deliberately **not** [`litrpg_core::Segment`]: that type carries
//! `voice_ref`, `start_ms` and `end_ms`, none of which exist until cast assignment and
//! rendering have happened. The engine converts once it knows them.

use litrpg_core::SpeakerKind;

/// Canonical narrator speaker name.
pub const NARRATOR: &str = "narrator";

/// Canonical SYSTEM speaker name (spec §6.0: `cast.kind` = `system`).
pub const SYSTEM: &str = "SYSTEM";

/// One attributed run of text, before a voice or any timing exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSegment {
    /// Dense and zero-based, so the manifest can be built straight from these.
    pub idx: u32,
    /// Canonicalised speaker name; the key the `cast` table is looked up by.
    pub speaker: String,
    pub kind: SpeakerKind,
    pub text: String,
}

/// Which voice family a speaker name belongs to. Case-insensitive.
pub fn classify_speaker(name: &str) -> SpeakerKind {
    let n = name.trim();
    if n.eq_ignore_ascii_case("system") {
        SpeakerKind::System
    } else if n.eq_ignore_ascii_case("narrator") {
        SpeakerKind::Narrator
    } else {
        SpeakerKind::Character
    }
}

/// Collapse a raw tag name to the form stored in `cast.speaker`.
///
/// Internal whitespace is collapsed and the two reserved names are pinned to one
/// spelling each, so `[ sYsTeM ]` and `[SYSTEM]` are the same cast member rather than
/// two rows drawing two different voices.
pub fn canonical_speaker(name: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    match classify_speaker(&collapsed) {
        SpeakerKind::System => SYSTEM.to_string(),
        SpeakerKind::Narrator => NARRATOR.to_string(),
        SpeakerKind::Character => collapsed,
    }
}

/// Split a line into `(speaker, remaining text)` if — and only if — it opens with a
/// well-formed tag. Returns `None` for prose, including every malformed-tag case.
pub fn split_tag(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let inner_start = line.strip_prefix('[')?;
    let close = inner_start.find(']')?;
    let inner = &inner_start[..close];

    // A `[` inside the tag means the brackets are prose, not structure.
    if inner.contains('[') {
        return None;
    }

    // Tolerate `[Kaelen:]` as well as `[Kaelen]:`.
    let name = inner
        .trim()
        .strip_suffix(':')
        .unwrap_or(inner.trim())
        .trim();
    if name.is_empty() {
        return None;
    }

    let rest = inner_start[close + 1..].trim();
    let rest = rest.strip_prefix(':').unwrap_or(rest).trim();

    Some((name, rest))
}

/// Parse pass-1 output into attributed segments. Total: every input produces a valid
/// (possibly empty) result and no input text is discarded.
pub fn parse_tagged_prose(raw: &str) -> Vec<ParsedSegment> {
    let mut out: Vec<ParsedSegment> = Vec::new();
    let mut current = NARRATOR.to_string();

    for line in raw.lines() {
        let line = line.trim();

        if line.is_empty() {
            // End of block. Untagged prose after this is narration again.
            current = NARRATOR.to_string();
            continue;
        }

        match split_tag(line) {
            Some((name, rest)) => {
                current = canonical_speaker(name);
                if !rest.is_empty() {
                    push_text(&mut out, &current, rest);
                }
            }
            // Prose, or a tag too malformed to trust. Either way it is story text.
            None => push_text(&mut out, &current, line),
        }
    }

    out
}

/// Append text, merging into the previous segment when the speaker is unchanged.
fn push_text(out: &mut Vec<ParsedSegment>, speaker: &str, text: &str) {
    if let Some(last) = out.last_mut()
        && last.speaker.eq_ignore_ascii_case(speaker)
    {
        last.text.push(' ');
        last.text.push_str(text);
        return;
    }

    out.push(ParsedSegment {
        idx: out.len() as u32,
        speaker: speaker.to_string(),
        kind: classify_speaker(speaker),
        text: text.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_tag_accepts_the_well_formed_cases() {
        assert_eq!(split_tag("[narrator] Ash."), Some(("narrator", "Ash.")));
        assert_eq!(
            split_tag("  [Kaelen]  \"Hi.\""),
            Some(("Kaelen", "\"Hi.\""))
        );
        assert_eq!(split_tag("[SYSTEM]"), Some(("SYSTEM", "")));
        assert_eq!(split_tag("[Sera Vane] x"), Some(("Sera Vane", "x")));
    }

    #[test]
    fn split_tag_rejects_every_malformed_case() {
        for line in [
            "[unclosed tag",
            "]backwards[",
            "[] empty",
            "[   ] blank",
            "[Kae[len] nested",
            "no tag at all",
            "The sign read [Ashen Vale] in chalk.",
            "",
        ] {
            assert_eq!(split_tag(line), None, "should be prose: {line:?}");
        }
    }

    #[test]
    fn canonical_speaker_collapses_internal_whitespace() {
        assert_eq!(canonical_speaker("Sera   Vane"), "Sera Vane");
        assert_eq!(canonical_speaker("  sYsTeM "), "SYSTEM");
        assert_eq!(canonical_speaker("NARRATOR"), "narrator");
    }
}
