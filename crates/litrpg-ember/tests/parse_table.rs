//! Table-driven fixtures for the tagged-prose parser (spec §11).
//!
//! The load-bearing invariant across every case: **no input text is ever silently
//! discarded**. A malformed tag degrades to prose; it never drops a line.

use litrpg_core::SpeakerKind;
use litrpg_ember::parse::{ParsedSegment, classify_speaker, parse_tagged_prose};

/// Compact assertion helper: (speaker, kind, text).
fn shape(segs: &[ParsedSegment]) -> Vec<(&str, SpeakerKind, &str)> {
    segs.iter()
        .map(|s| (s.speaker.as_str(), s.kind, s.text.as_str()))
        .collect()
}

/// Every segment index is sequential from zero, with no gaps.
fn assert_indices_dense(segs: &[ParsedSegment]) {
    for (want, seg) in segs.iter().enumerate() {
        assert_eq!(
            seg.idx,
            want as u32,
            "segment indices must be dense and zero-based, got {:?}",
            segs.iter().map(|s| s.idx).collect::<Vec<_>>()
        );
    }
}

#[test]
fn parses_the_canonical_three_line_example() {
    let raw = "\
[narrator] The vale smelled of iron and wet ash.
[Kaelen]   \"You brought a sword to a debt collection?\"
[SYSTEM]   Quest updated — The Ashen Ledger: 1 of 3 seals broken.";

    let segs = parse_tagged_prose(raw);
    assert_indices_dense(&segs);
    assert_eq!(
        shape(&segs),
        vec![
            (
                "narrator",
                SpeakerKind::Narrator,
                "The vale smelled of iron and wet ash."
            ),
            (
                "Kaelen",
                SpeakerKind::Character,
                "\"You brought a sword to a debt collection?\""
            ),
            (
                "SYSTEM",
                SpeakerKind::System,
                "Quest updated — The Ashen Ledger: 1 of 3 seals broken."
            ),
        ]
    );
}

#[test]
fn untagged_leading_prose_becomes_narrator() {
    let raw = "\
The vale smelled of iron and wet ash.
[Kaelen] \"You're late.\"";

    let segs = parse_tagged_prose(raw);
    assert_eq!(
        shape(&segs),
        vec![
            (
                "narrator",
                SpeakerKind::Narrator,
                "The vale smelled of iron and wet ash."
            ),
            ("Kaelen", SpeakerKind::Character, "\"You're late.\""),
        ]
    );
}

#[test]
fn wholly_untagged_input_is_one_narrator_segment() {
    let raw = "First paragraph.\n\nSecond paragraph.";
    let segs = parse_tagged_prose(raw);
    assert_eq!(
        shape(&segs),
        vec![(
            "narrator",
            SpeakerKind::Narrator,
            "First paragraph. Second paragraph."
        )]
    );
}

#[test]
fn system_tag_is_case_insensitive_and_canonicalized() {
    for tag in ["[SYSTEM]", "[system]", "[System]", "[ sYsTeM ]"] {
        let segs = parse_tagged_prose(&format!("{tag} You have leveled up."));
        assert_eq!(
            shape(&segs),
            vec![("SYSTEM", SpeakerKind::System, "You have leveled up.")],
            "tag {tag} should canonicalize to SYSTEM/System kind"
        );
    }
}

#[test]
fn narrator_tag_is_case_insensitive_and_canonicalized() {
    for tag in ["[narrator]", "[Narrator]", "[NARRATOR]"] {
        let segs = parse_tagged_prose(&format!("{tag} Ash fell like slow snow."));
        assert_eq!(
            shape(&segs),
            vec![(
                "narrator",
                SpeakerKind::Narrator,
                "Ash fell like slow snow."
            )],
            "tag {tag} should canonicalize to narrator/Narrator kind"
        );
    }
}

/// Measured against the live model 2026-07-29: asked for `[narrator]`, Ember emitted
/// `[narration]` for most of the chapter and `[narrator]` for the opening paragraph. Without
/// this alias, `narration` is classified as a **Character**, mints a cast row, and draws a
/// character voice — so the same chapter's narration is read by two different people, one of
/// whom sounds like a cast member. Only audible, so it would ship.
#[test]
fn narration_is_an_alias_for_narrator() {
    for tag in ["[narration]", "[Narration]", "[NARRATION]"] {
        let segs = parse_tagged_prose(&format!("{tag} Ash fell like slow snow."));
        assert_eq!(
            shape(&segs),
            vec![(
                "narrator",
                SpeakerKind::Narrator,
                "Ash fell like slow snow."
            )],
            "tag {tag} must canonicalize to the narrator, not mint a character"
        );
    }
}

