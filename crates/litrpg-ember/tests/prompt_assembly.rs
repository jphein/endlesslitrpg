//! Prompt assembly is pure, so every rule in spec §6.3 is a plain unit test.

use litrpg_core::{LedgerEntry, Op, fold};
use litrpg_ember::prompt::{
    ChapterSummary, DEFAULT_TARGET_WORDS, LoreEntry, Pass1Input, match_lore, pass1_messages,
    pass2_messages, render_state_snapshot,
};

fn lore(name: &str, keywords: &str, priority: i32, always_on: bool) -> LoreEntry {
    LoreEntry {
        name: name.to_string(),
        kind: "place".to_string(),
        keywords: keywords.to_string(),
        body_md: format!("Body of {name}."),
        priority,
        always_on,
    }
}

fn entry(
    seq: u64,
    subject: &str,
    field: &str,
    op: Op,
    num: Option<i64>,
    txt: Option<&str>,
) -> LedgerEntry {
    LedgerEntry {
        seq,
        chapter: 1,
        subject: subject.to_string(),
        field: field.to_string(),
        op,
        value_num: num,
        value_txt: txt.map(str::to_string),
        applied: true,
    }
}

// ---------------------------------------------------------------------------
// Lorebook keyword matching (spec §6.3)
// ---------------------------------------------------------------------------

#[test]
fn an_entry_matches_when_any_keyword_appears_in_the_scan_text() {
    let entries = vec![lore("Ashen Vale", "ashen vale,the vale", 0, false)];
    let hits = match_lore(&entries, "They rode into the Vale before dawn.");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Ashen Vale");
}

#[test]
fn keyword_matching_is_case_insensitive_in_both_directions() {
    let entries = vec![lore("Kaelen", "KAELEN", 0, false)];
    assert_eq!(match_lore(&entries, "kaelen walked").len(), 1);

    let entries = vec![lore("Kaelen", "kaelen", 0, false)];
    assert_eq!(match_lore(&entries, "KAELEN WALKED").len(), 1);
}

#[test]
fn non_matching_entries_are_left_out() {
    let entries = vec![lore("Sunspire", "sunspire", 0, false)];
    assert!(match_lore(&entries, "The vale was quiet.").is_empty());
}

#[test]
fn always_on_entries_match_regardless_of_the_scan_text() {
    let entries = vec![lore("World Rules", "nothing-here", 0, true)];
    let hits = match_lore(&entries, "");
    assert_eq!(hits.len(), 1, "always_on must not depend on keywords");
}

#[test]
fn results_are_ordered_by_descending_priority() {
    let entries = vec![
        lore("Low", "vale", 1, false),
        lore("High", "vale", 100, false),
        lore("Mid", "vale", 50, false),
    ];
    let names: Vec<&str> = match_lore(&entries, "the vale")
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, vec!["High", "Mid", "Low"]);
}

#[test]
fn equal_priorities_break_ties_by_name_so_prompts_are_reproducible() {
    let entries = vec![
        lore("Zeta", "vale", 5, false),
        lore("Alpha", "vale", 5, false),
    ];
    let names: Vec<&str> = match_lore(&entries, "vale")
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Alpha", "Zeta"],
        "an unstable order would change prompt_hash for identical state"
    );
}

#[test]
fn an_entry_is_never_returned_twice_even_if_several_keywords_hit() {
    let entries = vec![lore("Ashen Vale", "vale,ash,ashen", 0, false)];
    let hits = match_lore(&entries, "the ashen vale, full of ash");
    assert_eq!(hits.len(), 1);
}

#[test]
fn an_always_on_entry_that_also_matches_appears_once() {
    let entries = vec![lore("World Rules", "vale", 0, true)];
    assert_eq!(match_lore(&entries, "the vale").len(), 1);
}

#[test]
fn blank_and_whitespace_only_keywords_are_ignored() {
    // A trailing comma must not produce an empty keyword that matches everything.
    let entries = vec![lore("Sunspire", "sunspire, ,", 0, false)];
    assert!(
        match_lore(&entries, "The vale was quiet.").is_empty(),
        "an empty keyword would make every entry match every chapter"
    );
}

