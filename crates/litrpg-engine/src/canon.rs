//! Resolving a proposed character name onto an established one (issue #11).
//!
//! `/api/state` grew both `"Kaelen"` and `"Kaelen Vord"` — one character under two keys, with his
//! stats split so neither view is complete. It happened because `litrpg init --protagonist
//! "Kaelen"` ran against a prompt calling him "Kaelen Vord": both got ledger rows, and both are
//! now legitimately known subjects.
//!
//! The fix is the same shape as gender hints and the tagged-prose parser: **canonicalise what the
//! model emits before it becomes durable identity**, rather than validating and rejecting after.
//!
//! # Why this is deliberately less clever than the narrator-tag matcher
//!
//! [`litrpg_ember::parse::classify_speaker`] tolerates a typo two edits from `narrator`, and that
//! is safe *because the tag namespace is structural* — the model is choosing between two role
//! words and a name, so a near-miss of a role word is a broken role word.
//!
//! A cast is not structural. `Kaelen` and `Kaelith` are two edits apart and are two people, and
//! **fusing them is unrecoverable**: the ledger is append-only, so a wrongly merged subject cannot
//! be unmerged. So there is no similarity metric here at all. Only two things resolve:
//!
//! 1. an **exact** match, case-insensitively; or
//! 2. a **whole-word prefix** relation — `Kaelen` ↔ `Kaelen Vord`, either direction.
//!
//! Everything else is left exactly as the model wrote it, and the validation gate decides.
//!
//! # `story.protagonist` outranks even an exact match
//!
//! Necessarily, and this is the part that actually closes #11. The **cast** also confers subject
//! identity: prose tagged `[Kaelen]` writes a cast row, which makes `"Kaelen"` an established
//! name — so resolving only against established names would find an exact match and change
//! nothing, which is precisely how the duplicate key arises. So a name prefix-related to the
//! protagonist resolves to the protagonist even when it exactly matches something else.
//!
//! The cost, plainly: a character named *exactly* `"Kaelen"` who is not the protagonist
//! `"Kaelen Vord"` would be folded into him. That is a pathological cast, and the trade is
//! deliberate — the operator declared the protagonist's full name, the model did not.
//!
//! ## What that deliberately does *not* catch
//!
//! * **Surnames alone.** `Vord` does not resolve to `Kaelen Vord`, because a bare second name is a
//!   plausible nickname for more than one character (`Ash` for both `Sera Ash` and `Kaelen Ash`),
//!   and a wrong merge costs more than a missed one.
//! * **Character-level prefixes.** `Kael` does not resolve to `Kaelen Vord`. Words are the unit,
//!   so a shortened name is not silently a different name.
//! * **Anything already written.** This stops *new* instances. The two keys already in the live
//!   ledger stay split, because append-only means they must — retrospective merging needs an alias
//!   table, which is a separate decision.

use std::collections::BTreeSet;

/// What resolution did to a proposed name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectResolution {
    /// Already an established name, give or take case. Nothing to do.
    Canonical(String),
    /// Resolved onto an established name, which is what stops a second key appearing.
    Aliased { from: String, to: String },
    /// More than one established name could be meant. **Left alone** — guessing here is the
    /// failure mode this module exists to avoid.
    Ambiguous {
        name: String,
        candidates: Vec<String>,
    },
    /// Not an established name. Passed through untouched; the gate decides whether it is legitimate.
    New(String),
}

impl SubjectResolution {
    /// The name to actually use.
    pub fn name(&self) -> &str {
        match self {
            Self::Canonical(n) | Self::New(n) => n,
            Self::Aliased { to, .. } => to,
            Self::Ambiguous { name, .. } => name,
        }
    }

    /// Whether the name was changed, for logging.
    pub fn changed(&self) -> bool {
        matches!(self, Self::Aliased { .. })
    }
}

/// Whole-word, case-insensitive lowercase words.
fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether one name's words are a strict prefix of the other's.
fn prefix_related(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() || a.len() == b.len() {
        return false;
    }
    let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
    long.starts_with(short)
}

