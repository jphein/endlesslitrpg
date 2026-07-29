use litrpg_cli::{CliError, cast, note, rewind, state, status};
use litrpg_core::hash::content_hash;
use litrpg_core::ledger::Op;
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_core::validate::Delta;
use litrpg_store::{NewChapter, Store};
use std::path::Path;
use tempfile::TempDir;

// ------------------------------------------------------------------ helpers

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-status-")
        .tempdir()
        .unwrap()
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
    s.attach_audio(n, &m).unwrap();
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
    assert!(cast::list(&store()).unwrap().entries.is_empty());
}

#[test]
fn cast_list_reports_entries_with_the_backend_split_out() {
    let s = with_kaelen();
    let entries = cast::list(&s).unwrap().entries;
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

    let row = &cast::list(&s).unwrap().entries[0];
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
    assert_eq!(cast::list(&s).unwrap().entries.len(), 1);
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
    assert_eq!(cast::list(&s).unwrap().entries[0].kind, "system");
}

#[test]
fn a_miscased_kind_is_canonicalised_rather_than_stored_as_given() {
    // `known_subjects` excludes narrator and SYSTEM with SQL literals
    // (`kind NOT IN ('narrator','system')`). `--kind System` stored verbatim would not
    // match, re-admitting SYSTEM as a delta subject — verified before the fix:
    // known_subjects returned {"Kaelen", "SYSTEM"}. The gate then accepts a whole stat
    // block under subject "SYSTEM" with every stage reporting success.
    let s = store();
    s.upsert_story(&litrpg_store::NewStory {
        title: "T".into(),
        protagonist: "Kaelen".into(),
        prompt_path: "prompt.md".into(),
        prompt_hash: "h".into(),
        target_words: 2000,
    })
    .unwrap();

    for given in ["System", "SYSTEM", " system ", "SyStEm"] {
        let s = store();
        s.upsert_story(&litrpg_store::NewStory {
            title: "T".into(),
            protagonist: "Kaelen".into(),
            prompt_path: "prompt.md".into(),
            prompt_hash: "h".into(),
            target_words: 2000,
        })
        .unwrap();
        cast::set(&s, "SYSTEM", "sherpa:x:0", true, Some(given)).unwrap();
        assert_eq!(
            cast::list(&s).unwrap().entries[0].kind,
            "system",
            "{given:?} should canonicalise"
        );
        assert!(
            !s.known_subjects().unwrap().contains("SYSTEM"),
            "{given:?} let SYSTEM become a delta subject"
        );
    }
}

#[test]
fn a_misspelt_kind_is_refused_before_anything_is_written() {
    // A typo must not silently become a new kind that matches none of the store's
    // filters. `narrater` previously admitted the narrator as a delta subject.
    let s = store();
    // Note "Character " is absent: input is trimmed and lower-cased by design, so it
    // canonicalises rather than failing. Only values outside core's enum are refused.
    for bad in [
        "narrater",
        "sytsem",
        "person",
        "",
        "narrator character",
        "syst em",
    ] {
        let err = cast::set(&s, "Someone", "sherpa:x:0", true, Some(bad)).unwrap_err();
        assert!(
            matches!(err, CliError::UnknownKind { .. }),
            "{bad:?} should be refused, got {err:?}"
        );
        assert!(
            cast::list(&s).unwrap().entries.is_empty(),
            "{bad:?} must not have written a row"
        );
    }
}

#[test]
fn the_refusal_names_the_kinds_it_accepts() {
    let s = store();
    let err = cast::set(&s, "Someone", "sherpa:x:0", true, Some("person")).unwrap_err();
    let msg = err.to_string();
    for k in ["narrator", "character", "system"] {
        assert!(msg.contains(k), "{k} missing from: {msg}");
    }
    assert!(msg.contains("own stats"), "must say why it matters:\n{msg}");
}