#[test]
fn an_entry_with_no_keywords_and_no_always_on_never_matches() {
    let entries = vec![lore("Orphan", "", 0, false)];
    assert!(match_lore(&entries, "anything at all").is_empty());
}

// ---------------------------------------------------------------------------
// Word-bounded matching. Plain substring containment injects lore about the wrong
// entity and burns context doing it, and the failure is invisible -- no error, just
// a chapter that quietly drifts off-canon.
// ---------------------------------------------------------------------------

fn matches(keywords: &str, scan: &str) -> bool {
    let entries = vec![lore("Probe", keywords, 0, false)];
    !match_lore(&entries, scan).is_empty()
}

#[test]
fn a_keyword_matches_when_bounded_by_non_alphanumerics_or_string_edges() {
    assert!(matches("ash", "ash"), "the whole scan text is the keyword");
    assert!(matches("ash", "the ash fell"), "bounded by spaces");
    assert!(matches("ash", "Ash."), "a full stop is a boundary");
    assert!(matches("ash", "ash,"), "a comma is a boundary");
    assert!(matches("ash", "wet ash"), "boundary at the string edge");
    assert!(matches("ash", "(ash)"), "brackets are boundaries");
    assert!(matches("ash", "iron-ash"), "a hyphen is a boundary");
    assert!(matches("ash", "\"ash\""), "quotes are boundaries");
    assert!(matches("ash", "ash\nfell"), "a newline is a boundary");
}

#[test]
fn a_keyword_does_not_fire_on_a_longer_word_that_contains_it() {
    assert!(!matches("ash", "cash"), "prefix must not match");
    assert!(!matches("ash", "ashen"), "suffix must not match");
    assert!(!matches("ash", "cashew"), "infix must not match");
    assert!(!matches("ash", "Ashen Vale"), "ashen is not ash");
    assert!(!matches("vale", "valentine"), "vale is not valentine");
}

#[test]
fn a_bounded_occurrence_is_found_even_after_an_unbounded_one() {
    // "ashen" comes first and must not short-circuit the scan for the real "ash".
    assert!(
        matches("ash", "the ashen vale, and then ash"),
        "every occurrence must be checked, not just the first"
    );
    assert!(matches("ash", "cash ash"));
}

#[test]
fn multi_word_keywords_match_as_a_whole_phrase() {
    assert!(matches(
        "ashen vale",
        "They rode into the ashen vale, at dusk."
    ));
    assert!(matches("ashen vale", "The Ashen Vale"), "case-insensitive");
    assert!(
        !matches("ashen vale", "the ashen valence"),
        "the phrase's trailing boundary applies to the phrase, not to its first word"
    );
    assert!(
        !matches("ashen vale", "ashen  vale"),
        "internal spacing must match the keyword literally"
    );
}

#[test]
fn boundary_matching_is_unicode_aware() {
    assert!(matches("kaelén", "Kaelén drew his blade."));
    assert!(
        !matches("kael", "Kaelén"),
        "an accented letter is a word character, not punctuation"
    );
}

#[test]
fn pass1_tells_the_model_to_repeat_a_tag_after_a_blank_line() {
    // The parser resets to `narrator` on a blank line, which is what stops narration
    // after a [SYSTEM] block being read in the robotic voice. The cost -- a speaker's
    // block split by a blank line -- is recovered here, in the prompt.
    let text = pass1_messages(&sample_input(&[], &[], &[]))
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    assert!(
        text.contains("repeat the tag"),
        "the model must be told to repeat the tag when a speaker continues past a blank line"
    );
    assert!(text.contains("blank line"));
}

// ---------------------------------------------------------------------------
// State snapshot rendering
// ---------------------------------------------------------------------------

