use litrpg_cli::{CliError, cast, note, render, rewind, state, status};
use litrpg_core::ledger::Op;
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_core::validate::Delta;
use litrpg_store::{NewChapter, Store};

// ------------------------------------------------------------------ helpers

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn with_kaelen() -> Store {
    let s = store();
    s.upsert_cast("Kaelen", "sherpa:kokoro-multi-lang-v1_0:18", "character", 1)
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

fn txt(subject: &str, field: &str, v: &str) -> Delta {
    Delta {
        subject: subject.into(),
        field: field.into(),
        op: Op::Set,
        value_num: None,
        value_txt: Some(v.into()),
    }
}

fn add_chapter(s: &Store, n: u32) {
    s.insert_chapter(&NewChapter {
        number: n,
        title: format!("Chapter {n}"),
        text_md: "[narrator] Text.".into(),
        prompt_hash: "fnv1a64:0000000000000000".into(),
        state_dirty: false,
    })
    .unwrap();
}

fn give_audio(s: &Store, n: u32) {
    let m = Manifest::new(
        n,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "sherpa:piper-en_GB-cori-high:0".into(),
            text: "Text.".into(),
            start_ms: 0,
            end_ms: 1000,
        }],
    );
    s.attach_audio(n, &m, &format!("{n}.pcm"), &format!("{n}.mp3"))
        .unwrap();
}

// --------------------------------------------------------------------- note

#[test]
fn note_is_queued_and_counted() {
    let s = store();
    let added = note::add(&s, "introduce a rival").unwrap();
    assert_eq!(added.body, "introduce a rival");
    assert_eq!(added.source, "cli");
    assert_eq!(added.pending, 1);
    assert!(added.id > 0);

    let second = note::add(&s, "burn down the inn").unwrap();
    assert_eq!(second.pending, 2);
    assert_eq!(s.pending_notes().unwrap().len(), 2);
}

#[test]
fn note_body_is_trimmed() {
    let s = store();
    let added = note::add(&s, "  a rival appears \n").unwrap();
    assert_eq!(added.body, "a rival appears");
    assert_eq!(s.pending_notes().unwrap()[0].body, "a rival appears");
}

#[test]
fn empty_and_whitespace_notes_are_refused_and_not_stored() {
    let s = store();
    for bad in ["", "   ", "\t\n"] {
        let err = note::add(&s, bad).unwrap_err();
        assert!(
            matches!(err, CliError::EmptyNote),
            "{bad:?} should be refused, got {err:?}"
        );
    }
    assert!(s.pending_notes().unwrap().is_empty());
}

// --------------------------------------------------------------------- cast

#[test]
fn cast_list_is_empty_on_a_fresh_store() {
    assert!(cast::list(&store()).unwrap().is_empty());
}

#[test]
fn cast_list_reports_entries_with_the_backend_split_out() {
    let s = with_kaelen();
    let entries = cast::list(&s).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].speaker, "Kaelen");
    assert_eq!(entries[0].kind, "character");
    assert_eq!(entries[0].first_chapter, 1);
    assert_eq!(entries[0].backend.as_deref(), Some("sherpa"));
}

#[test]
fn cast_set_overrides_an_existing_voice_and_preserves_provenance() {
    let s = with_kaelen();
    let outcome = cast::set(&s, "Kaelen", "azure:en-GB-RyanNeural", false, None).unwrap();
    assert_eq!(
        outcome,
        cast::CastSetOutcome::Overridden {
            speaker: "Kaelen".into(),
            from: "sherpa:kokoro-multi-lang-v1_0:18".into(),
            to: "azure:en-GB-RyanNeural".into(),
        }
    );

    let row = &cast::list(&s).unwrap()[0];
    assert_eq!(row.voice_ref, "azure:en-GB-RyanNeural");
    // kind and first_chapter are provenance — an override must not rewrite them.
    assert_eq!(row.kind, "character");
    assert_eq!(row.first_chapter, 1);
}

#[test]
fn cast_set_refuses_an_unknown_speaker_without_new() {
    // A typo must not create a cast row: cast rows feed known_subjects, so this
    // would hand the validation gate a ghost subject.
    let s = with_kaelen();
    let err = cast::set(&s, "Kaelenn", "sherpa:piper-en_GB-cori-high:0", false, None).unwrap_err();
    assert!(
        matches!(&err, CliError::UnknownSpeaker { speaker } if speaker == "Kaelenn"),
        "got {err:?}"
    );
    assert_eq!(cast::list(&s).unwrap().len(), 1);
    assert!(!s.known_subjects().unwrap().contains("Kaelenn"));
}