#[test]
fn the_default_kind_is_cores_canonical_form() {
    // Derived from `SpeakerKind`, not written out as a fifth copy of the list.
    assert_eq!(cast::default_kind(), "character");
    let s = store();
    cast::set(&s, "Vessa", "sherpa:x:0", true, None).unwrap();
    assert_eq!(cast::list(&s).unwrap().entries[0].kind, "character");
}

#[test]
fn a_narrator_cast_row_is_still_excluded_from_delta_subjects() {
    // The other half of the same filter: narrator gets a cast row because it needs a
    // voice, but it is not a person who can own stats.
    let s = store();
    s.upsert_story(&litrpg_store::NewStory {
        title: "T".into(),
        protagonist: "Kaelen".into(),
        prompt_path: "prompt.md".into(),
        prompt_hash: "h".into(),
        target_words: 2000,
    })
    .unwrap();
    cast::set(&s, "narrator", "sherpa:x:0", true, Some("Narrator")).unwrap();
    let known = s.known_subjects().unwrap();
    assert!(!known.contains("narrator"), "{known:?}");
    assert!(
        known.contains("Kaelen"),
        "the protagonist must still be known"
    );
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
        cast::list(&s).unwrap().entries[0].voice_ref,
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
    let row = &cast::list(&s).unwrap().entries[0];
    assert_eq!(row.voice_ref, "azure:en-GB-Ada:DragonHDLatestNeural");
    assert_eq!(row.backend.as_deref(), Some("azure"));
}

// ------------------------------------------------------------------- status

#[test]
fn status_on_an_empty_store_is_all_zeros_and_quiet() {
    let r = status::status(&store(), 3, Path::new("/nonexistent")).unwrap();
    assert_eq!(r.latest_chapter, 0);
    assert_eq!(r.total_chapters, 0);
    assert_eq!(r.chapters_with_audio, 0);
    assert_eq!(r.consumed_through, 0);
    assert_eq!(r.playable_ahead, 0);
    assert_eq!(r.chapters_ahead, 0);
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

    let r = status::status(&s, 2, Path::new("/nonexistent")).unwrap();
    assert_eq!(r.latest_chapter, 4);
    assert_eq!(r.total_chapters, 4);
    assert_eq!(r.chapters_with_audio, 3);
    // Cursor is 0, so the playable run starts at chapter 1: only chapter 1 is
    // rendered before the gap at 2, even though 3 and 4 are rendered too.
    assert_eq!(r.consumed_through, 0);
    assert_eq!(r.playable_ahead, 1);
    assert_eq!(r.chapters_ahead, 3);
    assert!(!r.buffer_ok);
}

#[test]
fn the_playable_run_is_measured_forwards_from_the_cursor() {
    let s = store();
    for n in 1..=3 {
        add_chapter(&s, n);
    }
    // 1 and 2 rendered, 3 not. From a cursor of 0 the listener can play two
    // chapters and then stalls.
    give_audio(&s, 1);
    give_audio(&s, 2);
    let r = status::status(&s, 2, Path::new("/nonexistent")).unwrap();
    assert_eq!(r.chapters_with_audio, 2);
    assert_eq!(r.playable_ahead, 2);
    assert!(r.buffer_ok);
}

#[test]
fn status_reports_buffer_against_the_configured_target() {
    let s = store();
    for n in 1..=2 {
        add_chapter(&s, n);
        give_audio(&s, n);
    }
    assert!(
        status::status(&s, 2, Path::new("/nonexistent"))
            .unwrap()
            .buffer_ok
    );
    assert!(
        !status::status(&s, 3, Path::new("/nonexistent"))
            .unwrap()
            .buffer_ok
    );
    assert_eq!(
        status::status(&s, 3, Path::new("/nonexistent"))
            .unwrap()
            .playable_ahead,
        2
    );
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

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
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

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
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

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
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

    let before = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    assert_eq!(before.rejected_deltas, 1);

    s.rewind(1).unwrap();

    let after = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
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
    assert_eq!(
        status::status(&s, 3, Path::new("/nonexistent"))
            .unwrap()
            .dirty_chapters,
        vec![2]
    );
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

    let out = status::render_text(&status::status(&s, 3, Path::new("/nonexistent")).unwrap());
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
    let out = status::render_text(&status::status(&s, 3, Path::new("/nonexistent")).unwrap());
    assert!(!out.contains("**"), "no warning expected:\n{out}");
}