#[test]
fn narrator_and_narration_merge_into_one_speaker() {
    let raw = "[narrator] The vale was quiet.\n[narration] Nothing moved.";
    let segs = parse_tagged_prose(raw);
    assert_eq!(
        segs.len(),
        1,
        "the two spellings are one speaker and must merge: {segs:?}"
    );
    assert_eq!(segs[0].speaker, "narrator");
}

#[test]
fn anything_else_is_a_character() {
    let segs = parse_tagged_prose("[Sera Vane] \"Hold the line.\"");
    assert_eq!(
        shape(&segs),
        vec![("Sera Vane", SpeakerKind::Character, "\"Hold the line.\"")]
    );
}

#[test]
fn consecutive_same_speaker_lines_merge_into_one_segment() {
    let raw = "\
[narrator] The vale smelled of iron.
[narrator] Somewhere below, a bell rang once.
[Kaelen] \"That's the first seal.\"
[Kaelen] \"Two to go.\"";

    let segs = parse_tagged_prose(raw);
    assert_indices_dense(&segs);
    assert_eq!(
        shape(&segs),
        vec![
            (
                "narrator",
                SpeakerKind::Narrator,
                "The vale smelled of iron. Somewhere below, a bell rang once."
            ),
            (
                "Kaelen",
                SpeakerKind::Character,
                "\"That's the first seal.\" \"Two to go.\""
            ),
        ]
    );
}

#[test]
fn merging_is_case_insensitive_and_keeps_the_first_spelling() {
    let raw = "[Kaelen] \"One.\"\n[KAELEN] \"Two.\"";
    let segs = parse_tagged_prose(raw);
    assert_eq!(
        shape(&segs),
        vec![("Kaelen", SpeakerKind::Character, "\"One.\" \"Two.\"")],
        "case-variant spellings must not mint a second cast member"
    );
}

#[test]
fn blank_lines_never_create_empty_segments() {
    let raw = "\n\n[narrator] Ash.\n\n   \n\n[Kaelen] \"Debt.\"\n\n\n";
    let segs = parse_tagged_prose(raw);
    assert_indices_dense(&segs);
    assert_eq!(
        shape(&segs),
        vec![
            ("narrator", SpeakerKind::Narrator, "Ash."),
            ("Kaelen", SpeakerKind::Character, "\"Debt.\""),
        ]
    );
    assert!(
        segs.iter().all(|s| !s.text.trim().is_empty()),
        "no segment may carry empty text"
    );
}

#[test]
fn blank_line_between_same_speaker_lines_still_merges() {
    let raw = "[narrator] One.\n\n[narrator] Two.";
    let segs = parse_tagged_prose(raw);
    assert_eq!(segs.len(), 1, "a blank line is not a speaker change");
    assert_eq!(segs[0].text, "One. Two.");
}

#[test]
fn empty_input_yields_no_segments() {
    assert!(parse_tagged_prose("").is_empty());
    assert!(parse_tagged_prose("   \n\n \t \n").is_empty());
}

// ---------------------------------------------------------------------------
// Malformed tags: degrade to prose, NEVER drop the text.
// ---------------------------------------------------------------------------

#[test]
fn unclosed_tag_is_treated_as_prose() {
    let segs = parse_tagged_prose("[unclosed the vale went quiet");
    assert_eq!(
        shape(&segs),
        vec![(
            "narrator",
            SpeakerKind::Narrator,
            "[unclosed the vale went quiet"
        )],
        "an unclosed tag must survive as prose, brackets and all"
    );
}

#[test]
fn backwards_brackets_are_treated_as_prose() {
    let segs = parse_tagged_prose("]backwards[ he said");
    assert_eq!(
        shape(&segs),
        vec![("narrator", SpeakerKind::Narrator, "]backwards[ he said")]
    );
}

#[test]
fn empty_and_whitespace_tags_are_treated_as_prose() {
    for line in ["[] the vale went quiet", "[   ] the vale went quiet"] {
        let segs = parse_tagged_prose(line);
        assert_eq!(
            shape(&segs),
            vec![("narrator", SpeakerKind::Narrator, line)],
            "line {line:?} must survive verbatim as prose"
        );
    }
}