#[test]
fn cast_set_adds_deliberately_with_new() {
    let s = with_kaelen();
    add_chapter(&s, 1);
    add_chapter(&s, 2);

    let outcome = cast::set(&s, "Vessa", "sherpa:piper-en_GB-alba:0", true, None).unwrap();
    assert_eq!(
        outcome,
        cast::CastSetOutcome::Added {
            speaker: "Vessa".into(),
            voice_ref: "sherpa:piper-en_GB-alba:0".into(),
            kind: "character".into(),
            // Pre-assigned voice belongs to the chapter not yet written.
            first_chapter: 3,
        }
    );
    assert!(s.known_subjects().unwrap().contains("Vessa"));
}

#[test]
fn cast_set_honours_an_explicit_kind() {
    let s = store();
    let outcome = cast::set(
        &s,
        "SYSTEM",
        "sherpa:piper-en_GB-alba:0",
        true,
        Some("system"),
    )
    .unwrap();
    assert!(matches!(
        outcome,
        cast::CastSetOutcome::Added { ref kind, .. } if kind == "system"
    ));
    assert_eq!(cast::list(&s).unwrap()[0].kind, "system");
}

#[test]
fn cast_set_validates_the_voice_ref_before_writing_anything() {
    let s = with_kaelen();
    for bad in ["novoice", ":empty-backend", "sherpa:", ""] {
        let err = cast::set(&s, "Kaelen", bad, false, None).unwrap_err();
        assert!(
            matches!(err, CliError::BadVoiceRef { .. }),
            "{bad:?} should be rejected, got {err:?}"
        );
    }
    // Unchanged: validation happens before the write.
    assert_eq!(
        cast::list(&s).unwrap()[0].voice_ref,
        "sherpa:kokoro-multi-lang-v1_0:18"
    );
}

#[test]
fn cast_set_accepts_an_azure_ref_whose_remainder_contains_a_colon() {
    // The bug the spec called out: split on the first colon only.
    let s = with_kaelen();
    cast::set(
        &s,
        "Kaelen",
        "azure:en-GB-Ada:DragonHDLatestNeural",
        false,
        None,
    )
    .unwrap();
    let row = &cast::list(&s).unwrap()[0];
    assert_eq!(row.voice_ref, "azure:en-GB-Ada:DragonHDLatestNeural");
    assert_eq!(row.backend.as_deref(), Some("azure"));
}

// ------------------------------------------------------------------- status

#[test]
fn status_on_an_empty_store_is_all_zeros_and_quiet() {
    let r = status::status(&store(), 3).unwrap();
    assert_eq!(r.latest_chapter, 0);
    assert_eq!(r.total_chapters, 0);
    assert_eq!(r.chapters_with_audio, 0);
    assert_eq!(r.rendered_tail, 0);
    assert_eq!(r.applied_deltas, 0);
    assert_eq!(r.rejected_deltas, 0);
    assert_eq!(r.rejection_rate, 0.0);
    assert!(!r.buffer_ok);
    assert!(!r.drift_warning(), "no deltas must not read as drift");
}

#[test]
fn status_counts_chapters_and_audio() {
    let s = store();
    for n in 1..=4 {
        add_chapter(&s, n);
    }
    give_audio(&s, 1);
    give_audio(&s, 3);
    give_audio(&s, 4);

    let r = status::status(&s, 2).unwrap();
    assert_eq!(r.latest_chapter, 4);
    assert_eq!(r.total_chapters, 4);
    assert_eq!(r.chapters_with_audio, 3);
    // Contiguous run back from the latest is 4 and 3 — chapter 2 breaks it.
    assert_eq!(r.rendered_tail, 2);
    assert!(r.buffer_ok);
}

#[test]
fn rendered_tail_stops_at_the_first_gap_below_the_latest() {
    let s = store();
    for n in 1..=3 {
        add_chapter(&s, n);
    }
    // Latest chapter has no audio at all: nothing is ready to play next.
    give_audio(&s, 1);
    give_audio(&s, 2);
    let r = status::status(&s, 2).unwrap();
    assert_eq!(r.chapters_with_audio, 2);
    assert_eq!(r.rendered_tail, 0);
    assert!(!r.buffer_ok);
}