// ------------------------------------------------------- prompt sync

fn story_with_prompt_rel(s: &Store, rel: &str, hash: &str) {
    s.upsert_story(&litrpg_store::NewStory {
        title: "The Ashen Vale".into(),
        protagonist: "Kaelen".into(),
        prompt_path: rel.into(),
        prompt_hash: hash.into(),
        target_words: 2000,
    })
    .unwrap();
}

fn story_with_prompt(s: &Store, path: &std::path::Path, hash: &str) {
    s.upsert_story(&litrpg_store::NewStory {
        title: "The Ashen Vale".into(),
        protagonist: "Kaelen".into(),
        prompt_path: path.display().to_string(),
        prompt_hash: hash.into(),
        target_words: 2000,
    })
    .unwrap();
}

#[test]
fn no_story_row_reports_not_initialised() {
    let r = status::status(&store(), 3, Path::new("/nonexistent")).unwrap();
    assert_eq!(r.prompt, status::PromptSync::NotInitialised);
    assert!(!r.prompt_edit_pending);
    let out = status::render_text(&r);
    assert!(out.contains("Not initialised"), "{out}");
    assert!(out.contains("litrpg init"), "must say what to run:\n{out}");
}

#[test]
fn a_prompt_matching_the_row_says_nothing_at_all() {
    // A status command that reassures you about every subsystem trains you to stop
    // reading it.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "# A premise\n").unwrap();
    let s = store();
    story_with_prompt(&s, &path, &content_hash("# A premise\n"));

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    assert!(matches!(r.prompt, status::PromptSync::InSync { .. }));
    assert!(!r.prompt_edit_pending);
    let out = status::render_text(&r);
    assert!(!out.contains("Prompt"), "should be silent:\n{out}");
    assert!(!out.contains("pending"), "{out}");
}

#[test]
fn an_edited_prompt_is_reported_as_pending_with_both_hashes() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let s = store();
    // Row records the old prompt; the file has since moved on.
    story_with_prompt(&s, &path, &content_hash("# Old premise\n"));
    std::fs::write(&path, "# New premise\n").unwrap();

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    assert!(r.prompt_edit_pending);
    match &r.prompt {
        status::PromptSync::Pending {
            in_effect,
            on_disk,
            path: p,
        } => {
            assert_eq!(in_effect, &content_hash("# Old premise\n"));
            assert_eq!(on_disk, &content_hash("# New premise\n"));
            assert_eq!(p, &path);
        }
        other => panic!("expected Pending, got {other:?}"),
    }

    let out = status::render_text(&r);
    assert!(out.contains("Prompt edit pending"), "{out}");
    assert!(
        out.contains("next\n chapter boundary") || out.contains("next"),
        "{out}"
    );
    assert!(out.contains("§9.3"), "must cite why it lags:\n{out}");
    assert!(out.contains(&content_hash("# New premise\n")), "{out}");
    assert!(out.contains(&content_hash("# Old premise\n")), "{out}");
}

#[test]
fn a_missing_prompt_file_is_named_not_treated_as_in_sync() {
    // The file of record is gone but chapters keep generating from the in-effect
    // prompt. Silently reporting "in sync" would hide a real problem.
    let dir = tmp();
    let path = dir.path().join("gone.md");
    let s = store();
    story_with_prompt(&s, &path, &content_hash("# Old premise\n"));

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    assert!(!r.prompt_edit_pending, "missing is not a pending edit");
    match &r.prompt {
        status::PromptSync::Missing { path: p, in_effect } => {
            assert_eq!(p, &path);
            assert_eq!(in_effect, &content_hash("# Old premise\n"));
        }
        other => panic!("expected Missing, got {other:?}"),
    }
    let out = status::render_text(&r);
    assert!(out.contains("missing from disk"), "{out}");
    assert!(out.contains("gone.md"), "must name the path:\n{out}");
    assert!(out.contains("git"), "must say how to recover:\n{out}");
}

