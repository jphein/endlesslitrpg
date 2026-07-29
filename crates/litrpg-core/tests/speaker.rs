//! One owner for "is this the same person". Before this there were seven rules and they
//! disagreed; these tests pin the rule itself, and the store/engine tests assert that their
//! own comparisons route through it.

use litrpg_core::speaker::{
    NARRATOR, RESERVED, SYSTEM, canonical, identity_key, is_reserved, same_speaker,
};

#[test]
fn canonical_collapses_internal_whitespace_but_keeps_case() {
    assert_eq!(canonical("Sera   Vane"), "Sera Vane");
    assert_eq!(canonical("  Kaelen Vord  "), "Kaelen Vord");
    // Case is preserved: this is the name a reader sees.
    assert_eq!(canonical("kaelen vord"), "kaelen vord");
}

#[test]
fn canonical_pins_the_reserved_names_to_one_spelling() {
    for spelling in ["SYSTEM", "system", "sYsTeM", "  System  "] {
        assert_eq!(canonical(spelling), SYSTEM, "{spelling}");
    }
    for spelling in ["narrator", "NARRATOR", "Narrator", " nArRaToR "] {
        assert_eq!(canonical(spelling), NARRATOR, "{spelling}");
    }
}

/// Pinning a multi-word name to a role would silently delete a person.
#[test]
fn a_multi_word_name_is_never_a_reserved_role() {
    assert_eq!(canonical("System Lord"), "System Lord");
    assert_eq!(canonical("The Narrator Of Ash"), "The Narrator Of Ash");
    assert!(!is_reserved("System Lord"));
    assert!(!is_reserved("The Narrator Of Ash"));
}

/// The live wrong-answer path this module exists for: `cast.speaker UNIQUE` is binary in
/// SQLite, so these were two rows, while every reader compared them case-insensitively and
/// resolved the tie by row order.
#[test]
fn case_variants_are_one_person() {
    assert!(same_speaker("Kaelen", "kaelen"));
    assert!(same_speaker("KAELEN", "kaelen"));
    assert_eq!(identity_key("Kaelen"), identity_key("  KAELEN  "));
}

/// Whitespace is part of the identity rule, which is why `COLLATE NOCASE` could not have
/// expressed it — a stored key can, because it is this function's output.
#[test]
fn whitespace_variants_are_one_person() {
    assert!(same_speaker("Sera  Vane", "sera vane"));
    assert_eq!(identity_key("Sera   Vane"), "sera vane");
}

/// Different names are different people. `identity_key` merges spellings, never aliases —
/// `Kaelen` and `Kaelen Vord` stay distinct, which is issue #11 and a decision (an alias
/// mapping), not a case rule.
#[test]
fn an_alias_is_not_a_spelling() {
    assert!(!same_speaker("Kaelen", "Kaelen Vord"));
    assert_ne!(identity_key("Kaelen"), identity_key("Kaelen Vord"));
}

#[test]
fn identity_key_is_idempotent_and_agrees_with_canonical() {
    for name in [
        "Kaelen Vord",
        "  sYsTeM ",
        "Sera   Vane",
        "narrator",
        "System Lord",
    ] {
        let key = identity_key(name);
        assert_eq!(identity_key(&key), key, "not idempotent for {name:?}");
        assert_eq!(
            key,
            canonical(name).to_lowercase(),
            "identity_key must be canonical then lower-cased, for {name:?}"
        );
    }
}

#[test]
fn the_reserved_names_are_reserved_under_every_spelling() {
    for r in RESERVED {
        assert!(is_reserved(r));
        assert!(is_reserved(&r.to_uppercase()));
        assert!(is_reserved(&r.to_lowercase()));
    }
    assert!(!is_reserved("Kaelen Vord"));
    assert!(!is_reserved(""));
}

/// Strict on purpose (§5.3). Typo and alias tolerance belongs to `litrpg-ember`, which faces a
/// model; this faces a protocol we wrote. If this were lenient, a stored row's identity could
/// change with a heuristic.
#[test]
fn no_typo_or_alias_tolerance_here() {
    assert_eq!(canonical("narration"), "narration");
    assert!(!is_reserved("narration"));
    assert_eq!(canonical("sytsem"), "sytsem");
    assert!(!is_reserved("sytsem"));
}