#[test]
fn status_reports_buffer_against_the_configured_target() {
    let s = store();
    for n in 1..=2 {
        add_chapter(&s, n);
        give_audio(&s, n);
    }
    assert!(status::status(&s, 2).unwrap().buffer_ok);
    assert!(!status::status(&s, 3).unwrap().buffer_ok);
}

#[test]
fn status_reports_applied_and_rejected_delta_counts_and_rate() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "max_hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Sub, 999))
        .unwrap()
        .unwrap_err();

    let r = status::status(&s, 3).unwrap();
    assert_eq!(r.applied_deltas, 2);
    assert_eq!(r.rejected_deltas, 1);
    assert_eq!(r.total_deltas(), 3);
    assert!((r.rejection_rate - 1.0 / 3.0).abs() < 1e-9);
    assert!(r.drift_warning(), "33% rejection is drift");
}

#[test]
fn top_rejections_group_by_code_without_fragmenting_on_payload() {
    // HpAboveMax carries its max. If the stored reason kept the payload, these two
    // rejections would land in separate buckets and the drift histogram would be
    // noise instead of a signal.
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "max_hp", Op::Set, 50))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 10))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap_err();
    s.append_delta(1, &num("Kaelen", "max_hp", Op::Set, 60))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 200))
        .unwrap()
        .unwrap_err();

    let r = status::status(&s, 3).unwrap();
    assert_eq!(r.rejected_deltas, 2);
    assert_eq!(r.top_rejections.len(), 1, "{:?}", r.top_rejections);
    assert_eq!(r.top_rejections[0].code, "HpAboveMax");
    assert_eq!(r.top_rejections[0].count, 2);
}

#[test]
fn top_rejections_are_ordered_most_frequent_first() {
    let s = with_kaelen();
    // Two UnknownField, one UnknownSubject.
    s.append_delta(1, &num("Kaelen", "charisma", Op::Set, 5))
        .unwrap()
        .unwrap_err();
    s.append_delta(1, &num("Kaelen", "luck", Op::Set, 5))
        .unwrap()
        .unwrap_err();
    s.append_delta(1, &num("Nobody", "hp", Op::Set, 5))
        .unwrap()
        .unwrap_err();

    let r = status::status(&s, 3).unwrap();
    assert_eq!(r.top_rejections[0].code, "UnknownField");
    assert_eq!(r.top_rejections[0].count, 2);
    assert_eq!(r.top_rejections[1].code, "UnknownSubject");
}

#[test]
fn a_rewind_does_not_inflate_the_rejection_metrics() {
    // Regenerating a stretch of story is deliberate. If rewound rows counted as
    // rejections, every rewind would fake a drift alarm.
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(2, &num("Kaelen", "hp", Op::Sub, 10))
        .unwrap()
        .unwrap();
    s.append_delta(2, &num("Kaelen", "charisma", Op::Set, 5))
        .unwrap()
        .unwrap_err();

    let before = status::status(&s, 3).unwrap();
    assert_eq!(before.rejected_deltas, 1);

    s.rewind(1).unwrap();

    let after = status::status(&s, 3).unwrap();
    assert_eq!(after.rejected_deltas, 1, "rewound rows are not rejections");
    assert_eq!(after.applied_deltas, 1);
    assert!(
        after.top_rejections.iter().all(|r| r.code != "rewound"),
        "{:?}",
        after.top_rejections
    );
}

#[test]
fn status_surfaces_dirty_chapters() {
    let s = store();
    add_chapter(&s, 1);
    s.insert_chapter(&NewChapter {
        number: 2,
        title: "Chapter 2".into(),
        text_md: "text".into(),
        prompt_hash: String::new(),
        state_dirty: true,
    })
    .unwrap();
    assert_eq!(status::status(&s, 3).unwrap().dirty_chapters, vec![2]);
}

#[test]
fn status_text_makes_the_drift_signal_visible() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "charisma", Op::Set, 1))
        .unwrap()
        .unwrap_err();

    let out = status::render_text(&status::status(&s, 3).unwrap());
    assert!(out.contains("reject rate"), "{out}");
    assert!(out.contains("UnknownField"), "{out}");
    assert!(out.contains("§6.2"), "{out}");
    assert!(out.contains("**"), "warning marker missing:\n{out}");
}