#[test]
fn an_empty_in_effect_hash_still_compares_rather_than_crashing() {
    // A story row written before any prompt existed has prompt_hash = ''. That must
    // read as "pending", not as a match.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "# A premise\n").unwrap();
    let s = store();
    story_with_prompt(&s, &path, "");

    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    assert!(r.prompt_edit_pending);
}

#[test]
fn a_relative_prompt_path_is_joined_against_story_dir() {
    // Migration 004 made `story.prompt_path` relative to `story_dir`. The old version
    // of this test used a decoy file at another path to prove the row's location won
    // over the config's; that premise is gone, because a relative basename cannot
    // point outside the configured story dir. The staleness became unrepresentable,
    // which is better than detecting it — so what is asserted now is the join.
    let dir = tmp();
    std::fs::write(dir.path().join("prompt.md"), "# Recorded\n").unwrap();

    let s = store();
    story_with_prompt_rel(&s, "prompt.md", &content_hash("# Recorded\n"));
    assert!(
        matches!(
            status::status(&s, 3, dir.path()).unwrap().prompt,
            status::PromptSync::InSync { .. }
        ),
        "a bare basename must resolve inside story_dir"
    );

    // And a different story_dir looks for it there, not where it used to be.
    let elsewhere = tmp();
    assert!(matches!(
        status::status(&s, 3, elsewhere.path()).unwrap().prompt,
        status::PromptSync::Missing { .. }
    ));
}

#[test]
fn an_absolute_prompt_path_is_honoured_rather_than_mangled() {
    // Resolution reuses `litrpg_config::resolve_path`, so a row still holding an
    // absolute path from before migration 004 keeps working instead of being joined
    // into `<story_dir>/home/jp/...`. That is what stops the migration stranding a
    // database, and it preserves pointing the prompt somewhere else deliberately.
    let elsewhere = tmp();
    let abs = elsewhere.path().join("my-prompt.md");
    std::fs::write(&abs, "# Absolute\n").unwrap();

    let s = store();
    story_with_prompt(&s, &abs, &content_hash("# Absolute\n"));

    let story_dir = tmp();
    assert!(
        matches!(
            status::status(&s, 3, story_dir.path()).unwrap().prompt,
            status::PromptSync::InSync { .. }
        ),
        "absolute must be used verbatim"
    );
}

#[test]
fn a_relative_prompt_path_that_is_missing_names_the_resolved_location() {
    // The reported path must be where it actually looked, not the bare basename —
    // otherwise the operator goes hunting for "prompt.md" with no directory.
    let dir = tmp();
    let s = store();
    story_with_prompt_rel(&s, "prompt.md", "fnv1a64:0000000000000000");
    match status::status(&s, 3, dir.path()).unwrap().prompt {
        status::PromptSync::Missing { path, .. } => {
            assert_eq!(path, dir.path().join("prompt.md"));
        }
        other => panic!("expected Missing, got {other:?}"),
    }
}

#[test]
fn prompt_sync_is_exposed_in_json_as_a_boolean_and_both_hashes() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let s = store();
    story_with_prompt(&s, &path, &content_hash("# Old\n"));
    std::fs::write(&path, "# New\n").unwrap();

    let json =
        serde_json::to_string(&status::status(&s, 3, Path::new("/nonexistent")).unwrap()).unwrap();
    assert!(json.contains("\"prompt_edit_pending\":true"), "{json}");
    assert!(json.contains("\"state\":\"pending\""), "{json}");
    assert!(json.contains(&content_hash("# Old\n")), "{json}");
    assert!(json.contains(&content_hash("# New\n")), "{json}");
}

