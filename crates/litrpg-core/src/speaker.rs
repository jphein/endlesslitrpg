//! What it means for two speaker names to be the same person.
//!
//! One owner, because before this there were seven rules for that question and they did not
//! agree:
//!
//! | Where | Rule |
//! |---|---|
//! | `cast.speaker UNIQUE` (SQL) | binary equality |
//! | `known_subjects` (SQL) | exact match; kind filtered by lowercase literals |
//! | `validate_delta` | `BTreeSet::contains` — exact |
//! | engine, ~10 sites | `eq_ignore_ascii_case` |
//! | `ember::parse::canonical_speaker` | collapses whitespace, pins reserved names |
//! | `cast::voice_divergence` | `to_lowercase()` |
//! | `is_voice_not_a_person` vs `known_subjects` | personhood by *name* vs by *kind* |
//!
//! The wrong answer that fell out: `cast.speaker TEXT NOT NULL UNIQUE` is a **binary**
//! constraint in SQLite, so `Kaelen` and `kaelen` are two rows — while every reader used
//! `eq_ignore_ascii_case` with `.find()`, so which voice the character got was decided by
//! `ORDER BY first_chapter, speaker` rather than by anyone's intent. Case-preserving,
//! case-*sensitive* writes against case-*insensitive* reads.
//!
//! # Two functions, because there are two questions
//!
//! [`canonical`] is the **storage** form — what to write down. [`identity_key`] is the
//! **comparison** form — what to compare. They differ because the stored name is shown to a
//! human (`Kaelen Vord`, not `kaelen vord`) while the comparison must ignore case. Collapsing
//! them into one function would force a choice between a correct display name and a correct
//! identity, which is the trap the seven rules were each half-solving.
//!
//! # Strict on purpose (spec §5.3)
//!
//! This module does **not** tolerate typos or aliases. `narration` does not become `narrator`
//! here. That leniency is `litrpg-ember`'s, because it faces a *model*; this faces a protocol
//! we wrote, and by the time a name reaches storage it should already be canonical. Adding
//! fuzzy matching here would mean a stored row's identity could change with a heuristic.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The one spelling of the narrator's name.
pub const NARRATOR: &str = "narrator";

/// The one spelling of the system voice's name. Upper-case because the prose tags it
/// `[SYSTEM]`, and the ledger's clerical register depends on it reading as a stamp.
pub const SYSTEM: &str = "SYSTEM";

/// The reserved names, in canonical spelling. Not people: they are roles, and nothing may be
/// cast as one or accrue stats under one.
pub const RESERVED: [&str; 2] = [NARRATOR, SYSTEM];

/// The storage form of a name: internal whitespace collapsed, reserved names pinned to their
/// one canonical spelling.
///
/// Case is otherwise **preserved**, because this is the name a reader sees. `identity_key` is
/// what ignores case.
///
/// A multi-word name is never a reserved role — `System Lord` is a character, and pinning it
/// to `SYSTEM` would silently delete a person. That matches
/// `ember::parse::classify_speaker`, deliberately.
pub fn canonical(name: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    for reserved in RESERVED {
        if collapsed.eq_ignore_ascii_case(reserved) {
            return reserved.to_string();
        }
    }
    collapsed
}

/// The comparison form: [`canonical`], then lower-cased. **This is what "the same person"
/// means.**
///
/// Store it alongside the display name and index *that*, so SQL indexes this rule's output
/// rather than reimplementing it. `COLLATE NOCASE` would be the same rule expressed a second
/// time in another language, and it cannot express whitespace collapsing at all.
pub fn identity_key(name: &str) -> String {
    canonical(name).to_lowercase()
}

/// Whether two names denote the same person.
pub fn same_speaker(a: &str, b: &str) -> bool {
    identity_key(a) == identity_key(b)
}

/// Whether a name is a reserved role rather than a person.
///
/// This answers "is this name a role", which is **not** the same question as "is this row a
/// person" — that one is answered by the row's `kind`, and `kind` is the only authority.
/// The two used to be interchanged, and they can disagree about something that is not a
/// spelling: a character legitimately named `System` in the prose is excluded by the name rule
/// and included by the kind rule. Use this to decide what to *call* something; use `kind` to
/// decide whether it can hold stats.
pub fn is_reserved(name: &str) -> bool {
    let key = identity_key(name);
    RESERVED.iter().any(|r| identity_key(r) == key)
}