#[test]
fn status_text_is_quiet_when_the_rate_is_healthy() {
    let s = with_kaelen();
    for _ in 0..40 {
        s.append_delta(1, &num("Kaelen", "gold", Op::Add, 1))
            .unwrap()
            .unwrap();
    }
    let out = status::render_text(&status::status(&s, 3).unwrap());
    assert!(!out.contains("**"), "no warning expected:\n{out}");
}

// -------------------------------------------------------------------- state

#[test]
fn state_on_an_empty_store_has_no_subjects() {
    let r = state::state(&store(), None).unwrap();
    assert!(r.subjects.is_empty());
    assert!(r.anomalies.is_empty());
    assert!(state::render_all(&r).contains("No state"));
}

#[test]
fn state_buckets_each_field_namespace() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 82))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "max_hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "level", Op::Set, 7))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "inv:torch", Op::Set, 3))
        .unwrap()
        .unwrap();
    s.append_delta(1, &txt("Kaelen", "equip:main_hand", "Ashen Blade"))
        .unwrap()
        .unwrap();
    s.append_delta(1, &txt("Kaelen", "appear:eyes", "grey"))
        .unwrap()
        .unwrap();
    s.append_delta(1, &txt("Kaelen", "location", "the vale"))
        .unwrap()
        .unwrap();

    let r = state::state(&s, None).unwrap();
    assert_eq!(r.subjects.len(), 1);
    let v = &r.subjects[0];
    assert_eq!(v.subject, "Kaelen");
    assert_eq!(v.stats.get("hp"), Some(&82));
    assert_eq!(v.stats.get("max_hp"), Some(&100));
    assert_eq!(v.stats.get("level"), Some(&7));
    assert_eq!(v.inventory.get("torch"), Some(&3));
    assert_eq!(v.equipment.get("main_hand").unwrap(), "Ashen Blade");
    assert_eq!(v.appearance.get("eyes").unwrap(), "grey");
    assert_eq!(v.text_fields.get("location").unwrap(), "the vale");
    // Prefixed keys are stripped, never left doubled up.
    assert!(v.stats.keys().all(|k| !k.contains(':')));
    assert!(v.other.is_empty(), "{:?}", v.other);
}

#[test]
fn state_can_be_filtered_to_one_subject() {
    let s = with_kaelen();
    s.upsert_cast("Vessa", "sherpa:piper-en_GB-alba:0", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 50))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Vessa", "hp", Op::Set, 30))
        .unwrap()
        .unwrap();

    assert_eq!(state::state(&s, None).unwrap().subjects.len(), 2);
    let only = state::state(&s, Some("Vessa")).unwrap();
    assert_eq!(only.subjects.len(), 1);
    assert_eq!(only.subjects[0].subject, "Vessa");
    assert_eq!(only.subjects[0].stats.get("hp"), Some(&30));
}

#[test]
fn subject_view_lists_every_whitelisted_slot_and_trait_even_when_empty() {
    // §9.4.1: the watch's rows must not reshuffle, so the CLI mirror shows the
    // full whitelist rather than only the populated entries.
    let s = with_kaelen();
    s.append_delta(1, &txt("Kaelen", "equip:main_hand", "Ashen Blade"))
        .unwrap()
        .unwrap();

    let r = state::state(&s, Some("Kaelen")).unwrap();
    let out = state::render_subject(&r, "Kaelen");
    for slot in litrpg_core::validate::EQUIP_SLOTS {
        assert!(out.contains(slot), "missing slot {slot}:\n{out}");
    }
    for tr in litrpg_core::validate::APPEAR_TRAITS {
        assert!(out.contains(tr), "missing trait {tr}:\n{out}");
    }
    assert!(out.contains("Ashen Blade"), "{out}");
    assert!(out.contains('—'), "empty slots should be dashed:\n{out}");
}

#[test]
fn an_empty_equip_string_means_the_slot_is_empty_not_missing() {
    // Spec §6.0: empty string is a legal value meaning "slot is empty".
    let s = with_kaelen();
    s.append_delta(1, &txt("Kaelen", "equip:off_hand", ""))
        .unwrap()
        .unwrap();
    let r = state::state(&s, Some("Kaelen")).unwrap();
    assert_eq!(r.subjects[0].equipment.get("off_hand").unwrap(), "");
    assert!(state::render_subject(&r, "Kaelen").contains("off_hand"));
}