#[test]
fn json_reports_not_initialised_and_missing_distinctly() {
    let empty =
        serde_json::to_string(&status::status(&store(), 3, Path::new("/nonexistent")).unwrap())
            .unwrap();
    assert!(empty.contains("\"state\":\"not_initialised\""), "{empty}");
    assert!(empty.contains("\"prompt_edit_pending\":false"), "{empty}");

    let dir = tmp();
    let s = store();
    story_with_prompt(&s, &dir.path().join("gone.md"), "fnv1a64:0000000000000000");
    let missing =
        serde_json::to_string(&status::status(&s, 3, Path::new("/nonexistent")).unwrap()).unwrap();
    assert!(missing.contains("\"state\":\"missing\""), "{missing}");
    assert!(
        missing.contains("\"prompt_edit_pending\":false"),
        "{missing}"
    );
}

#[test]
fn a_pending_prompt_does_not_suppress_the_drift_signal() {
    // Two independent concerns; one must not hide the other.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    let s = with_kaelen();
    story_with_prompt(&s, &path, &content_hash("# Old\n"));
    std::fs::write(&path, "# New\n").unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 100))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen", "charisma", Op::Set, 1))
        .unwrap()
        .unwrap_err();

    let out = status::render_text(&status::status(&s, 3, Path::new("/nonexistent")).unwrap());
    assert!(out.contains("Prompt edit pending"), "{out}");
    assert!(out.contains("reject rate"), "{out}");
    assert!(out.contains("UnknownField"), "{out}");
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
        possible_aliases: Vec::new(),
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

// NOTE: the `render` stub tests that lived here are gone — `render` is no longer a
// stub, and its behaviour is covered in tests/render.rs. Removed rather than adapted:
// they asserted "prints that it is not implemented", which is the opposite of the
// current contract.

// --------------------------------------------------------------------- json

#[test]
fn status_state_and_cast_serialize_to_json() {
    let s = with_kaelen();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 50))
        .unwrap()
        .unwrap();

    let status_json =
        serde_json::to_string(&status::status(&s, 3, Path::new("/nonexistent")).unwrap()).unwrap();
    assert!(status_json.contains("rejection_rate"), "{status_json}");
    assert!(status_json.contains("playable_ahead"), "{status_json}");
    assert!(status_json.contains("consumed_through"), "{status_json}");

    let state_json = serde_json::to_string(&state::state(&s, None).unwrap()).unwrap();
    assert!(state_json.contains("Kaelen"), "{state_json}");
    assert!(state_json.contains("anomalies"), "{state_json}");

    let cast_json = serde_json::to_string(&cast::list(&s).unwrap()).unwrap();
    assert!(cast_json.contains("voice_ref"), "{cast_json}");
}

// -------------------------------------------- observed voice substitution

fn render_speaker(s: &Store, chapter_no: u32, speaker: &str, voice: &str) {
    add_chapter(s, chapter_no);
    let m = Manifest::new(
        chapter_no,
        vec![Segment {
            idx: 0,
            speaker: speaker.into(),
            kind: SpeakerKind::Character,
            voice_ref: voice.into(),
            text: "Line.".into(),
            start_ms: 0,
            end_ms: 1000,
        }],
    );
    s.attach_audio(chapter_no, &m).unwrap();
}

#[test]
fn a_cast_row_matching_the_rendered_audio_is_not_flagged() {
    let s = with_kaelen();
    render_speaker(&s, 1, "Kaelen", "sherpa:kokoro-multi-lang-v1_0:18");
    let listing = cast::list(&s).unwrap();
    assert_eq!(listing.entries[0].rendered_as, None);
    assert_eq!(listing.substituted().count(), 0);
    let out = cast::render_list(&listing);
    assert!(!out.contains("!!"), "{out}");
}