#[test]
fn state_snapshot_renders_grouped_by_subject_with_all_field_families() {
    let snap = fold(&[
        entry(1, "Kaelen", "level", Op::Set, Some(7), None),
        entry(2, "Kaelen", "hp", Op::Set, Some(41), None),
        entry(3, "Kaelen", "max_hp", Op::Set, Some(60), None),
        entry(4, "Kaelen", "location", Op::Set, None, Some("Ashen Vale")),
        entry(5, "Kaelen", "inv:ashen seal", Op::Set, Some(1), None),
        entry(
            6,
            "Kaelen",
            "equip:main_hand",
            Op::Set,
            None,
            Some("Blade of Unpaid Debts"),
        ),
        entry(7, "Kaelen", "appear:eyes", Op::Set, None, Some("grey")),
        entry(8, "Sera", "level", Op::Set, Some(5), None),
    ]);

    let rendered = render_state_snapshot(&snap);

    assert!(rendered.contains("Kaelen"), "subject heading missing");
    assert!(rendered.contains("Sera"), "second subject missing");
    for fragment in [
        "level: 7",
        "hp: 41",
        "max_hp: 60",
        "location: Ashen Vale",
        "ashen seal",
        "main_hand",
        "Blade of Unpaid Debts",
        "eyes",
        "grey",
    ] {
        assert!(
            rendered.contains(fragment),
            "rendered snapshot lost {fragment:?}:\n{rendered}"
        );
    }
    // Kaelen must come before Sera, and each subject appears once.
    let k = rendered.find("Kaelen").unwrap();
    let s = rendered.find("Sera").unwrap();
    assert!(k < s, "subjects should render in a stable order");
}

#[test]
fn an_empty_snapshot_renders_something_the_model_can_read() {
    let rendered = render_state_snapshot(&fold(&[]));
    assert!(
        !rendered.trim().is_empty(),
        "chapter 1 has an empty ledger; the prompt must still say so explicitly \
         rather than leaving a blank section the model will invent numbers for"
    );
}

// ---------------------------------------------------------------------------
// Pass 1
// ---------------------------------------------------------------------------

fn pass1_text(input: &Pass1Input) -> String {
    pass1_messages(input)
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn sample_input<'a>(
    lore_hits: &'a [&'a LoreEntry],
    summaries: &'a [ChapterSummary],
    notes: &'a [String],
) -> Pass1Input<'a> {
    Pass1Input {
        chapter_number: 42,
        story_prompt: "A debt-collector for a dead god.",
        arc_outline: "Arc 2: the three seals.",
        state_snapshot: "Kaelen\n  level: 7\n  hp: 41/60",
        lore: lore_hits,
        recent_summaries: summaries,
        director_notes: notes,
        target_words: 2000,
    }
}

#[test]
fn pass1_has_a_system_message_and_a_user_message() {
    let msgs = pass1_messages(&sample_input(&[], &[], &[]));
    assert!(msgs.len() >= 2);
    assert_eq!(msgs[0].role.as_str(), "system");
    assert_eq!(msgs.last().unwrap().role.as_str(), "user");
}

#[test]
fn pass1_carries_every_input_it_was_given() {
    let hits = [lore("Ashen Vale", "vale", 10, false)];
    let hit_refs: Vec<&LoreEntry> = hits.iter().collect();
    let summaries = [ChapterSummary {
        chapter: 41,
        body_md: "Kaelen reached the second seal.".into(),
    }];
    let notes = ["introduce a rival".to_string()];

    let text = pass1_text(&sample_input(&hit_refs, &summaries, &notes));

    for fragment in [
        "A debt-collector for a dead god.",
        "Arc 2: the three seals.",
        "level: 7",
        "Ashen Vale",
        "Body of Ashen Vale.",
        "Kaelen reached the second seal.",
        "introduce a rival",
        "2000",
    ] {
        assert!(text.contains(fragment), "pass 1 prompt lost {fragment:?}");
    }
}

#[test]
fn pass1_teaches_the_exact_tag_format_the_parser_accepts() {
    let text = pass1_text(&sample_input(&[], &[], &[]));
    assert!(
        text.contains("[narrator]"),
        "the narrator tag must be shown"
    );
    assert!(text.contains("[SYSTEM]"), "the SYSTEM tag must be shown");
    assert!(
        text.contains("start of the line") || text.contains("beginning of the line"),
        "the model must be told a tag only counts at line start"
    );
}

