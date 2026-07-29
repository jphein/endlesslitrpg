//! The narrator and SYSTEM have voices but are not people.
//!
//! Found by a live run: both get `cast` rows so they can be voiced, and because
//! `known_subjects()` drew from `cast` unfiltered, pass 2 was offered `SYSTEM` as a
//! subject and the gate **accepted** it. An entire stat block landed under
//! `subject: "SYSTEM"` with `rejected = 0`, so the protagonist's inventory accrued to
//! a pseudo-person while his character screen stayed empty — with every stage
//! reporting success.

use litrpg_core::ledger::Op;
use litrpg_core::validate::{Delta, Rejection};
use litrpg_store::Store;

fn cast_store() -> Store {
    let s = Store::open_in_memory().unwrap();
    s.upsert_cast("narrator", "sherpa:piper-en_GB-cori-high:0", "narrator", 1)
        .unwrap();
    s.upsert_cast(
        "SYSTEM",
        "azure:en-US-Steffan:DragonHDLatestNeural",
        "system",
        1,
    )
    .unwrap();
    s.upsert_cast(
        "Kaelen",
        "azure:en-US-Andrew:DragonHDLatestNeural",
        "character",
        1,
    )
    .unwrap();
    s
}

fn delta(subject: &str, field: &str, n: i64) -> Delta {
    Delta {
        subject: subject.into(),
        field: field.into(),
        op: Op::Add,
        value_num: Some(n),
        value_txt: None,
    }
}

#[test]
fn characters_are_known_but_narrator_and_system_are_not() {
    let known = cast_store().known_subjects().unwrap();
    assert!(known.contains("Kaelen"));
    assert!(!known.contains("SYSTEM"), "SYSTEM is a voice, not a person");
    assert!(
        !known.contains("narrator"),
        "narrator is a voice, not a person"
    );
}

#[test]
fn the_gate_rejects_stats_for_the_system_voice() {
    let s = cast_store();
    assert_eq!(
        s.append_delta(1, &delta("SYSTEM", "xp", 150)).unwrap(),
        Err(Rejection::UnknownSubject)
    );
    assert_eq!(
        s.append_delta(1, &delta("narrator", "gold", 10)).unwrap(),
        Err(Rejection::UnknownSubject)
    );

    // The protagonist still works — the fix must not cost real characters.
    s.append_delta(1, &delta("Kaelen", "xp", 150))
        .unwrap()
        .unwrap();
    assert_eq!(s.snapshot().unwrap().num("Kaelen", "xp"), Some(150));
    assert!(s.snapshot().unwrap().num("SYSTEM", "xp").is_none());
}

/// `upsert_cast` now refuses to create the pollution the next test defends against, so the
/// hole is closed at the write boundary. Asserted here so the two cannot drift apart: if this
/// guard is ever removed, this test fails rather than the one below silently becoming
/// reachable-by-API again.
#[test]
fn a_reserved_name_cannot_be_cast_as_a_character() {
    let s = Store::open_in_memory().unwrap();
    let err = s
        .upsert_cast("SYSTEM", "azure:en-US-Steffan", "character", 1)
        .unwrap_err();
    assert!(err.to_string().contains("SYSTEM"), "{err}");
    assert!(
        s.cast().unwrap().is_empty(),
        "nothing should have been written"
    );
}

/// The `EXCEPT` clause, not just the filtered union. A database that already accrued rows
/// under `SYSTEM` before the fix must not keep readmitting it through the ledger union —
/// which is precisely the state the live run left behind.
#[test]
fn a_database_already_polluted_with_system_rows_stops_accepting_more() {
    let s = Store::open_in_memory().unwrap();
    // The pollution is injected out-of-band, because `upsert_cast` now refuses it — see the
    // test above. That refusal is the primary defence; this one is the read-side gate that
    // still has to hold for rows written *before* the guard existed, or by a hand edit. A
    // migration cannot retroactively clean them, so the gate cannot be retired.
    //
    // This is §5.5's rule meeting a guard that makes the broken state unreachable: a
    // recovery path must be tested from the broken state, so when the API stops being able
    // to produce it, the test constructs it directly. `raw_execute_for_tests` exists for
    // exactly this and its doc comment says so.
    s.raw_execute_for_tests(
        "INSERT INTO cast (speaker, voice_ref, kind, first_chapter, identity_key)
         VALUES ('SYSTEM', 'azure:en-US-Steffan', 'character', 1, 'system')",
    )
    .unwrap();
    s.append_delta(1, &delta("SYSTEM", "xp", 150))
        .unwrap()
        .unwrap();
    assert_eq!(s.snapshot().unwrap().num("SYSTEM", "xp"), Some(150));

    // Now classify it correctly, as the engine does.
    s.set_cast_kind("SYSTEM", "system").unwrap();

    assert!(!s.known_subjects().unwrap().contains("SYSTEM"));
    assert_eq!(
        s.append_delta(2, &delta("SYSTEM", "xp", 999)).unwrap(),
        Err(Rejection::UnknownSubject)
    );
    // The historical rows remain — the ledger is append-only and auditable — but
    // nothing new accrues.
    assert_eq!(s.snapshot().unwrap().num("SYSTEM", "xp"), Some(150));
}

#[test]
fn lore_characters_are_still_known() {
    let s = cast_store();
    s.upsert_lore(
        "Sera",
        "character",
        "sera",
        "Watches the exits.",
        0,
        false,
        1,
    )
    .unwrap();
    assert!(s.known_subjects().unwrap().contains("Sera"));
}

/// A corrupt `segments.kind` fails loudly rather than becoming `Narrator`.
///
/// The store used to coerce anything unrecognised, and kind **selects the voice** — the
/// cast assigner maps Narrator and System to different voices from Character. So a
/// silent coercion re-voices a character, and the only symptom is hearing the wrong
/// person speak. Same asymmetry that made `op_from_str` strict.
#[test]
fn an_unrecognised_segment_kind_is_an_error_not_a_narrator() {
    use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
    use litrpg_store::NewChapter;

    let s = Store::open_in_memory().unwrap();
    s.insert_chapter(&NewChapter {
        number: 1,
        title: "Chapter 1".into(),
        text_md: "[Kaelen] \"Pay up.\"".into(),
        prompt_hash: "fnv1a64:cbf29ce484222325".into(),
        state_dirty: false,
    })
    .unwrap();
    let m = Manifest::new(
        1,
        vec![Segment {
            idx: 0,
            speaker: "Kaelen".into(),
            kind: SpeakerKind::Character,
            voice_ref: "sherpa:kokoro-multi-lang-v1_0:18".into(),
            text: "Pay up.".into(),
            start_ms: 0,
            end_ms: 1000,
        }],
    );
    s.attach_audio(1, &m).unwrap();
    assert_eq!(s.segments(1).unwrap()[0].kind, SpeakerKind::Character);

    // Simulate a hand-edit, a bad migration, or a casing change in the canonical form.
    s.raw_execute_for_tests("UPDATE segments SET kind = 'Character'")
        .unwrap();

    let err = s.segments(1).unwrap_err();
    assert!(err.to_string().contains("unrecognised kind"), "{err}");
    assert!(err.to_string().contains("Character"), "{err}");
}