#[test]
fn a_substituted_voice_is_flagged_with_what_was_actually_used() {
    // An Azure-only build substitutes at render time without rewriting the cast row,
    // so `litrpg cast` shows sherpa while the audio is Azure. Without this, comparing
    // the table against a manifest reads as a bug.
    let s = with_kaelen();
    render_speaker(&s, 1, "Kaelen", "azure:en-GB-Ada:DragonHDLatestNeural");

    let listing = cast::list(&s).unwrap();
    assert_eq!(
        listing.entries[0].rendered_as.as_deref(),
        Some("azure:en-GB-Ada:DragonHDLatestNeural")
    );
    assert_eq!(listing.substituted().count(), 1);

    let out = cast::render_list(&listing);
    assert!(out.contains("!!"), "{out}");
    assert!(
        out.contains("rendered as azure:en-GB-Ada:DragonHDLatestNeural"),
        "{out}"
    );
    assert!(
        out.contains("story's intent"),
        "must say which one is authoritative:\n{out}"
    );
}

#[test]
fn the_most_recent_render_wins_when_the_voice_changed_over_time() {
    let s = with_kaelen();
    render_speaker(&s, 1, "Kaelen", "azure:old-voice:0");
    render_speaker(&s, 2, "Kaelen", "azure:new-voice:0");
    assert_eq!(
        cast::list(&s).unwrap().entries[0].rendered_as.as_deref(),
        Some("azure:new-voice:0")
    );
}

#[test]
fn a_speaker_never_rendered_makes_no_claim_either_way() {
    // Silence is not evidence of agreement.
    let s = with_kaelen();
    add_chapter(&s, 1); // text only, no audio
    let listing = cast::list(&s).unwrap();
    assert_eq!(listing.entries[0].rendered_as, None);
    assert!(listing.scanned.is_empty(), "{:?}", listing.scanned);
}

#[test]
fn only_rendered_chapters_are_scanned() {
    let s = with_kaelen();
    add_chapter(&s, 1);
    render_speaker(&s, 2, "Kaelen", "azure:x:0");
    add_chapter(&s, 3);
    let listing = cast::list(&s).unwrap();
    assert_eq!(listing.scanned, vec![2]);
}

#[test]
fn the_scan_window_is_bounded_and_newest_first() {
    // One query per chapter, so an endless story must not make `litrpg cast` slow.
    let s = with_kaelen();
    for n in 1..=(cast::SUBSTITUTION_SCAN_LIMIT as u32 + 5) {
        render_speaker(&s, n, "Kaelen", "azure:x:0");
    }
    let listing = cast::list(&s).unwrap();
    assert_eq!(listing.scanned.len(), cast::SUBSTITUTION_SCAN_LIMIT);
    let newest = cast::SUBSTITUTION_SCAN_LIMIT as u32 + 5;
    assert_eq!(listing.scanned[0], newest, "newest first");
    assert!(!listing.scanned.contains(&1), "oldest must fall outside");
}

#[test]
fn substitution_is_exposed_in_json() {
    let s = with_kaelen();
    render_speaker(&s, 1, "Kaelen", "azure:en-GB-Ada:DragonHDLatestNeural");
    let json = serde_json::to_string(&cast::list(&s).unwrap()).unwrap();
    assert!(json.contains("rendered_as"), "{json}");
    assert!(
        json.contains("azure:en-GB-Ada:DragonHDLatestNeural"),
        "{json}"
    );
    assert!(json.contains("\"scanned\":[1]"), "{json}");
}

// ------------------------------------------------- split-identity notice

#[test]
fn state_flags_a_character_recorded_under_two_names() {
    // The live collision: `Kaelen` and `Kaelen Vord` both accrued stats, so neither
    // character sheet is complete and the ledger cannot be merged retrospectively.
    let s = store();
    s.upsert_cast("Kaelen", "sherpa:x:0", "character", 1)
        .unwrap();
    s.upsert_cast("Kaelen Vord", "sherpa:y:0", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 80))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen Vord", "gold", Op::Set, 12))
        .unwrap()
        .unwrap();

    let r = state::state(&s, None).unwrap();
    assert_eq!(
        r.possible_aliases,
        vec![("Kaelen".to_string(), "Kaelen Vord".to_string())]
    );

    let out = state::render_all(&r);
    assert!(
        out.contains("one character recorded under two names"),
        "{out}"
    );
    assert!(
        out.contains("append-only"),
        "must say it cannot be merged:\n{out}"
    );
    assert!(
        out.contains("prompt.md"),
        "must say how to stop it recurring:\n{out}"
    );
}

