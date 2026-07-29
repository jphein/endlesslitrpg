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
//! 5. `SYSTEM` and `narrator` are recognised case-insensitively, canonicalised, and matched
//!    **through a typo** — `[narration]` and `[narraraor]` are both the narrator, because the
//!    live model emits both. Anything else is a character. Getting this wrong mints a
//!    permanent `cast` row that narrates in a character's voice for the rest of the serial.
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

/// Canonical narrator speaker name. Re-exported from core so there is one spelling.
pub const NARRATOR: &str = litrpg_core::speaker::NARRATOR;

/// Canonical SYSTEM speaker name (spec §6.0: `cast.kind` = `system`). From core, one spelling.
pub const SYSTEM: &str = litrpg_core::speaker::SYSTEM;

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

/// Spellings that mean "the narrating voice".
///
/// `narration` is here because the live model uses it: asked for `[narrator]`, Ember
/// emitted `[narration]` for most of a measured chapter and `[narrator]` for the opening
/// paragraph. Treating it as a character mints a cast row and draws a *character* voice, so
/// one chapter's narration gets read by two different people — a defect that is only
/// audible, and would therefore ship.
pub const NARRATOR_ALIASES: &[&str] = &["narrator", "narration"];

/// Spellings that mean the RPG readout voice.
pub const SYSTEM_ALIASES: &[&str] = &["system"];

/// Largest edit distance still treated as a misspelling of a role tag.
const ROLE_TYPO_DISTANCE: usize = 2;

/// A candidate must share this many leading characters with a role word before its edit
/// distance is even considered. Without it, short names start colliding with role words.
const ROLE_TYPO_PREFIX: usize = 2;

/// Shortest candidate eligible for fuzzy matching. Below this, two edits is most of the word.
const ROLE_TYPO_MIN_LEN: usize = 4;

/// Which voice family a speaker name belongs to. Case-insensitive.
///
/// Exact alias matches first, then a **bounded fuzzy match** against the role words.
///
/// The fuzzy step exists because the live model mistypes them: a measured chapter contained
/// `[narraraor]`, which was cast as a character, drew a character voice, and — `cast` being
/// permanent — would have narrated the rest of the serial in it. An alias list cannot
/// enumerate typos.
///
/// This is safe *because the tag namespace is structural*, not free text: the model is
/// choosing between two role words and a character name, so a word two edits from `narrator`
/// is overwhelmingly a broken `narrator` rather than a person. The guards keep it from
/// eating real names — `Nara`, `Narran`, `Nadia`, `Syl` and `Nyx` all stay characters.
pub fn classify_speaker(name: &str) -> SpeakerKind {
    let n = name.trim();

    if SYSTEM_ALIASES.iter().any(|a| n.eq_ignore_ascii_case(a)) {
        return SpeakerKind::System;
    }
    if NARRATOR_ALIASES.iter().any(|a| n.eq_ignore_ascii_case(a)) {
        return SpeakerKind::Narrator;
    }

    // A multi-word tag is a name ("Sera Vane", "System Lord"), never a mistyped role word.
    if n.chars().any(char::is_whitespace) {
        return SpeakerKind::Character;
    }

    if SYSTEM_ALIASES.iter().any(|a| is_typo_of(n, a)) {
        return SpeakerKind::System;
    }
    if NARRATOR_ALIASES.iter().any(|a| is_typo_of(n, a)) {
        return SpeakerKind::Narrator;
    }

    SpeakerKind::Character
}

/// Whether `candidate` is a near-miss of `role`, under the guards above.
fn is_typo_of(candidate: &str, role: &str) -> bool {
    let c: Vec<char> = candidate.to_lowercase().chars().collect();
    let r: Vec<char> = role.to_lowercase().chars().collect();

    if c.len() < ROLE_TYPO_MIN_LEN {
        return false;
    }
    // A length gap wider than the edit budget cannot be closed by it, and checking here
    // stops a long name being compared against a short role word at all.
    if c.len().abs_diff(r.len()) > ROLE_TYPO_DISTANCE {
        return false;
    }
    if c.iter()
        .take(ROLE_TYPO_PREFIX)
        .ne(r.iter().take(ROLE_TYPO_PREFIX))
    {
        return false;
    }

    edit_distance(&c, &r) <= ROLE_TYPO_DISTANCE
}

/// Levenshtein distance, two rows rather than a full matrix.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        core::mem::swap(&mut prev, &mut cur);
    }

    prev[b.len()]
}

/// Collapse a raw tag name to the form stored in `cast.speaker`.
///
/// Internal whitespace is collapsed and the two reserved names are pinned to one
/// spelling each, so `[ sYsTeM ]` and `[SYSTEM]` are the same cast member rather than
/// two rows drawing two different voices.
pub fn canonical_speaker(name: &str) -> String {
    // `classify_speaker` is the lenient half and stays here: it tolerates a typo because it faces a
    // *model*, and §5.3 puts that leniency at the boundary. Once the role is decided, the spelling
    // is `litrpg_core::speaker::canonical`'s to choose — one owner for the storage form, so a
    // stored row's identity can never depend on which crate wrote it.
    match classify_speaker(name) {
        SpeakerKind::System => litrpg_core::speaker::canonical(litrpg_core::speaker::SYSTEM),
        SpeakerKind::Narrator => litrpg_core::speaker::canonical(litrpg_core::speaker::NARRATOR),
        SpeakerKind::Character => litrpg_core::speaker::canonical(name),
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
    fn edit_distance_is_correct_on_the_cases_that_matter() {
        let d = |a: &str, b: &str| {
            edit_distance(
                &a.chars().collect::<Vec<_>>(),
                &b.chars().collect::<Vec<_>>(),
            )
        };
        assert_eq!(d("narrator", "narrator"), 0);
        assert_eq!(d("narrater", "narrator"), 1);
        assert_eq!(d("narraraor", "narrator"), 2);
        assert_eq!(d("", "abc"), 3);
        assert_eq!(d("abc", ""), 3);
        assert_eq!(d("kaelen", "narrator"), 7);
    }

    #[test]
    fn the_typo_guards_hold_in_both_directions() {
        assert!(is_typo_of("narraraor", "narrator"));
        assert!(is_typo_of("NARRATER", "narrator"));
        assert!(is_typo_of("sytem", "system"));

        // Too short to risk it.
        assert!(!is_typo_of("nar", "narrator"));
        // Prefix does not match.
        assert!(!is_typo_of("marrator", "narrator"));
        // Length gap wider than the edit budget.
        assert!(!is_typo_of("narr", "narrator"));
        // Real names.
        assert!(!is_typo_of("nara", "narrator"));
        assert!(!is_typo_of("narran", "narrator"));
        assert!(!is_typo_of("syl", "system"));
    }

    #[test]
    fn canonical_speaker_collapses_internal_whitespace() {
        assert_eq!(canonical_speaker("Sera   Vane"), "Sera Vane");
        assert_eq!(canonical_speaker("  sYsTeM "), "SYSTEM");
        assert_eq!(canonical_speaker("NARRATOR"), "narrator");
    }
}
