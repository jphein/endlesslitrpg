use litrpg_cli::naming::{
    ProtagonistCheck, check_protagonist, find_whole_word, possible_aliases, warning,
};

// ---------------------------------------------------------- word boundaries

#[test]
fn a_name_inside_a_longer_word_is_not_a_match() {
    // The whole point: "Kaelen" and "Kaelendra" are different characters.
    assert_eq!(find_whole_word("Kaelendra drew her blade.", "Kaelen"), None);
    assert_eq!(find_whole_word("The Kaelenites gathered.", "Kaelen"), None);
    assert_eq!(find_whole_word("unKaelen", "Kaelen"), None);
}

#[test]
fn a_name_at_a_word_boundary_matches() {
    assert!(find_whole_word("Kaelen drew his blade.", "Kaelen").is_some());
    assert!(find_whole_word("And then Kaelen.", "Kaelen").is_some());
    assert!(find_whole_word("Kaelen", "Kaelen").is_some());
    assert!(find_whole_word("(Kaelen)", "Kaelen").is_some());
    assert!(find_whole_word("—Kaelen—", "Kaelen").is_some());
}

#[test]
fn matching_is_case_insensitive() {
    assert!(find_whole_word("KAELEN drew his blade.", "Kaelen").is_some());
    assert!(find_whole_word("kaelen drew his blade.", "Kaelen").is_some());
    assert!(find_whole_word("Kaelen drew his blade.", "kAeLeN").is_some());
}

#[test]
fn a_possessive_still_counts_as_a_mention() {
    // Apostrophe is deliberately not a word character: "Kaelen's blade" plainly
    // mentions Kaelen, and treating ' as a word char would miss it.
    assert!(find_whole_word("Kaelen's blade broke.", "Kaelen").is_some());
}

#[test]
fn a_later_occurrence_is_found_when_an_earlier_one_is_embedded() {
    // The scan must not stop at the first substring hit if that hit fails the boundary
    // test — "Kaelendra" appears first, but "Kaelen" is genuinely present later.
    let text = "Kaelendra spoke first. Then Kaelen answered.";
    let at = find_whole_word(text, "Kaelen").expect("should find the real mention");
    assert!(at > 20, "found the embedded one at {at}");
}

#[test]
fn an_empty_or_blank_needle_never_matches() {
    assert_eq!(find_whole_word("anything", ""), None);
    assert_eq!(find_whole_word("anything", "   "), None);
}

#[test]
fn multi_word_names_match_as_a_unit() {
    assert!(find_whole_word("Kaelen Vord returned.", "Kaelen Vord").is_some());
    assert_eq!(find_whole_word("Kaelen returned.", "Kaelen Vord"), None);
}

// ------------------------------------------------------ protagonist check

const PROMPT_FULL: &str = "# The Ashen Vale\n\nKaelen Vord returns to the vale that burned.\n";
const PROMPT_SHORT: &str = "# The Ashen Vale\n\nKaelen returns to the vale that burned.\n";

#[test]
fn a_protagonist_named_in_the_prompt_is_not_a_warning() {
    let c = check_protagonist("Kaelen", PROMPT_SHORT);
    assert_eq!(c, ProtagonistCheck::Named);
    assert!(!c.is_warning());
    assert_eq!(warning(&c, "Kaelen"), None);
}

#[test]
fn the_incident_case_reports_the_longer_name_the_prompt_uses() {
    // `init --protagonist "Kaelen"` against a prompt saying "Kaelen Vord" — exactly
    // what split the live story's stats across two identities.
    let c = check_protagonist("Kaelen", PROMPT_FULL);
    assert_eq!(
        c,
        ProtagonistCheck::NamedWithinLongerName {
            in_prompt: "Kaelen Vord".to_string()
        }
    );
    assert!(c.is_warning());

    let w = warning(&c, "Kaelen").expect("should warn");
    assert!(w.contains("Kaelen Vord"), "{w}");
    assert!(w.contains("split"), "must say what goes wrong:\n{w}");
    assert!(
        w.contains("Editing prompt.md"),
        "must name the safe direction:\n{w}"
    );
    // Found by smoke-testing: suggesting `init --force --protagonist ...` was actively
    // harmful advice, because --force rewrites prompt.md to the template and loses the
    // premise. The warning must steer away from that, not toward it.
    assert!(
        w.contains("lose the premise"),
        "must warn that --force eats the prompt:\n{w}"
    );
}

#[test]
fn the_mirror_mistake_reports_which_words_the_prompt_does_use() {
    // Recorded more fully than the prompt uses: "Kaelen Vord" against a prompt that
    // only ever says "Kaelen". Same underlying error, opposite direction.
    let c = check_protagonist("Kaelen Vord", PROMPT_SHORT);
    assert_eq!(
        c,
        ProtagonistCheck::Absent {
            words_found: vec!["Kaelen".to_string()]
        }
    );
    let w = warning(&c, "Kaelen Vord").unwrap();
    assert!(w.contains("\"Kaelen\""), "{w}");
    assert!(w.contains("UnknownSubject"), "{w}");
}

