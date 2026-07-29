//! Speaker identity across the boundary (#14) and alias resolution (#11).
//!
//! The point of this file is that **one fixture drives all three mechanisms** —
//! `core::same_speaker`, the store's uniqueness constraint, and alias resolution — so a
//! divergence between them fails a test rather than being described in a comment. Seven places
//! each half-answered "are these the same person" and agreed by luck; a doc comment cannot
//! fail, this can.

use litrpg_core::ledger::Op;
use litrpg_core::speaker::{identity_key, same_speaker};
use litrpg_core::validate::Delta;
use litrpg_store::{NewStory, Store};

/// The spellings that must all denote one character.
const SAME: [&str; 4] = ["Kaelen", "kaelen", "  Kaelen  ", "KAELEN"];

fn store() -> Store {
    let s = Store::open_in_memory().unwrap();
    s.upsert_story(&NewStory {
        title: "The Ashen Vale".into(),
        protagonist: "Kaelen Vord".into(),
        prompt_path: "prompt.md".into(),
        prompt_hash: "h".into(),
        target_words: 2000,
    })
    .unwrap();
    s
}

fn num(subject: &str, field: &str, op: Op, n: i64) -> Delta {
    Delta {
        subject: subject.into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
    }
}

// ============================================ the boundary, one fixture

#[test]
fn core_and_the_store_agree_that_these_are_one_character() {
    // Mechanism 1: the owner.
    for name in SAME {
        assert!(
            same_speaker(name, "Kaelen"),
            "core says {name:?} is a different person"
        );
    }

    // Mechanism 2: the store's uniqueness. Each write must land on the same row.
    let s = store();
    for (i, name) in SAME.iter().enumerate() {
        s.upsert_cast(name, &format!("sherpa:voice-{i}:0"), "character", 1)
            .unwrap();
        assert_eq!(
            s.cast().unwrap().len(),
            1,
            "{name:?} created a second cast row — writes and reads disagree"
        );
    }
    // The last write wins, as an upsert should.
    assert_eq!(s.cast().unwrap()[0].voice_ref, "sherpa:voice-3:0");

    // Mechanism 3: the stored key is the owner's output, not an approximation.
    let row = &s.cast().unwrap()[0];
    assert_eq!(identity_key(&row.speaker), identity_key("Kaelen"));
}

#[test]
fn the_two_voices_one_character_bug_is_now_impossible() {
    // Demonstrated before the fix, through this same public API:
    //   cast rows for one character: 2
    //     "Kaelen" -> sherpa:voice-A:0
    //     "kaelen" -> sherpa:voice-B:0
    // The voice a character got was decided by `ORDER BY first_chapter, speaker`.
    let s = store();
    s.upsert_cast("Kaelen", "sherpa:voice-A:0", "character", 1)
        .unwrap();
    s.upsert_cast("kaelen", "sherpa:voice-B:0", "character", 1)
        .unwrap();
    let rows = s.cast().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].voice_ref, "sherpa:voice-B:0");
}

#[test]
fn whitespace_and_case_do_not_make_a_second_subject_known() {
    let s = store();
    s.upsert_cast("Kaelen Vord", "sherpa:x:0", "character", 1)
        .unwrap();
    s.upsert_cast("kaelen  vord", "sherpa:y:0", "character", 1)
        .unwrap();
    let known = s.known_subjects().unwrap();
    assert_eq!(
        known.len(),
        1,
        "one character produced {} known subjects: {known:?}",
        known.len()
    );
}

#[test]
fn reserved_names_are_pinned_to_one_spelling_on_write() {
    // `[ sYsTeM ]` and `[SYSTEM]` must be one cast member drawing one voice.
    let s = store();
    s.upsert_cast(" sYsTeM ", "sherpa:a:0", "system", 1)
        .unwrap();
    let rows = s.cast().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].speaker, litrpg_core::speaker::SYSTEM);
}

#[test]
fn a_multi_word_name_is_never_pinned_to_a_reserved_role() {
    // `System Lord` is a character; pinning it would silently delete a person.
    let s = store();
    s.upsert_cast("System Lord", "sherpa:a:0", "character", 1)
        .unwrap();
    assert_eq!(s.cast().unwrap()[0].speaker, "System Lord");
}

// ================================================== alias resolution (#11)