/// Measured 2026-07-29: a live chapter came back with **no `[SYSTEM]` block at all**, so
/// pass 2 correctly proposed zero deltas and the ledger could never advance. A LitRPG with
/// no RPG layer looks like a working pipeline — every stage reports success — while the
/// numbers the whole design exists to keep consistent simply never appear.
#[test]
fn pass1_requires_at_least_one_system_block() {
    let text = pass1_text(&sample_input(&[], &[], &[]));
    assert!(
        text.contains("at least one [SYSTEM] block"),
        "the model must be required to emit an RPG readout, or state never changes"
    );
    assert!(
        text.contains("mana") || text.contains("stamina"),
        "naming the stats that get discarded is cheaper than a rejection: {text}"
    );
}

#[test]
fn pass1_omits_empty_sections_rather_than_shipping_dangling_headings() {
    let text = pass1_text(&sample_input(&[], &[], &[]));
    assert!(
        !text.contains("Director notes"),
        "with no notes queued, the heading should not appear at all:\n{text}"
    );
    assert!(!text.contains("Relevant lore"));
}

#[test]
fn pass1_input_has_no_field_for_verbatim_previous_chapters() {
    // Spec §6.3: raw prose in context makes the model mimic its own cadence and the
    // story collapses into a loop. Only summaries are accepted -- enforced by the
    // *type*, so no caller can pass prose even by accident. This test documents the
    // guarantee; the compiler is what enforces it.
    let summaries = [ChapterSummary {
        chapter: 41,
        body_md: "Kaelen reached the second seal.".into(),
    }];
    let text = pass1_text(&sample_input(&[], &summaries, &[]));
    assert!(
        text.contains("summar"),
        "summaries must be labelled as such"
    );
}

#[test]
fn pass1_stays_inside_the_context_budget_for_a_realistic_chapter() {
    // Spec §6.3 budgets ~6 k tokens of the ~131 k window. 4 chars/token is the usual
    // rough ratio, so ~32 k chars is a generous ceiling that still catches a runaway.
    let hits: Vec<LoreEntry> = (0..20)
        .map(|i| {
            let mut e = lore(&format!("Entry {i}"), "vale", i, false);
            e.body_md = "x".repeat(400);
            e
        })
        .collect();
    let hit_refs: Vec<&LoreEntry> = hits.iter().collect();
    let summaries: Vec<ChapterSummary> = (37..42)
        .map(|c| ChapterSummary {
            chapter: c,
            body_md: "y".repeat(600),
        })
        .collect();

    let text = pass1_text(&sample_input(&hit_refs, &summaries, &[]));
    assert!(
        text.len() < 32_000,
        "assembled prompt is {} chars, over the §6.3 budget",
        text.len()
    );
}

#[test]
fn the_default_target_word_count_matches_the_spec() {
    assert_eq!(DEFAULT_TARGET_WORDS, 2000, "spec §6.0");
}

// ---------------------------------------------------------------------------
// Pass 2
// ---------------------------------------------------------------------------

#[test]
fn pass2_includes_the_chapter_text_and_the_known_subjects() {
    let subjects = ["Kaelen".to_string(), "Sera".to_string()];
    let text = pass2_messages("Kaelen broke the first seal.", &subjects)
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("Kaelen broke the first seal."));
    assert!(text.contains("Sera"));
}

#[test]
fn pass2_lists_the_whitelists_straight_from_litrpg_core() {
    // Building the prompt from the gate's own constants is what stops the prompt and
    // the validator drifting apart (spec §6.2 / §9.4.1).
    let text = pass2_messages("body", &[])
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");

    for field in litrpg_core::validate::NUMERIC_FIELDS {
        assert!(
            text.contains(field),
            "numeric field {field} not offered to the model"
        );
    }
    for field in litrpg_core::validate::TEXT_FIELDS {
        assert!(
            text.contains(field),
            "text field {field} not offered to the model"
        );
    }
    for slot in litrpg_core::validate::EQUIP_SLOTS {
        assert!(
            text.contains(slot),
            "equip slot {slot} not offered to the model"
        );
    }
    for tr in litrpg_core::validate::APPEAR_TRAITS {
        assert!(
            text.contains(tr),
            "appearance trait {tr} not offered to the model"
        );
    }
}

#[test]
fn pass2_warns_against_inventing_subjects() {
    let text = pass2_messages("body", &["Kaelen".to_string()])
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    assert!(
        text.contains("invent") || text.contains("only") || text.contains("exactly"),
        "an invented subject is rejected by the gate, so say so in the prompt"
    );
}
