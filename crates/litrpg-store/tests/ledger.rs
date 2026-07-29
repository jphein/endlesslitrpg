use litrpg_core::ledger::Op;
use litrpg_core::validate::{Delta, Rejection};
use litrpg_store::Store;

fn store_with_kaelen() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_cast("Kaelen", "sherpa:kokoro-multi-lang-v1_0:18", "character", 1)
        .unwrap();
    store
}

fn delta(field: &str, op: Op, n: i64) -> Delta {
    Delta {
        subject: "Kaelen".into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
    }
}

#[test]
fn cast_members_are_known_subjects() {
    let store = store_with_kaelen();
    assert!(store.known_subjects().unwrap().contains("Kaelen"));
}

#[test]
fn accepted_delta_lands_in_the_snapshot() {
    let store = store_with_kaelen();
    store
        .append_delta(1, &delta("hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
}

#[test]
fn seq_increments_across_appends() {
    let store = store_with_kaelen();
    store
        .append_delta(1, &delta("hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    store
        .append_delta(1, &delta("hp", Op::Sub, 40))
        .unwrap()
        .unwrap();
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(60));
}

#[test]
fn rejected_delta_is_stored_but_inert() {
    let store = store_with_kaelen();
    store
        .append_delta(1, &delta("hp", Op::Set, 10))
        .unwrap()
        .unwrap();

    let outcome = store.append_delta(1, &delta("hp", Op::Sub, 999)).unwrap();
    assert_eq!(outcome, Err(Rejection::HpBelowZero));

    // Still 10 — the rejection did not apply.
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(10));
    // But it was recorded for audit.
    assert_eq!(store.rejected_count().unwrap(), 1);
}

#[test]
fn unknown_subject_is_rejected_not_silently_created() {
    let store = store_with_kaelen();
    let d = Delta {
        subject: "Kaelenn".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: Some(50),
        value_txt: None,
    };
    assert_eq!(
        store.append_delta(1, &d).unwrap(),
        Err(Rejection::UnknownSubject)
    );
    assert!(store.snapshot().unwrap().num("Kaelenn", "hp").is_none());
}

/// Regression: a rejected delta is retained for audit, and `known_subjects`
/// unions in ledger subjects. Without an `applied = 1` filter on that union the
/// first typo would enter the known set, so the *second* identical attempt would
/// be accepted — the gate would teach itself the ghost it had just rejected.
#[test]
fn a_rejected_subject_does_not_become_known() {
    let store = store_with_kaelen();
    let d = Delta {
        subject: "Kaelenn".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: Some(50),
        value_txt: None,
    };
    assert_eq!(
        store.append_delta(1, &d).unwrap(),
        Err(Rejection::UnknownSubject)
    );
    assert!(!store.known_subjects().unwrap().contains("Kaelenn"));
    assert_eq!(
        store.append_delta(1, &d).unwrap(),
        Err(Rejection::UnknownSubject)
    );
    assert!(store.snapshot().unwrap().num("Kaelenn", "hp").is_none());
    assert_eq!(store.rejected_count().unwrap(), 2);
}

#[test]
fn rewind_deactivates_later_chapters() {
    let store = store_with_kaelen();
    store
        .append_delta(40, &delta("hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    store
        .append_delta(41, &delta("hp", Op::Sub, 60))
        .unwrap()
        .unwrap();
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(40));

    let touched = store.rewind(40).unwrap();
    assert_eq!(touched, 1);
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
}

#[test]
fn rewind_is_idempotent() {
    let store = store_with_kaelen();
    store
        .append_delta(40, &delta("hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    store
        .append_delta(41, &delta("hp", Op::Sub, 60))
        .unwrap()
        .unwrap();
    store.rewind(40).unwrap();
    assert_eq!(store.rewind(40).unwrap(), 0);
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
}

#[test]
fn lore_characters_are_known_subjects_too() {
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_lore("Vessa", "character", "vessa,thief", "A thief.", 0, false, 3)
        .unwrap();
    assert!(store.known_subjects().unwrap().contains("Vessa"));
    let d = Delta {
        subject: "Vessa".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: Some(70),
        value_txt: None,
    };
    assert_eq!(store.append_delta(3, &d).unwrap(), Ok(()));
}