/// The live shape, faithfully: `story.protagonist` is `Kaelen Vord` while the **cast row** is
/// `Kaelen`, so both names are legitimately known subjects — the protagonist seeds one and the
/// cast row seeds the other — and pass 2 addressed deltas to whichever the context offered.
/// That is precisely how the split formed, and the first version of this fixture got it wrong:
/// with only `Kaelen Vord` in the cast, the gate correctly rejected `Kaelen` as unknown and no
/// split could form at all.
fn split_story() -> Store {
    let s = store(); // protagonist = "Kaelen Vord"
    s.upsert_cast("Kaelen", "sherpa:kokoro:18", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen Vord", "hp", Op::Set, 60))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen Vord", "max_hp", Op::Set, 64))
        .unwrap()
        .unwrap();
    // Chapter 3 addressed the same person by the other name.
    s.append_delta(3, &num("Kaelen", "gold", Op::Set, 12))
        .unwrap()
        .unwrap();
    s
}

#[test]
fn without_an_alias_the_sheet_is_split() {
    let s = split_story();
    let snap = s.snapshot().unwrap();
    assert_eq!(snap.num("Kaelen Vord", "hp"), Some(60));
    assert_eq!(
        snap.num("Kaelen Vord", "gold"),
        None,
        "gold is on the other sheet"
    );
    assert_eq!(snap.num("Kaelen", "gold"), Some(12));
    assert_eq!(snap.subjects().len(), 2);
}

#[test]
fn an_alias_merges_the_sheet_at_read_time() {
    let s = split_story();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();

    let snap = s.snapshot().unwrap();
    assert_eq!(snap.num("Kaelen Vord", "hp"), Some(60));
    assert_eq!(
        snap.num("Kaelen Vord", "gold"),
        Some(12),
        "chapter 3's stat change must appear on the one sheet"
    );
    assert_eq!(snap.subjects().len(), 1, "{:?}", snap.subjects());
    assert_eq!(
        snap.num("Kaelen", "gold"),
        None,
        "the alias is not its own subject"
    );
}

#[test]
fn the_ledger_itself_is_never_rewritten() {
    // JP chose read-time resolution precisely to keep append-only, which is what makes
    // `rewind N` free (§6.1). The rows must still say what they said.
    let s = split_story();
    let before = s.applied_count().unwrap();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();
    assert_eq!(s.applied_count().unwrap(), before);

    // Still resolvable to the original spelling for audit.
    assert_eq!(s.resolve_subject("Kaelen").unwrap(), "Kaelen Vord");
    assert_eq!(s.resolve_subject("Kaelen Vord").unwrap(), "Kaelen Vord");
}

#[test]
fn an_alias_matches_regardless_of_case_and_whitespace() {
    let s = split_story();
    s.add_alias("kaelen", "Kaelen Vord").unwrap();
    for spelling in SAME {
        assert_eq!(
            s.resolve_subject(spelling).unwrap(),
            "Kaelen Vord",
            "{spelling:?} did not resolve"
        );
    }
}

#[test]
fn known_subjects_reports_canonical_names_only() {
    // Offering both spellings to pass 2 would invite the model to keep the split alive.
    let s = split_story();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();
    let known = s.known_subjects().unwrap();
    assert!(known.contains("Kaelen Vord"), "{known:?}");
    assert!(!known.contains("Kaelen"), "{known:?}");
}

#[test]
fn a_delta_addressed_to_an_alias_is_clamped_against_the_canonical_subject() {
    // The bug this prevents: `hp -5` on `Kaelen` would clamp against nothing, compute
    // `0 - 5`, and be rejected as HpBelowZero while `Kaelen Vord` sits at 60.
    let s = split_story();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();

    s.append_delta(4, &num("Kaelen", "hp", Op::Sub, 5))
        .unwrap()
        .expect("must validate against the canonical subject's real hp");
    assert_eq!(s.snapshot().unwrap().num("Kaelen Vord", "hp"), Some(55));
}

#[test]
fn a_delta_addressed_to_an_alias_is_still_recorded_under_the_name_given() {
    // Audit: the row keeps the model's spelling; only the judgement is canonical. Proved
    // through the public API by removing the alias and watching the row reappear under its
    // own name — if `append_delta` had canonicalised on write, it would not.
    let s = split_story();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();
    s.append_delta(4, &num("Kaelen", "gold", Op::Add, 1))
        .unwrap()
        .unwrap();
    assert_eq!(s.snapshot().unwrap().num("Kaelen Vord", "gold"), Some(13));

    assert!(s.remove_alias("Kaelen").unwrap());
    let snap = s.snapshot().unwrap();
    assert_eq!(
        snap.num("Kaelen", "gold"),
        Some(13),
        "the row was stored as \"Kaelen\", so un-aliasing must reveal it there"
    );
    assert_eq!(snap.num("Kaelen Vord", "gold"), None);
}

#[test]
fn removing_an_alias_un_merges_rather_than_losing_anything() {
    // The decision is reversible, and only because history was never rewritten.
    let s = split_story();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();
    assert_eq!(s.snapshot().unwrap().subjects().len(), 1);

    assert!(
        s.remove_alias("KAELEN").unwrap(),
        "removal is case-insensitive too"
    );
    assert_eq!(s.snapshot().unwrap().subjects().len(), 2);
    assert!(
        !s.remove_alias("Kaelen").unwrap(),
        "second removal is a no-op"
    );
}