#[test]
fn a_completely_absent_protagonist_says_an_unnamed_premise_is_legitimate() {
    // A prompt describing "a collector" and letting the model name him is valid, so the
    // warning must not read as an error.
    let c = check_protagonist(
        "Kaelen",
        "# A premise\n\nA collector walks out of the ash.\n",
    );
    assert_eq!(
        c,
        ProtagonistCheck::Absent {
            words_found: vec![]
        }
    );
    let w = warning(&c, "Kaelen").unwrap();
    assert!(
        w.contains("This is fine if"),
        "must not read as fatal:\n{w}"
    );
    assert!(w.contains("UnknownSubject"), "{w}");
}

#[test]
fn an_unset_protagonist_is_not_checked() {
    for empty in ["", "   "] {
        let c = check_protagonist(empty, PROMPT_FULL);
        assert_eq!(c, ProtagonistCheck::Unset);
        assert!(!c.is_warning());
        assert_eq!(warning(&c, empty), None);
    }
}

#[test]
fn a_name_embedded_in_a_different_name_is_absent_not_named() {
    // The boundary rule reaching the protagonist check: a prompt about "Kaelendra"
    // does not name "Kaelen".
    let c = check_protagonist("Kaelen", "# A premise\n\nKaelendra rules the vale.\n");
    assert!(matches!(c, ProtagonistCheck::Absent { .. }));
}

#[test]
fn a_lowercase_following_word_is_not_mistaken_for_a_surname() {
    let c = check_protagonist("Kaelen", "Kaelen walked into the ash.");
    assert_eq!(c, ProtagonistCheck::Named);
}

#[test]
fn a_sentence_boundary_is_not_mistaken_for_a_surname() {
    // "Kaelen. The vale…" — capitalised, but punctuation intervenes.
    assert_eq!(
        check_protagonist("Kaelen", "Kaelen. The vale burned."),
        ProtagonistCheck::Named
    );
    assert_eq!(
        check_protagonist("Kaelen", "Kaelen, The Collector, arrived."),
        ProtagonistCheck::Named
    );
}

#[test]
fn a_multi_word_protagonist_fully_present_is_named() {
    assert_eq!(
        check_protagonist("Kaelen Vord", PROMPT_FULL),
        ProtagonistCheck::Named
    );
}

// -------------------------------------------------------- possible aliases

#[test]
fn a_leading_whole_word_containment_is_flagged() {
    let pairs = possible_aliases(&["Kaelen".to_string(), "Kaelen Vord".to_string()]);
    assert_eq!(
        pairs,
        vec![("Kaelen".to_string(), "Kaelen Vord".to_string())]
    );
}

#[test]
fn unrelated_subjects_are_not_flagged() {
    let pairs = possible_aliases(&[
        "Kaelen".to_string(),
        "Vessa".to_string(),
        "Mara".to_string(),
    ]);
    assert!(pairs.is_empty(), "{pairs:?}");
}

#[test]
fn an_embedded_but_not_whole_word_name_is_not_flagged() {
    // "Kaelen" and "Kaelendra" are different characters, not one split identity.
    let pairs = possible_aliases(&["Kaelen".to_string(), "Kaelendra".to_string()]);
    assert!(pairs.is_empty(), "{pairs:?}");
}

#[test]
fn only_leading_containment_counts() {
    // "Vord" appearing at the end of "Kaelen Vord" is not evidence that a character
    // named Vord is the same person — narrowing this deliberately to cut false pairs.
    let pairs = possible_aliases(&["Vord".to_string(), "Kaelen Vord".to_string()]);
    assert!(pairs.is_empty(), "{pairs:?}");
}

#[test]
fn identical_names_are_not_a_pair() {
    let pairs = possible_aliases(&["Kaelen".to_string(), "Kaelen".to_string()]);
    assert!(pairs.is_empty(), "{pairs:?}");
}

#[test]
fn a_single_subject_produces_no_pairs() {
    assert!(possible_aliases(&["Kaelen".to_string()]).is_empty());
    assert!(possible_aliases(&[]).is_empty());
}

#[test]
fn the_pair_is_ordered_shortest_first() {
    // So the message reads "'Kaelen' and 'Kaelen Vord'" rather than the reverse.
    for input in [
        vec!["Kaelen Vord".to_string(), "Kaelen".to_string()],
        vec!["Kaelen".to_string(), "Kaelen Vord".to_string()],
    ] {
        assert_eq!(possible_aliases(&input)[0].0, "Kaelen");
    }
}
