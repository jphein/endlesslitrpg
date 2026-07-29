use litrpg_core::ledger::{LedgerEntry, Op, fold, rewind};
use proptest::prelude::*;

fn entry(seq: u64, chapter: u32, subject: &str, field: &str, op: Op, n: i64) -> LedgerEntry {
    LedgerEntry {
        seq,
        chapter,
        subject: subject.into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
        applied: true,
    }
}

fn text_entry(seq: u64, subject: &str, field: &str, value: &str) -> LedgerEntry {
    LedgerEntry {
        seq,
        chapter: 1,
        subject: subject.into(),
        field: field.into(),
        op: Op::Set,
        value_num: None,
        value_txt: Some(value.into()),
        applied: true,
    }
}

#[test]
fn set_then_add_then_sub() {
    let e = vec![
        entry(1, 1, "Kaelen", "hp", Op::Set, 100),
        entry(2, 1, "Kaelen", "hp", Op::Sub, 30),
        entry(3, 2, "Kaelen", "hp", Op::Add, 10),
    ];
    assert_eq!(fold(&e).num("Kaelen", "hp"), Some(80));
}

#[test]
fn add_to_absent_field_treats_it_as_zero() {
    let e = vec![entry(1, 1, "Kaelen", "xp", Op::Add, 250)];
    assert_eq!(fold(&e).num("Kaelen", "xp"), Some(250));
}

#[test]
fn rejected_entries_are_inert() {
    let mut rejected = entry(2, 1, "Kaelen", "hp", Op::Sub, 999);
    rejected.applied = false;
    let e = vec![entry(1, 1, "Kaelen", "hp", Op::Set, 100), rejected];
    assert_eq!(fold(&e).num("Kaelen", "hp"), Some(100));
}

#[test]
fn text_values_are_set_only() {
    let e = vec![text_entry(1, "Kaelen", "location", "Ashen Vale")];
    assert_eq!(fold(&e).txt("Kaelen", "location"), Some("Ashen Vale"));
}

#[test]
fn arithmetic_against_a_text_value_is_recorded_as_an_anomaly() {
    let e = vec![
        text_entry(1, "Kaelen", "location", "Ashen Vale"),
        entry(2, 1, "Kaelen", "location", Op::Add, 5),
    ];
    let snap = fold(&e);
    assert_eq!(snap.txt("Kaelen", "location"), Some("Ashen Vale"));
    assert_eq!(snap.anomalies.len(), 1);
}

#[test]
fn subjects_are_enumerated() {
    let e = vec![
        entry(1, 1, "Kaelen", "hp", Op::Set, 100),
        entry(2, 1, "Vessa", "hp", Op::Set, 80),
    ];
    let snap = fold(&e);
    let subjects = snap.subjects();
    assert!(subjects.contains("Kaelen"));
    assert!(subjects.contains("Vessa"));
    assert_eq!(subjects.len(), 2);
}

#[test]
fn rewind_includes_the_boundary_chapter() {
    let e = vec![
        entry(1, 40, "Kaelen", "hp", Op::Set, 100),
        entry(2, 41, "Kaelen", "hp", Op::Sub, 50),
    ];
    let kept = rewind(&e, 40);
    assert_eq!(kept.len(), 1);
    assert_eq!(fold(&kept).num("Kaelen", "hp"), Some(100));
}

proptest! {
    /// The fold sorts by `seq` itself, so input order cannot change the result.
    /// This is what lets the store hand over rows in any order SQLite returns them.
    #[test]
    fn fold_is_order_independent(deltas in prop::collection::vec(-50i64..50, 1..30)) {
        let entries: Vec<LedgerEntry> = deltas
            .iter()
            .enumerate()
            .map(|(i, d)| entry(i as u64 + 1, 1, "Kaelen", "gold", Op::Add, *d))
            .collect();

        let forward = fold(&entries);
        let mut reversed = entries.clone();
        reversed.reverse();

        prop_assert_eq!(forward.num("Kaelen", "gold"), fold(&reversed).num("Kaelen", "gold"));
        prop_assert_eq!(forward.num("Kaelen", "gold"), Some(deltas.iter().sum::<i64>()));
    }

    /// Flipping every entry to rejected must yield an empty snapshot.
    #[test]
    fn all_rejected_yields_empty_snapshot(n in 1usize..20) {
        let entries: Vec<LedgerEntry> = (0..n)
            .map(|i| {
                let mut e = entry(i as u64 + 1, 1, "Kaelen", "hp", Op::Add, 10);
                e.applied = false;
                e
            })
            .collect();
        prop_assert!(fold(&entries).values.is_empty());
    }

    /// rewind(N) keeps exactly the entries at or before chapter N.
    #[test]
    fn rewind_keeps_prefix(chapters in prop::collection::vec(1u32..100, 1..40), cut in 1u32..100) {
        let entries: Vec<LedgerEntry> = chapters
            .iter()
            .enumerate()
            .map(|(i, c)| entry(i as u64 + 1, *c, "Kaelen", "xp", Op::Add, 1))
            .collect();
        let kept = rewind(&entries, cut);
        prop_assert!(kept.iter().all(|e| e.chapter <= cut));
        prop_assert_eq!(kept.len(), entries.iter().filter(|e| e.chapter <= cut).count());
    }
}
