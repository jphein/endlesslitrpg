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
    s.upsert_cast("SYSTEM", "azure:en-US-Steffan:DragonHDLatestNeural", "system", 1)
        .unwrap();
    s.upsert_cast("Kaelen", "azure:en-US-Andrew:DragonHDLatestNeural", "character", 1)
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
    assert!(!known.contains("narrator"), "narrator is a voice, not a person");
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

/// The `EXCEPT` clause, not just the filtered union. A database that already accrued
/// rows under `SYSTEM` before the fix must not keep readmitting it through the ledger
/// union — which is precisely the state the live run left behind.
#[test]
fn a_database_already_polluted_with_system_rows_stops_accepting_more() {
    let s = Store::open_in_memory().unwrap();
    // Pollute first, while SYSTEM is still an unknown subject to nobody's benefit:
    // insert it as a *character* so the delta is accepted, then reclassify.
    s.upsert_cast("SYSTEM", "azure:en-US-Steffan:DragonHDLatestNeural", "character", 1)
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
    s.upsert_lore("Sera", "character", "sera", "Watches the exits.", 0, false, 1)
        .unwrap();
    assert!(s.known_subjects().unwrap().contains("Sera"));
}