/// Resolve `proposed` against the established subjects.
///
/// `protagonist` is authoritative and is treated as an established name in its own right — it is
/// already load-bearing for `known_subjects()`, and when it is one of several prefix candidates it
/// wins, because it is the one name an operator declared rather than the model invented.
pub fn resolve_subject(
    proposed: &str,
    known: &BTreeSet<String>,
    protagonist: &str,
) -> SubjectResolution {
    let trimmed = proposed.trim();
    if trimmed.is_empty() {
        return SubjectResolution::New(proposed.to_string());
    }

    let candidates: Vec<&str> = known.iter().map(String::as_str).collect();
    let protagonist = protagonist.trim();

    let proposed_words = words(trimmed);
    let protagonist_words = words(protagonist);

    // 0. The protagonist outranks everything, including an exact match on some other candidate.
    //
    //    This is what makes `story.protagonist` "the anchor" rather than merely one more name. It
    //    matters because the *cast* also confers subject identity: prose tagged `[Kaelen]` gives a
    //    cast row, which makes "Kaelen" an established name, which would make it an exact match
    //    and leave issue #11 unfixed — the short form is exactly how the duplicate key arises.
    //
    //    The cost, stated plainly: a character named *exactly* "Kaelen" who is **not** the
    //    protagonist "Kaelen Vord" would be folded into him. That is a pathological cast, and the
    //    operator declared the protagonist's full name; the model did not.
    if !protagonist_words.is_empty() {
        if proposed_words == protagonist_words {
            return SubjectResolution::Canonical(protagonist.to_string());
        }
        if prefix_related(&proposed_words, &protagonist_words) {
            return SubjectResolution::Aliased {
                from: trimmed.to_string(),
                to: protagonist.to_string(),
            };
        }
    }

    // 1. Exact, comparing on normalised words — so neither a case difference nor a doubled space
    //    can mint a second key. Returns the *established* spelling, so casing converges too.
    if let Some(hit) = candidates.iter().find(|c| words(c) == proposed_words) {
        return SubjectResolution::Canonical((*hit).to_string());
    }

    // 2. Whole-word prefix, either direction.
    let matches: Vec<&str> = candidates
        .iter()
        .filter(|c| prefix_related(&proposed_words, &words(c)))
        .copied()
        .collect();

    match matches.as_slice() {
        [] => SubjectResolution::New(trimmed.to_string()),
        [only] => SubjectResolution::Aliased {
            from: trimmed.to_string(),
            to: (*only).to_string(),
        },
        // Two or more established names could be meant and none of them is the protagonist, so
        // there is nothing authoritative to pick with. Left as written.
        many => SubjectResolution::Ambiguous {
            name: trimmed.to_string(),
            candidates: many.iter().map(|c| (*c).to_string()).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // The incident from issue #11
    // -----------------------------------------------------------------------

    #[test]
    fn a_short_name_resolves_onto_the_protagonist() {
        let r = resolve_subject("Kaelen", &known(&[]), "Kaelen Vord");
        assert_eq!(
            r,
            SubjectResolution::Aliased {
                from: "Kaelen".into(),
                to: "Kaelen Vord".into()
            }
        );
        assert_eq!(r.name(), "Kaelen Vord");
        assert!(r.changed());
    }

    #[test]
    fn the_fuller_name_also_resolves_onto_the_established_one() {
        // The reverse direction: established is "Sera", the model embellishes.
        let r = resolve_subject("Sera Vane", &known(&["Sera"]), "Kaelen Vord");
        assert_eq!(r.name(), "Sera");
        assert!(r.changed());
    }

    #[test]
    fn an_exact_name_is_canonical_and_unchanged() {
        let r = resolve_subject("Kaelen Vord", &known(&["Kaelen Vord"]), "Kaelen Vord");
        assert_eq!(r, SubjectResolution::Canonical("Kaelen Vord".into()));
        assert!(!r.changed());
    }

    #[test]
    fn casing_converges_on_the_established_spelling() {
        let r = resolve_subject("kaelen vord", &known(&["Kaelen Vord"]), "");
        assert_eq!(r.name(), "Kaelen Vord", "two casings must not be two keys");
        assert!(!r.changed(), "a case difference is not an alias");
    }

    // -----------------------------------------------------------------------
    // The failures that must never happen: a wrong merge is unrecoverable
    // -----------------------------------------------------------------------

    #[test]
    fn two_edits_apart_is_not_the_same_person() {
        // `classify_speaker` would treat this as a typo. Here it must not.
        for other in ["Kaelith", "Kaelen2", "Kaefen", "Raelen"] {
            let r = resolve_subject(other, &known(&["Kaelen Vord"]), "Kaelen Vord");
            assert_eq!(
                r,
                SubjectResolution::New(other.to_string()),
                "{other} must not merge into Kaelen Vord"
            );
        }
    }

    #[test]
    fn a_character_level_prefix_does_not_resolve() {
        // Words are the unit. "Kael" is not "Kaelen".
        let r = resolve_subject("Kael", &known(&["Kaelen Vord"]), "Kaelen Vord");
        assert_eq!(r, SubjectResolution::New("Kael".into()));
    }

    #[test]
    fn a_surname_alone_does_not_resolve() {
        // A bare second name is a plausible nickname for more than one character, and a wrong
        // merge costs more than a missed one.
        let r = resolve_subject("Vord", &known(&["Kaelen Vord"]), "Kaelen Vord");
        assert_eq!(r, SubjectResolution::New("Vord".into()));
    }

    #[test]
    fn an_ambiguous_prefix_is_left_alone() {
        let r = resolve_subject(
            "Kaelen",
            &known(&["Kaelen Vord", "Kaelen Ash"]),
            "Sera", // protagonist is someone else, so no tie-break
        );
        match r {
            SubjectResolution::Ambiguous { name, candidates } => {
                assert_eq!(name, "Kaelen");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("two candidates must not resolve, got {other:?}"),
        }
    }

    #[test]
    fn the_protagonist_breaks_a_tie() {
        // Operator-declared beats model-invented.
        let r = resolve_subject(
            "Kaelen",
            &known(&["Kaelen Vord", "Kaelen Ash"]),
            "Kaelen Vord",
        );
        assert_eq!(r.name(), "Kaelen Vord");
        assert!(r.changed());
    }

    #[test]
    fn two_distinct_established_names_are_never_merged_with_each_other() {
        // Resolution only ever maps a *proposal* onto an established name.
        for name in ["Sera", "Kaelen Vord"] {
            let r = resolve_subject(name, &known(&["Sera", "Kaelen Vord"]), "Kaelen Vord");
            assert_eq!(r.name(), name);
            assert!(!r.changed());
        }
    }

    // -----------------------------------------------------------------------
    // Shape and edges
    // -----------------------------------------------------------------------

    #[test]
    fn an_unknown_character_passes_through_untouched() {
        let r = resolve_subject("Ilex", &known(&["Kaelen Vord"]), "Kaelen Vord");
        assert_eq!(r, SubjectResolution::New("Ilex".into()));
        assert_eq!(r.name(), "Ilex");
    }

    #[test]
    fn an_empty_name_is_passed_through_rather_than_matched() {
        assert_eq!(
            resolve_subject("", &known(&["Kaelen"]), "Kaelen"),
            SubjectResolution::New(String::new())
        );
        assert!(matches!(
            resolve_subject("   ", &known(&["Kaelen"]), "Kaelen"),
            SubjectResolution::New(_)
        ));
    }

    #[test]
    fn an_empty_protagonist_is_simply_not_a_candidate() {
        let r = resolve_subject("Kaelen", &known(&[]), "");
        assert_eq!(r, SubjectResolution::New("Kaelen".into()));
    }

    #[test]
    fn whitespace_is_normalised_for_comparison() {
        let r = resolve_subject("  Kaelen   Vord  ", &known(&["Kaelen Vord"]), "");
        assert_eq!(r.name(), "Kaelen Vord");
    }

    #[test]
    fn a_three_word_name_resolves_by_whole_words() {
        let r = resolve_subject("Kaelen Vord", &known(&["Kaelen Vord the Third"]), "");
        assert_eq!(r.name(), "Kaelen Vord the Third");

        // But a different middle word is a different person.
        let r = resolve_subject(
            "Kaelen Ash the Third",
            &known(&["Kaelen Vord the Third"]),
            "",
        );
        assert!(matches!(r, SubjectResolution::New(_)));
    }

    /// The reason this rule exists: the cast makes the short form an established name, so without
    /// the protagonist outranking an exact match, #11 would be unfixed.
    #[test]
    fn the_protagonist_outranks_an_exact_match_on_a_cast_row() {
        let r = resolve_subject("Kaelen", &known(&["Kaelen", "Sera"]), "Kaelen Vord");
        assert_eq!(
            r,
            SubjectResolution::Aliased {
                from: "Kaelen".into(),
                to: "Kaelen Vord".into()
            },
            "a cast row for the short form must not win against the declared protagonist"
        );
    }

    #[test]
    fn a_name_unrelated_to_the_protagonist_still_matches_exactly() {
        // The override is scoped to the protagonist; everyone else resolves normally.
        let r = resolve_subject("Sera", &known(&["Sera"]), "Kaelen Vord");
        assert_eq!(r, SubjectResolution::Canonical("Sera".into()));
    }

    #[test]
    fn the_protagonists_own_full_name_is_canonical() {
        let r = resolve_subject("Kaelen Vord", &known(&["Kaelen"]), "Kaelen Vord");
        assert_eq!(r, SubjectResolution::Canonical("Kaelen Vord".into()));
        assert!(!r.changed());
    }

    #[test]
    fn the_protagonist_is_established_before_their_first_ledger_row() {
        // Chapter 1: nothing is in the ledger yet, and the protagonist must still anchor.
        let r = resolve_subject("Kaelen", &BTreeSet::new(), "Kaelen Vord");
        assert_eq!(r.name(), "Kaelen Vord");
    }
}