#[test]
fn a_name_cannot_be_its_own_alias() {
    let s = store();
    for (a, b) in [
        ("Kaelen", "Kaelen"),
        ("Kaelen", "kaelen"),
        ("Kaelen", " KAELEN "),
    ] {
        let err = s.add_alias(a, b).unwrap_err();
        assert!(
            matches!(err, litrpg_store::StoreError::SelfAlias { .. }),
            "{a:?} -> {b:?} should be refused, got {err:?}"
        );
    }
}

#[test]
fn re_aliasing_a_name_replaces_the_earlier_decision() {
    let s = store();
    s.add_alias("Kaelen", "Kaelen Vord").unwrap();
    s.add_alias("Kaelen", "Kaelen the Grey").unwrap();
    assert_eq!(s.aliases().unwrap().len(), 1);
    assert_eq!(s.resolve_subject("Kaelen").unwrap(), "Kaelen the Grey");
}

#[test]
fn resolution_is_one_hop_not_a_chain() {
    // A chain would make the result depend on resolution order.
    let s = store();
    s.add_alias("A", "B").unwrap();
    s.add_alias("B", "C").unwrap();
    assert_eq!(
        s.resolve_subject("A").unwrap(),
        "B",
        "must not follow B -> C"
    );
}

// ==================================================== migration behaviour

#[test]
fn the_seed_is_applied_only_where_the_collision_exists() {
    // A fresh database has no Kaelen and must not be told it does.
    let fresh = store();
    assert!(fresh.aliases().unwrap().is_empty(), "{:?}", fresh.aliases());
}

#[test]
fn migrations_reach_the_current_target_and_are_idempotent() {
    let s = store();
    assert_eq!(
        s.schema_version().unwrap(),
        litrpg_store::migrations::TARGET_VERSION
    );
    s.migrate().unwrap();
    s.migrate().unwrap();
    assert_eq!(
        s.schema_version().unwrap(),
        litrpg_store::migrations::TARGET_VERSION
    );
    assert_eq!(s.cast().unwrap().len(), 0);
}

#[test]
fn every_migration_has_a_name_so_a_failure_can_be_located() {
    for (i, m) in litrpg_store::migrations::MIGRATIONS.iter().enumerate() {
        assert!(!m.name.is_empty(), "migration {i} has no name");
        assert!(
            m.name.starts_with(&format!("{:03}_", i + 1)),
            "migration {i} is named {:?}, which does not match its index",
            m.name
        );
    }
}

#[test]
fn a_migration_failure_is_legible_rather_than_a_dump_of_the_file() {
    // Found by smoke-testing: naming the failing migration is worthless if the message then
    // reproduces the entire file. The first version chained the rusqlite error as a `#[source]`,
    // and `SqlInputError`'s Display carries the whole statement — so the name was buried under
    // eighty lines of SQL, printed twice by the error chain.
    let s = store();

    // Force a real failure: wind the version back while the objects still exist, so 007's
    // CREATE TABLE runs against a table that is already there. (Not a supported operation —
    // it is a way to make the error happen.)
    s.conn_pragma_for_test(6);
    let err = s.migrate().unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("007_subject_alias"),
        "must name the migration: {msg}"
    );
    assert!(msg.contains("index 6"), "{msg}");
    assert!(
        msg.contains("already exists"),
        "must say what went wrong: {msg}"
    );

    assert!(
        !msg.contains("CREATE TABLE"),
        "must not reproduce the statement:\n{msg}"
    );
    assert!(
        !msg.contains("Why the ledger is not rewritten"),
        "must not reproduce the file's comments:\n{msg}"
    );
    assert!(
        msg.lines().count() == 1,
        "a schema failure should be one readable line, got {}:\n{msg}",
        msg.lines().count()
    );
}

#[test]
fn the_seed_matches_a_subject_written_with_odd_whitespace() {
    // The first version of `ledger_has_subject` used `lower(subject) = ?`, which is only part
    // of `identity_key` — so a subject recorded as "Kaelen  Vord" would not have matched and
    // the seed would have been skipped without a word. Rule number eight, in the file whose
    // whole purpose is to have one rule.
    let s = store();
    s.upsert_cast("Kaelen", "sherpa:a:0", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 10))
        .unwrap()
        .unwrap();
    // Written with doubled whitespace and different case, as a model might.
    s.append_delta(1, &num("kaelen  VORD", "gold", Op::Set, 5))
        .unwrap()
        .unwrap_err(); // not a known subject, so it is stored rejected — but stored.

    // The seed's presence check must still see both names.
    s.seed_known_aliases_for_test().unwrap();
    assert_eq!(
        s.resolve_subject("Kaelen").unwrap(),
        "Kaelen Vord",
        "the seed should have applied: {:?}",
        s.aliases().unwrap()
    );
}