#[test]
fn the_split_identity_notice_shows_even_when_filtered_to_one_subject() {
    // Asking for one character must still reveal that their stats are split — that view
    // is exactly where a half-empty sheet is noticed.
    let s = store();
    s.upsert_cast("Kaelen", "sherpa:x:0", "character", 1)
        .unwrap();
    s.upsert_cast("Kaelen Vord", "sherpa:y:0", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 80))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen Vord", "gold", Op::Set, 12))
        .unwrap()
        .unwrap();

    let r = state::state(&s, Some("Kaelen")).unwrap();
    assert_eq!(r.subjects.len(), 1);
    assert!(!r.possible_aliases.is_empty(), "must not be filtered away");
    assert!(state::render_subject(&r, "Kaelen").contains("two names"));
}

#[test]
fn distinct_characters_produce_no_split_identity_notice() {
    let s = with_kaelen();
    s.upsert_cast("Vessa", "sherpa:y:0", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 80))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Vessa", "hp", Op::Set, 40))
        .unwrap()
        .unwrap();
    let r = state::state(&s, None).unwrap();
    assert!(r.possible_aliases.is_empty(), "{:?}", r.possible_aliases);
    assert!(!state::render_all(&r).contains("two names"));
}

#[test]
fn the_split_identity_notice_is_in_the_json() {
    let s = store();
    s.upsert_cast("Kaelen", "sherpa:x:0", "character", 1)
        .unwrap();
    s.upsert_cast("Kaelen Vord", "sherpa:y:0", "character", 1)
        .unwrap();
    s.append_delta(1, &num("Kaelen", "hp", Op::Set, 80))
        .unwrap()
        .unwrap();
    s.append_delta(1, &num("Kaelen Vord", "gold", Op::Set, 12))
        .unwrap()
        .unwrap();
    let json = serde_json::to_string(&state::state(&s, None).unwrap()).unwrap();
    assert!(json.contains("possible_aliases"), "{json}");
    assert!(json.contains("Kaelen Vord"), "{json}");
}

#[test]
fn status_rechecks_the_protagonist_against_an_edited_prompt() {
    // The mismatch can be introduced long after init by editing prompt.md, and status
    // is where a pending prompt edit already surfaces.
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "# The Vale\n\nKaelen Vord returns.\n").unwrap();

    let s = store();
    s.upsert_story(&litrpg_store::NewStory {
        title: "T".into(),
        protagonist: "Kaelen".into(),
        prompt_path: "prompt.md".into(),
        prompt_hash: content_hash("# The Vale\n\nKaelen Vord returns.\n"),
        target_words: 2000,
    })
    .unwrap();

    let r = status::status(&s, 3, dir.path()).unwrap();
    assert!(
        r.protagonist_check.is_warning(),
        "{:?}",
        r.protagonist_check
    );
    let out = status::render_text(&r);
    assert!(out.contains("Kaelen Vord"), "{out}");
}

#[test]
fn status_is_quiet_when_the_prompt_names_the_protagonist() {
    let dir = tmp();
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, "# The Vale\n\nKaelen returns.\n").unwrap();
    let s = store();
    s.upsert_story(&litrpg_store::NewStory {
        title: "T".into(),
        protagonist: "Kaelen".into(),
        prompt_path: "prompt.md".into(),
        prompt_hash: content_hash("# The Vale\n\nKaelen returns.\n"),
        target_words: 2000,
    })
    .unwrap();
    let r = status::status(&s, 3, dir.path()).unwrap();
    assert!(!r.protagonist_check.is_warning());
    assert!(!status::render_text(&r).contains("stat changes"));
}
