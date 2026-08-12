//! The buffer target and its change trail (schema 008).

use litrpg_store::Store;

fn store() -> Store {
    Store::open_in_memory().expect("in-memory store")
}

/// `None`, not a default. The store does not know what `litrpg.toml` says, and inventing
/// a number here would put a value in the throttle that nobody chose.
#[test]
fn an_untouched_store_has_no_target() {
    assert_eq!(store().buffer_target().unwrap(), None);
}

#[test]
fn the_newest_change_is_the_effective_target() {
    let s = store();
    s.record_buffer_target(4, "cli", "first").unwrap();
    s.record_buffer_target(9, "voice", "second").unwrap();
    s.record_buffer_target(6, "cli", "third, and lower").unwrap();

    assert_eq!(
        s.buffer_target().unwrap(),
        Some(6),
        "newest wins, including when it lowers the target"
    );
}

/// Ordered by `id`, not by `at`. Three writes inside one millisecond share a timestamp,
/// and ordering by time would then make "the newest" ambiguous — which, for the value the
/// engine throttles on, is a coin flip rather than a bug you notice.
#[test]
fn same_millisecond_writes_still_have_a_total_order() {
    let s = store();
    for target in [3u32, 4, 5, 6, 7] {
        s.record_buffer_target(target, "cli", "rapid").unwrap();
    }
    assert_eq!(s.buffer_target().unwrap(), Some(7));

    let history = s.buffer_target_history(10).unwrap();
    let ids: Vec<i64> = history.iter().map(|c| c.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.reverse();
    assert_eq!(ids, sorted, "history must be newest-first by id");
}

/// The trail is the queryable replacement for the prose that used to live in
/// `litrpg.toml`'s comments, so it has to carry who and why, not just what.
#[test]
fn the_trail_records_who_asked_and_why() {
    let s = store();
    s.record_buffer_target(10, "voice", "cast ember continue")
        .unwrap();

    let latest = s.buffer_target_history(1).unwrap();
    let c = latest.first().expect("one row");
    assert_eq!(c.target, 10);
    assert_eq!(c.source, "voice");
    assert_eq!(c.justification, "cast ember continue");
    assert!(c.at > 0, "the store stamps its own clock");
}

/// Append-only: a later change adds a row rather than editing the previous one, so the
/// reasoning behind every past target survives.
#[test]
fn changes_accumulate_rather_than_overwrite() {
    let s = store();
    s.record_buffer_target(7, "cli", "verify the extractor")
        .unwrap();
    s.record_buffer_target(8, "cli", "local tts now, so a chapter costs cpu")
        .unwrap();

    let history = s.buffer_target_history(10).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].target, 8);
    assert_eq!(history[1].target, 7);
    assert_eq!(history[1].justification, "verify the extractor");
}

#[test]
fn history_respects_its_limit() {
    let s = store();
    for t in [2u32, 3, 4, 5] {
        s.record_buffer_target(t, "cli", "x").unwrap();
    }
    assert_eq!(s.buffer_target_history(2).unwrap().len(), 2);
}
