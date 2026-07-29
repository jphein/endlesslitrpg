use litrpg_cli::engine::{self, EngineStatus};
use litrpg_store::Store;

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

#[test]
fn no_heartbeat_reads_as_never_seen_not_as_stale() {
    // Different answers needing different words: nothing has ever rendered here, versus
    // something rendered and stopped.
    let s = store();
    assert_eq!(
        engine::engine_status(&s, 45, 1_000_000).unwrap(),
        EngineStatus::NeverSeen
    );
    assert!(!EngineStatus::NeverSeen.will_render());
}

#[test]
fn a_fresh_heartbeat_is_not_stale() {
    let s = store();
    s.stamp_engine_heartbeat(4242, "0.1.0", &["sherpa".to_string()])
        .unwrap();
    let hb = s.engine_heartbeat().unwrap().unwrap();
    let status = engine::engine_status(&s, 45, hb.seen_at + 5_000).unwrap();
    match status {
        EngineStatus::Reported {
            age_secs,
            stale,
            pid,
            ref backends,
            ..
        } => {
            assert_eq!(age_secs, 5);
            assert!(!stale);
            assert_eq!(pid, 4242);
            assert_eq!(backends, &["sherpa".to_string()]);
        }
        other => panic!("expected Reported, got {other:?}"),
    }
    assert!(status.will_render());
}

#[test]
fn staleness_is_generous_because_a_render_takes_minutes() {
    // The engine stamps once per cycle, and a cycle that renders a chapter includes
    // generation, TTS and encoding. A tight threshold would report "stopped" exactly when
    // it is busiest.
    let s = store();
    s.stamp_engine_heartbeat(1, "0.1.0", &[]).unwrap();
    let seen = s.engine_heartbeat().unwrap().unwrap().seen_at;

    // Five minutes into a long render: still trusted.
    let mid_render = engine::engine_status(&s, 45, seen + 300_000).unwrap();
    assert!(mid_render.will_render(), "{mid_render:?}");

    // Well past the bound: reported as stopped.
    let long_gone = engine::engine_status(&s, 45, seen + 3_600_000).unwrap();
    assert!(!long_gone.will_render(), "{long_gone:?}");
}

#[test]
fn the_stale_threshold_has_a_floor_so_a_short_poll_cannot_undercut_a_render() {
    assert_eq!(engine::stale_after_secs(1), engine::STALE_FLOOR_SECS);
    assert_eq!(engine::stale_after_secs(45), engine::STALE_FLOOR_SECS);
    // A long poll interval scales past the floor.
    assert_eq!(
        engine::stale_after_secs(600),
        600 * engine::STALE_AFTER_CYCLES
    );
}

#[test]
fn a_heartbeat_from_the_future_reads_as_age_zero_rather_than_underflowing() {
    // Clock skew, or a database restored from a backup taken on another machine.
    let s = store();
    s.stamp_engine_heartbeat(1, "0.1.0", &[]).unwrap();
    let seen = s.engine_heartbeat().unwrap().unwrap().seen_at;
    let status = engine::engine_status(&s, 45, seen - 60_000).unwrap();
    match status {
        EngineStatus::Reported {
            age_secs, stale, ..
        } => {
            assert_eq!(age_secs, 0);
            assert!(!stale, "a future timestamp must not read as stopped");
        }
        other => panic!("expected Reported, got {other:?}"),
    }
}

#[test]
fn missing_backends_are_reported_only_when_the_registry_is_known() {
    let known = EngineStatus::Reported {
        age_secs: 1,
        stale: false,
        pid: 1,
        version: "0.1.0".into(),
        backends: vec!["azure".into()],
    };
    assert_eq!(
        known.missing_backends(&["sherpa".into(), "azure".into()]),
        vec!["sherpa".to_string()]
    );
    // Unknown registry makes no claim.
    assert!(
        EngineStatus::NeverSeen
            .missing_backends(&["sherpa".into()])
            .is_empty()
    );
}

#[test]
fn missing_backends_are_deduplicated_and_ordered() {
    let e = EngineStatus::Reported {
        age_secs: 1,
        stale: false,
        pid: 1,
        version: "0.1.0".into(),
        backends: vec!["azure".into()],
    };
    assert_eq!(
        e.missing_backends(&["sherpa".into(), "sherpa".into(), "piper".into()]),
        vec!["piper".to_string(), "sherpa".to_string()]
    );
}

#[test]
fn ages_are_described_in_useful_units() {
    assert_eq!(engine::describe_age(3), "3s ago");
    assert_eq!(engine::describe_age(90), "90s ago");
    assert_eq!(engine::describe_age(300), "5 min ago");
    assert_eq!(engine::describe_age(7200), "2 h ago");
}

#[test]
fn a_duration_is_not_described_as_a_point_in_the_past() {
    // Found by smoke-testing: reusing the age formatter for the staleness threshold read
    // as "longer than the 15 min ago it is allowed".
    assert_eq!(engine::describe_duration(900), "15 min");
    assert!(!engine::describe_duration(900).contains("ago"));
    assert_eq!(engine::describe_duration(7200), "2 h");
}
