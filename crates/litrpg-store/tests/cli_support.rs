//! Coverage for the queries the CLI and daemon need on top of the engine's own.

use litrpg_core::ledger::Op;
use litrpg_core::validate::Delta;
use litrpg_store::{REWOUND_REASON, Store};

fn store_with_two_characters() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_cast("Kaelen", "sherpa:kokoro-multi-lang-v1_0:18", "character", 1)
        .unwrap();
    store
        .upsert_cast("Vessa", "sherpa:kokoro-multi-lang-v1_0:21", "character", 3)
        .unwrap();
    store
}

fn delta(subject: &str, field: &str, op: Op, n: i64) -> Delta {
    Delta {
        subject: subject.into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
    }
}

#[test]
fn notes_round_trip_and_drain() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.pending_notes().unwrap().is_empty());

    let id = store.insert_note("introduce a rival", "cli").unwrap();
    assert!(id > 0);
    store.insert_note("slow down, more worldbuilding", "watch").unwrap();

    let pending = store.pending_notes().unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].body, "introduce a rival");
    assert_eq!(pending[0].source, "cli");
    assert_eq!(pending[1].source, "watch");
    assert!(pending.iter().all(|n| n.consumed_chapter.is_none()));

    assert_eq!(store.mark_notes_consumed(42).unwrap(), 2);
    assert!(store.pending_notes().unwrap().is_empty());

    // A note queued after the drain is pending again.
    store.insert_note("bring back the thief", "candela").unwrap();
    assert_eq!(store.pending_notes().unwrap().len(), 1);
}

#[test]
fn cast_lists_in_first_appearance_order() {
    let store = store_with_two_characters();
    let cast = store.cast().unwrap();
    assert_eq!(cast.len(), 2);
    assert_eq!(cast[0].speaker, "Kaelen");
    assert_eq!(cast[0].first_chapter, 1);
    assert_eq!(cast[0].voice_ref, "sherpa:kokoro-multi-lang-v1_0:18");
    assert_eq!(cast[1].speaker, "Vessa");
    assert_eq!(cast[1].first_chapter, 3);
}

#[test]
fn applied_and_rejected_counts_are_separate() {
    let store = store_with_two_characters();
    store.append_delta(1, &delta("Kaelen", "hp", Op::Set, 100)).unwrap().unwrap();
    store.append_delta(1, &delta("Kaelen", "level", Op::Set, 3)).unwrap().unwrap();

    // Rejected: level may not decrease.
    assert!(store.append_delta(2, &delta("Kaelen", "level", Op::Set, 1)).unwrap().is_err());

    assert_eq!(store.applied_count().unwrap(), 2);
    assert_eq!(store.rejected_count().unwrap(), 1);
}

/// The §6.2 drift signal is only readable if payload-carrying rejections group
/// together. Two HpAboveMax rejections with *different* maxima must be one bucket.
#[test]
fn rejection_reasons_group_by_code_not_by_payload() {
    let store = store_with_two_characters();
    for (who, max) in [("Kaelen", 100), ("Vessa", 250)] {
        store.append_delta(1, &delta(who, "max_hp", Op::Set, max)).unwrap().unwrap();
        store.append_delta(1, &delta(who, "hp", Op::Set, max)).unwrap().unwrap();
        // Overheal: rejected as HpAboveMax, with a different `max` payload each time.
        assert!(store.append_delta(1, &delta(who, "hp", Op::Add, 500)).unwrap().is_err());
    }

    let reasons = store.rejection_reasons().unwrap();
    assert_eq!(reasons, vec![("HpAboveMax".to_string(), 2)]);
}

#[test]
fn rejection_reasons_are_ordered_most_frequent_first() {
    let store = store_with_two_characters();
    store.append_delta(1, &delta("Kaelen", "xp", Op::Set, 500)).unwrap().unwrap();

    // Two XpWouldDecrease, one UnknownField.
    assert!(store.append_delta(1, &delta("Kaelen", "xp", Op::Sub, 1)).unwrap().is_err());
    assert!(store.append_delta(1, &delta("Kaelen", "xp", Op::Set, 0)).unwrap().is_err());
    assert!(store.append_delta(1, &delta("Kaelen", "charisma", Op::Set, 9)).unwrap().is_err());

    let reasons = store.rejection_reasons().unwrap();
    assert_eq!(reasons[0], ("XpWouldDecrease".to_string(), 2));
    assert_eq!(reasons[1], ("UnknownField".to_string(), 1));
}

/// A rewind is deliberate, so it must not masquerade as prompt drift.
#[test]
fn rewound_rows_do_not_count_as_rejections() {
    let store = store_with_two_characters();
    store.append_delta(40, &delta("Kaelen", "hp", Op::Set, 100)).unwrap().unwrap();
    store.append_delta(41, &delta("Kaelen", "hp", Op::Sub, 60)).unwrap().unwrap();

    assert_eq!(store.rejected_count().unwrap(), 0);
    assert_eq!(store.rewind(40).unwrap(), 1);

    // The row is now applied = 0, but it is a rewind, not a rejection.
    assert_eq!(store.rejected_count().unwrap(), 0);
    assert!(store.rejection_reasons().unwrap().is_empty());
    assert_eq!(store.applied_count().unwrap(), 1);
}

/// The preview must use the same predicate as the rewind itself, or a confirmation
/// prompt could describe something different from what actually happens.
#[test]
fn rewind_preview_matches_what_rewind_does() {
    let store = store_with_two_characters();
    store.append_delta(40, &delta("Kaelen", "hp", Op::Set, 100)).unwrap().unwrap();
    store.append_delta(41, &delta("Kaelen", "hp", Op::Sub, 10)).unwrap().unwrap();
    store.append_delta(41, &delta("Kaelen", "gold", Op::Add, 5)).unwrap().unwrap();
    store.append_delta(43, &delta("Vessa", "hp", Op::Set, 80)).unwrap().unwrap();

    let (rows, chapters) = store.rewind_preview(40).unwrap();
    assert_eq!(rows, 3);
    assert_eq!(chapters, vec![41, 43]);

    assert_eq!(store.rewind(40).unwrap(), rows);

    // Idempotent: nothing left to do, and the preview agrees.
    assert_eq!(store.rewind_preview(40).unwrap(), (0, vec![]));
    assert_eq!(store.rewind(40).unwrap(), 0);
}

#[test]
fn rewind_marks_rows_with_the_shared_reason_constant() {
    let store = store_with_two_characters();
    store.append_delta(41, &delta("Kaelen", "hp", Op::Set, 100)).unwrap().unwrap();
    store.rewind(40).unwrap();
    assert_eq!(REWOUND_REASON, "rewound");
    assert_eq!(store.applied_count().unwrap(), 0);
}