#[test]
fn state_for_an_unknown_subject_says_so_rather_than_printing_nothing() {
    let r = state::state(&with_kaelen(), Some("Nobody")).unwrap();
    assert!(r.subjects.is_empty());
    assert!(state::render_subject(&r, "Nobody").contains("Nobody"));
}

// Anomalies cannot be produced through `append_delta` — the gate rejects every
// delta that would fold into one, which is by design. So the rendering is driven
// from a hand-built report; that is the part that needs testing anyway.
fn report_with_anomaly() -> state::StateReport {
    state::StateReport {
        subjects: state::state(&with_kaelen(), None).unwrap().subjects,
        anomalies: vec!["seq 12 Kaelen.gold: undecodable op \"increment\"".to_string()],
    }
}

#[test]
fn anomalies_are_rendered_before_the_state_not_after_it() {
    // litrpg-store now reports undecodable ledger rows through this channel, so
    // it is the only way an operator learns a row is unreadable. Printed below a
    // character sheet it would be scrolled past.
    let r = report_with_anomaly();
    let out = state::render_all(&r);
    let anomaly_at = out.find("undecodable").expect("anomaly missing");
    let heading_at = out.find("No state recorded").unwrap_or(usize::MAX);
    assert!(anomaly_at < heading_at, "anomaly must come first:\n{out}");
    assert!(out.contains("ANOMALY"), "{out}");
    assert!(
        out.contains("state below is incomplete"),
        "must say the state is not trustworthy:\n{out}"
    );
}

#[test]
fn the_subject_view_also_leads_with_anomalies() {
    let mut r = report_with_anomaly();
    r.subjects = state::state(
        &{
            let s = with_kaelen();
            s.append_delta(1, &num("Kaelen", "hp", Op::Set, 50))
                .unwrap()
                .unwrap();
            s
        },
        None,
    )
    .unwrap()
    .subjects;

    let out = state::render_subject(&r, "Kaelen");
    let anomaly_at = out.find("undecodable").expect("anomaly missing");
    let subject_at = out.find("Stats").expect("stats missing");
    assert!(anomaly_at < subject_at, "anomaly must come first:\n{out}");
}

#[test]
fn anomalies_are_reported_even_for_an_unknown_subject() {
    // Worst case: you look up a character, find nothing, and the reason is that
    // their rows are undecodable. That must not print as a bare "not found".
    let r = report_with_anomaly();
    let out = state::render_subject(&r, "Nobody");
    assert!(out.contains("undecodable"), "{out}");
    assert!(out.contains("Nobody"), "{out}");
}

#[test]
fn a_clean_snapshot_prints_no_anomaly_banner() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 50))
        .unwrap()
        .unwrap();
    let r = state::state(&s, None).unwrap();
    assert!(r.anomalies.is_empty());
    assert!(!state::render_all(&r).contains("!!"));
    assert!(!state::render_subject(&r, "Kaelen").contains("!!"));
}

#[test]
fn the_anomaly_banner_is_singular_for_one_and_plural_for_many() {
    let mut r = report_with_anomaly();
    assert!(state::render_all(&r).contains("1 ANOMALY"));
    r.anomalies
        .push("seq 13 Kaelen.hp: set with no value".to_string());
    assert!(state::render_all(&r).contains("2 ANOMALIES"));
}

#[test]
fn state_overview_renders_one_line_per_subject() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 40))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "max_hp", Op::Set, 90))
        .unwrap()
        .unwrap();
    let out = state::render_all(&state::state(&s, None).unwrap());
    assert!(out.contains("Kaelen"), "{out}");
    assert!(out.contains("hp 40/90"), "{out}");
}

// ------------------------------------------------------------------- rewind

#[test]
fn rewind_plan_on_an_empty_ledger_is_a_noop() {
    let p = rewind::plan(&store(), 5).unwrap();
    assert!(p.is_noop());
    assert_eq!(p.ledger_rows, 0);
    assert!(rewind::render_plan(&p).contains("Nothing to rewind"));
}