// ============================== the name/kind boundary (the seventh rule)

#[test]
fn a_reserved_name_cannot_be_given_a_persons_kind() {
    // The false negative that made `is_voice_not_a_person` necessary: a voice classified as a
    // character accrues stats, and a live run put a whole stat block under `subject: "SYSTEM"`
    // with every stage reporting success.
    let s = store();
    for name in ["SYSTEM", "system", "narrator", " NARRATOR "] {
        let err = s
            .upsert_cast(name, "sherpa:a:0", "character", 1)
            .unwrap_err();
        assert!(
            matches!(err, litrpg_store::StoreError::ReservedKindMismatch { .. }),
            "{name:?} should be refused, got {err:?}"
        );
    }
    assert!(
        s.cast().unwrap().is_empty(),
        "nothing may have been written"
    );
}

#[test]
fn a_person_cannot_be_given_a_roles_kind() {
    // The mirror, and just as silent: it would remove a real character from the known-subject
    // set, so their stat changes would start being rejected as UnknownSubject.
    let s = store();
    for kind in ["narrator", "system"] {
        let err = s.upsert_cast("Kaelen", "sherpa:a:0", kind, 1).unwrap_err();
        assert!(
            matches!(err, litrpg_store::StoreError::PersonGivenRoleKind { .. }),
            "kind {kind:?} should be refused for a character, got {err:?}"
        );
    }
}

#[test]
fn the_correct_pairings_are_all_accepted() {
    let s = store();
    s.upsert_cast("narrator", "sherpa:a:0", "narrator", 1)
        .unwrap();
    s.upsert_cast("SYSTEM", "sherpa:b:0", "system", 1).unwrap();
    s.upsert_cast("Kaelen", "sherpa:c:0", "character", 1)
        .unwrap();
    assert_eq!(s.cast().unwrap().len(), 3);
}

#[test]
fn a_multi_word_name_containing_a_role_word_is_a_person() {
    // `System Lord` is a character. The name rule must not claim him, and the kind rule must.
    let s = store();
    s.upsert_cast("System Lord", "sherpa:a:0", "character", 1)
        .unwrap();
    assert!(s.known_subjects().unwrap().contains("System Lord"));
}

#[test]
fn reclassifying_across_the_person_boundary_is_refused_too() {
    // `set_cast_kind` was the same hole through a different door.
    let s = store();
    s.upsert_cast("SYSTEM", "sherpa:a:0", "system", 1).unwrap();
    let err = s.set_cast_kind("SYSTEM", "character").unwrap_err();
    assert!(
        matches!(err, litrpg_store::StoreError::ReservedKindMismatch { .. }),
        "got {err:?}"
    );
    assert_eq!(s.cast().unwrap()[0].kind, "system", "unchanged");
}

#[test]
fn reclassifying_finds_the_row_by_identity_not_by_spelling() {
    // Also rule number eight: `WHERE speaker = ?1` was exact, so since 006 stores the canonical
    // spelling, `set_cast_kind("system", ..)` reported UnknownSpeaker for a row that exists.
    let s = store();
    s.upsert_cast("SYSTEM", "sherpa:a:0", "system", 1).unwrap();
    s.set_cast_kind("  sYsTeM  ", "narrator").unwrap();
    assert_eq!(s.cast().unwrap()[0].kind, "narrator");
}

#[test]
fn the_guard_messages_read_as_sentences() {
    // Found by running the binary: `cargo fmt` joined a line-continued string literal and baked
    // the indentation in, so the message read "`kind` is the only          authority on…".
    // Every assertion passed — they use `matches!` on the variant, which cannot see the words.
    // A formatter introducing an output defect is a new door onto §5.5, so this closes it by
    // asserting the shape of the prose rather than only the type.
    let s = store();
    let a = s
        .upsert_cast("SYSTEM", "sherpa:a:0", "character", 1)
        .unwrap_err()
        .to_string();
    let b = s
        .upsert_cast("Kaelen", "sherpa:a:0", "narrator", 1)
        .unwrap_err()
        .to_string();

    for msg in [&a, &b] {
        assert!(
            !msg.contains("  "),
            "run of spaces from a joined line continuation:\n{msg}"
        );
        assert_eq!(msg.lines().count(), 1, "should be one line:\n{msg}");
        assert!(
            msg.len() < 140,
            "too long to read at a prompt: {}",
            msg.len()
        );
    }
    assert!(a.contains("accrue stats"), "{a}");
    assert!(b.contains("stop their stat changes"), "{b}");
}