#[test]
fn nested_open_bracket_inside_a_tag_is_prose() {
    let line = "[Kae[len] \"broken tag\"";
    let segs = parse_tagged_prose(line);
    assert_eq!(
        shape(&segs),
        vec![("narrator", SpeakerKind::Narrator, line)]
    );
}

#[test]
fn malformed_line_after_a_tagged_line_is_not_dropped() {
    let raw = "[Kaelen] \"You're late.\"\n[unclosed still talking";
    let segs = parse_tagged_prose(raw);
    let joined: String = segs
        .iter()
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("[unclosed still talking"),
        "malformed text must appear somewhere in the output, got {joined:?}"
    );
}

// ---------------------------------------------------------------------------
// Brackets and quotes that are *content*, not structure.
// ---------------------------------------------------------------------------

#[test]
fn only_a_tag_at_line_start_counts() {
    let line = "The sign read [Ashen Vale] in flaking chalk.";
    let segs = parse_tagged_prose(line);
    assert_eq!(
        shape(&segs),
        vec![("narrator", SpeakerKind::Narrator, line)],
        "a mid-line bracket group is prose, not a speaker change"
    );
}

#[test]
fn brackets_mid_line_after_a_real_tag_are_preserved() {
    let segs = parse_tagged_prose("[SYSTEM] Loot acquired: [Ashen Seal] x1 [rare]");
    assert_eq!(
        shape(&segs),
        vec![(
            "SYSTEM",
            SpeakerKind::System,
            "Loot acquired: [Ashen Seal] x1 [rare]"
        )]
    );
}

#[test]
fn nested_quotes_survive_intact() {
    let text = "\"She called it 'the ledger', and then she laughed.\"";
    let segs = parse_tagged_prose(&format!("[Sera] {text}"));
    assert_eq!(shape(&segs), vec![("Sera", SpeakerKind::Character, text)]);
}

#[test]
fn leading_whitespace_before_a_tag_is_tolerated() {
    let segs = parse_tagged_prose("    [Kaelen] \"Indented.\"");
    assert_eq!(
        shape(&segs),
        vec![("Kaelen", SpeakerKind::Character, "\"Indented.\"")]
    );
}

#[test]
fn a_colon_after_the_tag_is_not_part_of_the_speech() {
    for line in ["[Kaelen]: \"Late again.\"", "[Kaelen:] \"Late again.\""] {
        let segs = parse_tagged_prose(line);
        assert_eq!(
            shape(&segs),
            vec![("Kaelen", SpeakerKind::Character, "\"Late again.\"")],
            "line {line:?} should strip the stray colon"
        );
    }
}

#[test]
fn a_bare_tag_line_changes_speaker_without_emitting_an_empty_segment() {
    let raw = "[Kaelen]\n\"So this is the vale.\"";
    let segs = parse_tagged_prose(raw);
    assert_eq!(
        shape(&segs),
        vec![("Kaelen", SpeakerKind::Character, "\"So this is the vale.\"")],
        "a tag alone on a line sets the speaker for what follows"
    );
}

#[test]
fn untagged_continuation_sticks_to_the_current_speaker() {
    let raw = "[Kaelen] \"You brought a sword to a debt collection?\"\nBecause I brought a ledger.";
    let segs = parse_tagged_prose(raw);
    assert_eq!(
        segs.len(),
        1,
        "a wrapped continuation line belongs to the speaker that was talking"
    );
    assert_eq!(segs[0].speaker, "Kaelen");
    assert!(segs[0].text.contains("Because I brought a ledger."));
}

// ---------------------------------------------------------------------------
// Block form. This is what Ember *actually* emits (measured against
// familiar:8091, 2026-07-29): the tag sits alone on its line, its content
// follows on the next lines, and a blank line ends the block. Untagged
// paragraphs between blocks are narration.
//
// A blank line therefore resets the speaker to `narrator`. Without that rule,
// the narration paragraph after a [SYSTEM] block would be read aloud in the
// robotic SYSTEM voice.
// ---------------------------------------------------------------------------