#[test]
fn rewind_plan_reports_rows_and_chapters_without_changing_anything() {
    let s = with_kaelen();
    s.append_delta(40, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(41, &num("Kaelen", "hp", Op::Sub, 10))
        .unwrap()
        .unwrap();
    s.append_delta(42, &num("Kaelen", "gold", Op::Add, 5))
        .unwrap()
        .unwrap();

    let p = rewind::plan(&s, 40).unwrap();
    assert_eq!(p.ledger_rows, 2);
    assert_eq!(p.chapters, vec![41, 42]);
    // A plan is read-only.
    assert_eq!(s.snapshot().unwrap().num("Kaelen", "hp"), Some(90));

    let text = rewind::render_plan(&p);
    assert!(text.contains("41, 42"), "{text}");
    assert!(text.contains('2'), "{text}");
}

#[test]
fn the_plan_count_equals_what_execute_actually_changes() {
    // The confirmation prompt must not be able to describe something other than
    // what happens, so preview and mutation share a predicate.
    let s = with_kaelen();
    s.append_delta(40, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(41, &num("Kaelen", "hp", Op::Sub, 10))
        .unwrap()
        .unwrap();
    s.append_delta(41, &num("Kaelen", "charisma", Op::Set, 1))
        .unwrap()
        .unwrap_err(); // already inactive; must not be counted

    let planned = rewind::plan(&s, 40).unwrap().ledger_rows;
    let actual = rewind::execute(&s, 40).unwrap();
    assert_eq!(planned, actual);
    assert_eq!(actual, 1);
}

#[test]
fn rewind_execute_restores_the_earlier_snapshot() {
    let s = with_kaelen();
    s.append_delta(40, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(41, &num("Kaelen", "hp", Op::Sub, 60))
        .unwrap()
        .unwrap();
    assert_eq!(s.snapshot().unwrap().num("Kaelen", "hp"), Some(40));

    let rows = rewind::execute(&s, 40).unwrap();
    assert_eq!(rows, 1);
    assert_eq!(s.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
    assert!(rewind::render_done(40, rows).contains("40"));
}

#[test]
fn only_an_explicit_yes_confirms() {
    for (input, expect) in [
        ("yes\n", true),
        ("YES\n", true),
        ("  yes  \n", true),
        ("y\n", false),
        ("no\n", false),
        ("\n", false),
        ("yes please\n", false),
        ("", false), // EOF: a pipe with nothing to say is not consent
    ] {
        let mut cursor = std::io::Cursor::new(input.as_bytes());
        assert_eq!(
            rewind::confirmed(&mut cursor, false).unwrap(),
            expect,
            "input {input:?}"
        );
    }
}

#[test]
fn force_confirms_without_consuming_input() {
    let mut cursor = std::io::Cursor::new(b"no\n".as_slice());
    assert!(rewind::confirmed(&mut cursor, true).unwrap());
    assert_eq!(cursor.position(), 0, "force must not read stdin");
}

// ------------------------------------------------------------------- render

#[test]
fn render_is_a_stub_that_names_the_blocking_crate() {
    let s = store();
    add_chapter(&s, 1);
    let stub = render::render(&s, 1).unwrap();
    assert!(!stub.implemented);
    assert!(stub.chapter_exists);
    assert!(!stub.has_audio);
    assert_eq!(stub.blocked_on, "litrpg-tts");

    let out = render::render_text(&stub);
    assert!(out.contains("litrpg-tts"), "{out}");
    assert!(out.contains("not implemented"), "{out}");
}

#[test]
fn render_reports_a_missing_chapter_rather_than_pretending() {
    let stub = render::render(&store(), 400).unwrap();
    assert!(!stub.chapter_exists);
    assert!(render::render_text(&stub).contains("does not exist"));
}

#[test]
fn render_notes_when_a_chapter_already_has_audio() {
    let s = store();
    add_chapter(&s, 1);
    give_audio(&s, 1);
    let stub = render::render(&s, 1).unwrap();
    assert!(stub.has_audio);
    assert!(render::render_text(&stub).contains("replace"));
}

// --------------------------------------------------------------------- json

#[test]
fn status_state_and_cast_serialize_to_json() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 50))
        .unwrap()
        .unwrap();

    let status_json = serde_json::to_string(&status::status(&s, 3).unwrap()).unwrap();
    assert!(status_json.contains("rejection_rate"), "{status_json}");
    assert!(status_json.contains("rendered_tail"), "{status_json}");

    let state_json = serde_json::to_string(&state::state(&s, None).unwrap()).unwrap();
    assert!(state_json.contains("Kaelen"), "{state_json}");
    assert!(state_json.contains("anomalies"), "{state_json}");

    let cast_json = serde_json::to_string(&cast::list(&s).unwrap()).unwrap();
    assert!(cast_json.contains("voice_ref"), "{cast_json}");
}
