//! The engine's liveness signal. The CLI cannot see whether a separate process is
//! running, so commands whose whole effect is "the engine will pick this up" had no way
//! to tell a queued chapter from one queued into a void.

use litrpg_store::Store;

fn backends(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

/// `None` is not the same answer as "stale", and callers should say different things:
/// nothing has ever rendered against this database, versus something did and stopped.
#[test]
fn absent_until_an_engine_has_run() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.engine_heartbeat().unwrap().is_none());
}

#[test]
fn stamping_records_what_the_engine_reported() {
    let store = Store::open_in_memory().unwrap();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    store
        .stamp_engine_heartbeat(123_866, "0.1.0", &backends(&["azure", "sherpa"]))
        .unwrap();

    let hb = store.engine_heartbeat().unwrap().unwrap();
    assert_eq!(hb.pid, 123_866);
    assert_eq!(hb.version, "0.1.0");
    assert_eq!(hb.backends, backends(&["azure", "sherpa"]));
    assert!(
        hb.seen_at >= before,
        "seen_at {} predates the call at {before}",
        hb.seen_at
    );
}

/// Stamped once per poll cycle for the life of the process, so every call after the first
/// is a conflict on the singleton's primary key. Without `ON CONFLICT DO UPDATE` the
/// second cycle would fail — and the engine would keep running while its liveness signal
/// froze at startup, which is worse than having no signal.
///
/// Duplicate rows are not asserted here because `CHECK (id = 1)` makes them unreachable;
/// a test could only restate the schema.
#[test]
fn stamping_every_cycle_keeps_working() {
    let store = Store::open_in_memory().unwrap();
    for cycle in 0..5 {
        store
            .stamp_engine_heartbeat(1, "0.1.0", &backends(&["azure"]))
            .unwrap_or_else(|e| panic!("cycle {cycle} failed to stamp: {e}"));
    }
    assert!(store.engine_heartbeat().unwrap().is_some());
}

/// A restart changes the pid, and a rebuild can change both the version and the backend
/// set. All three have to move, or the row describes a process that is no longer there.
#[test]
fn a_restarted_engine_overwrites_the_previous_process() {
    let store = Store::open_in_memory().unwrap();
    store
        .stamp_engine_heartbeat(111, "0.1.0", &backends(&["azure"]))
        .unwrap();
    store
        .stamp_engine_heartbeat(222, "0.2.0", &backends(&["azure", "sherpa"]))
        .unwrap();

    let hb = store.engine_heartbeat().unwrap().unwrap();
    assert_eq!(hb.pid, 222);
    assert_eq!(hb.version, "0.2.0");
    assert_eq!(hb.backends, backends(&["azure", "sherpa"]));
}

/// The column this table exists for. An engine built without `--features sherpa`
/// registers only `["azure"]` and then silently substitutes a voice for every `sherpa:`
/// cast member — which is what happened to chapters 3 and 4 of the live serial. With the
/// live registry recorded, a caller can compare a cast row's backend against what the
/// running engine can actually serve.
#[test]
fn a_missing_backend_is_visible_to_a_caller() {
    let store = Store::open_in_memory().unwrap();
    store
        .stamp_engine_heartbeat(1, "0.1.0", &backends(&["azure"]))
        .unwrap();

    let hb = store.engine_heartbeat().unwrap().unwrap();
    let cast_voice = "sherpa:piper-en_GB-cori-high:0";
    let needed = cast_voice.split(':').next().unwrap();
    assert!(
        !hb.backends.iter().any(|b| b == needed),
        "the engine should not claim a backend it never registered"
    );
}

/// An engine that registered nothing is a real state — every backend failed to
/// initialise — and it is precisely when a caller most wants to be told. It must not be
/// confused with "no engine has ever run".
#[test]
fn an_engine_with_no_backends_is_still_a_live_engine() {
    let store = Store::open_in_memory().unwrap();
    store.stamp_engine_heartbeat(1, "0.1.0", &[]).unwrap();

    let hb = store
        .engine_heartbeat()
        .unwrap()
        .expect("an engine with no backends is still an engine");
    assert!(hb.backends.is_empty());
}