#[test]
fn a_blank_line_ends_a_block_and_reverts_to_narrator() {
    let raw = "\
[SYSTEM]
Quest Updated: The Ashen Covenant
Health: 100/100

Kaelen adjusted the strap of his gauntlet, and the leather creaked.";

    let segs = parse_tagged_prose(raw);
    assert_eq!(
        shape(&segs),
        vec![
            (
                "SYSTEM",
                SpeakerKind::System,
                "Quest Updated: The Ashen Covenant Health: 100/100"
            ),
            (
                "narrator",
                SpeakerKind::Narrator,
                "Kaelen adjusted the strap of his gauntlet, and the leather creaked."
            ),
        ],
        "narration after a SYSTEM block must not inherit the SYSTEM voice"
    );
}

#[test]
fn parses_embers_real_block_output_shape() {
    // Verbatim shape captured from familiar:8091 at temp 0.9.
    let raw = "\
[SYSTEM]
Quest Updated: The Ashen Covenant
Seal integrity at 98%.

Kaelen adjusted the strap of his gauntlet, the leather creaking softly.

[Sera]
You're late, Kaelen. The dust is already settling on the hinges.

Kaelen didn't look back. He knew Sera was waiting by the altar.

[Kaelen]
Traffic was hell. I had to break their contract.";

    let segs = parse_tagged_prose(raw);
    assert_indices_dense(&segs);
    assert_eq!(
        segs.iter()
            .map(|s| (s.speaker.as_str(), s.kind))
            .collect::<Vec<_>>(),
        vec![
            ("SYSTEM", SpeakerKind::System),
            ("narrator", SpeakerKind::Narrator),
            ("Sera", SpeakerKind::Character),
            ("narrator", SpeakerKind::Narrator),
            ("Kaelen", SpeakerKind::Character),
        ]
    );
    assert!(segs[0].text.contains("Seal integrity at 98%."));
    assert!(segs[2].text.starts_with("You're late, Kaelen."));
}

#[test]
fn a_multi_line_block_merges_without_losing_lines() {
    let raw = "[SYSTEM]\nLevel: 7\nHP: 41/60\nGold: 12";
    let segs = parse_tagged_prose(raw);
    assert_eq!(segs.len(), 1);
    for fragment in ["Level: 7", "HP: 41/60", "Gold: 12"] {
        assert!(
            segs[0].text.contains(fragment),
            "lost {fragment:?} from a multi-line SYSTEM block"
        );
    }
}

// ---------------------------------------------------------------------------
// The no-loss invariant, stated directly.
// ---------------------------------------------------------------------------

#[test]
fn no_visible_text_is_ever_discarded() {
    let raw = "\
Untagged opener.
[narrator] Tagged narration.
[Kaelen] \"Dialogue with 'nested' quotes.\"
[unclosed malformed line
[] empty tag line
]backwards[
[SYSTEM] HP 41/60 — [Bleeding]

The sign read [Ashen Vale] in chalk.";

    let segs = parse_tagged_prose(raw);
    let joined: String = segs
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    // Every non-tag fragment of the source must be present in the output.
    for fragment in [
        "Untagged opener.",
        "Tagged narration.",
        "\"Dialogue with 'nested' quotes.\"",
        "[unclosed malformed line",
        "[] empty tag line",
        "]backwards[",
        "HP 41/60 — [Bleeding]",
        "The sign read [Ashen Vale] in chalk.",
    ] {
        assert!(
            joined.contains(fragment),
            "lost story content {fragment:?}\nfrom output {joined:?}"
        );
    }
}

#[test]
fn crlf_line_endings_parse_the_same_as_lf() {
    let lf = parse_tagged_prose("[narrator] One.\n[Kaelen] \"Two.\"");
    let crlf = parse_tagged_prose("[narrator] One.\r\n[Kaelen] \"Two.\"");
    assert_eq!(shape(&lf), shape(&crlf));
}

#[test]
fn classify_speaker_is_exposed_for_cast_assignment() {
    assert_eq!(classify_speaker("narrator"), SpeakerKind::Narrator);
    assert_eq!(classify_speaker("NARRATOR"), SpeakerKind::Narrator);
    assert_eq!(classify_speaker("SYSTEM"), SpeakerKind::System);
    assert_eq!(classify_speaker("system"), SpeakerKind::System);
    assert_eq!(classify_speaker("Kaelen"), SpeakerKind::Character);
    assert_eq!(classify_speaker("Sera Vane"), SpeakerKind::Character);
}
