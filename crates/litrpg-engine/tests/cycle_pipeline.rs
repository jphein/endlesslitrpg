//! The cycle end-to-end against fakes: ordering, degradations, and resume.
//!
//! These are the tests that matter most in the whole crate, because almost every rule
//! they check fails *silently* in production — a rejected stat that nobody sees, a note
//! applied twice, narration in the robot's voice, a manifest that disagrees with the audio
//! by a few milliseconds.

mod support;

use litrpg_ember::EmberError;
use litrpg_engine::{CycleOutcome, Engine, EngineConfig, SYSTEM_VOICE};
use support::*;

fn config() -> EngineConfig {
    EngineConfig {
        buffer_target: 3,
        target_words: 2000,
        narrator_voice: "sherpa:piper-en_GB-cori:0".to_string(),
        summary_window: 5,
    }
}

/// The standard rig: fresh in-memory store, scripted generator, silent renderer.
fn engine(
    generator: FakeGenerator,
    renderer: FakeRenderer,
    library: FakeLibrary,
    artifacts: FakeArtifacts,
) -> Engine<FakeGenerator, FakeRenderer, FakeLibrary, FakeArtifacts> {
    Engine::new(store(), generator, renderer, library, artifacts, config())
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_first_cycle_produces_chapter_one_with_audio() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            chapter,
            has_audio,
            state_dirty,
            ..
        } => {
            assert_eq!(chapter, 1);
            assert!(has_audio);
            assert!(!state_dirty);
        }
        other => panic!("expected Produced, got {other:?}"),
    }
}

#[tokio::test]
async fn chapter_numbers_advance_one_at_a_time() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    for expected in 1..=3u32 {
        assert_eq!(
            e.run_cycle(u32::MAX).await.unwrap().produced_chapter(),
            Some(expected)
        );
    }
}

#[tokio::test]
async fn all_four_artifacts_are_written() {
    let artifacts = FakeArtifacts::new();
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        artifacts,
    );
    e.run_cycle(0).await.unwrap();

    let kinds = e.artifacts_kinds();
    for want in ["pcm", "mp3", "manifest"] {
        assert!(
            kinds.contains(&want.to_string()),
            "missing {want} in {kinds:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Step 1 — the buffer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_cycle_idles_once_the_buffer_is_full() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    for _ in 0..3 {
        assert!(e.run_cycle(0).await.unwrap().produced_chapter().is_some());
    }

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Idle { buffer_depth } => assert_eq!(buffer_depth, 3),
        other => panic!("expected Idle at buffer_target 3, got {other:?}"),
    }
    assert_eq!(
        e.generator().pass1_count(),
        3,
        "an idle cycle must not call Ember"
    );
}

#[tokio::test]
async fn a_consumed_chapter_frees_buffer_space() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    for _ in 0..3 {
        e.run_cycle(0).await.unwrap();
    }
    assert!(matches!(
        e.run_cycle(0).await.unwrap(),
        CycleOutcome::Idle { .. }
    ));

    // The listener finished chapter 1, so only 2 remain ahead of them.
    assert!(
        e.run_cycle(1).await.unwrap().produced_chapter().is_some(),
        "consuming a chapter should let production resume"
    );
}

// ---------------------------------------------------------------------------
// Step 6 before step 7 — the ordering that silently loses every new character's stats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn new_lore_is_applied_before_the_deltas_so_a_new_character_is_a_known_subject() {
    // Ilex exists nowhere: not in the cast (she has no dialogue), not in the ledger.
    // Her stats are only acceptable because her `new_lore` row of kind `character` is
    // written first. Reverse the order and this delta is rejected as UnknownSubject.
    let extraction = extraction_with(
        vec![delta("Ilex", "level", "set", Some(3))],
        vec![lore_row("Ilex", "character", "ilex")],
    );

    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            applied, rejected, ..
        } => {
            assert_eq!(
                applied, 1,
                "Ilex's stats must be accepted; new_lore has to land before the deltas"
            );
            assert_eq!(rejected, 0);
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    assert_eq!(e.snapshot_num("Ilex", "level"), Some(3));
}

#[tokio::test]
async fn a_subject_that_was_never_introduced_is_still_rejected() {
    // The counterpart to the test above: the gate is real, and it is the lore ordering
    // that saves a legitimate new character -- not a weakened gate.
    let extraction = extraction_with(vec![delta("Nobody", "level", "set", Some(3))], vec![]);
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            applied, rejected, ..
        } => {
            assert_eq!(applied, 0);
            assert_eq!(rejected, 1);
        }
        other => panic!("expected Produced, got {other:?}"),
    }
    assert_eq!(e.snapshot_num("Nobody", "level"), None);
}

#[tokio::test]
async fn a_speaking_character_becomes_a_known_subject_via_the_cast() {
    // Kaelen speaks in DEFAULT_PROSE, so step 4 gives him a cast row, which makes his
    // stats acceptable without any lore entry at all.
    let extraction = extraction_with(vec![delta("Kaelen", "xp", "add", Some(150))], vec![]);
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced { applied, .. } => assert_eq!(applied, 1),
        other => panic!("expected Produced, got {other:?}"),
    }
    assert_eq!(e.snapshot_num("Kaelen", "xp"), Some(150));
}

// ---------------------------------------------------------------------------
// Step 7 — a rejection is recorded, never fatal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_rejected_delta_does_not_abort_the_chapter() {
    // `mana` is not a whitelisted field -- and Ember really does emit `Mana: 45/100`.
    let extraction = extraction_with(
        vec![
            delta("Kaelen", "xp", "add", Some(150)),
            delta("Kaelen", "mana", "set", Some(45)),
            delta("Kaelen", "gold", "add", Some(12)),
        ],
        vec![],
    );
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            chapter,
            applied,
            rejected,
            has_audio,
            state_dirty,
        } => {
            assert_eq!(chapter, 1);
            assert_eq!(applied, 2, "the good deltas must still apply");
            assert_eq!(rejected, 1);
            assert!(has_audio, "a rejected delta must not cost the audio");
            assert!(!state_dirty, "extraction succeeded; only the gate said no");
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    assert_eq!(e.snapshot_num("Kaelen", "gold"), Some(12));
    assert_eq!(
        e.rejected_count(),
        1,
        "the rejection is stored for the §6.2 audit trail"
    );
}

#[tokio::test]
async fn a_delta_with_an_illegal_op_is_counted_not_crashed_on() {
    let extraction = extraction_with(vec![delta("Kaelen", "xp", "multiply", Some(2))], vec![]);
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            rejected,
            has_audio,
            ..
        } => {
            assert_eq!(rejected, 1);
            assert!(has_audio);
        }
        other => panic!("expected Produced, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Step 5 — the state_dirty path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_extraction_still_ships_the_chapter_as_state_dirty() {
    let e = engine(
        FakeGenerator::new().with_pass2_always_malformed(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            chapter,
            state_dirty,
            has_audio,
            applied,
            rejected,
        } => {
            assert_eq!(chapter, 1);
            assert!(state_dirty, "spec §10: pass 2 schema failure ships anyway");
            assert!(has_audio, "the audio has nothing to do with the extraction");
            assert_eq!((applied, rejected), (0, 0));
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    assert_eq!(
        e.dirty_chapters(),
        vec![1],
        "the chapter is queued for re-extraction"
    );
    assert!(
        !e.chapter_text(1).is_empty(),
        "the prose must have been kept"
    );
    assert!(
        e.summaries_written().is_empty(),
        "there was no summary to write"
    );
}

#[tokio::test]
async fn extraction_is_retried_with_jitter_before_giving_up() {
    let e = engine(
        FakeGenerator::new().with_pass2_always_malformed(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();
    assert_eq!(
        e.generator().pass2_count(),
        litrpg_engine::PASS2_TEMPERATURES.len(),
        "every jittered attempt should be used before shipping state_dirty"
    );
}

#[tokio::test]
async fn a_transport_failure_on_extraction_gives_up_immediately() {
    // Backing off is the caller's job; burning two more round trips against a dead
    // socket just delays the chapter that is already written.
    let e = engine(
        FakeGenerator::new().push_pass2(Err(EmberError::Transport {
            detail: "connection refused".into(),
        })),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    let out = e.run_cycle(0).await.unwrap();
    assert!(matches!(
        out,
        CycleOutcome::Produced {
            state_dirty: true,
            ..
        }
    ));
    assert_eq!(
        e.generator().pass2_count(),
        1,
        "no retries on a transport failure"
    );
}

#[tokio::test]
async fn a_successful_extraction_writes_the_summary() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();
    let written = e.summaries_written();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].0, 1);
    assert_eq!(written[0].1, "Kaelen broke the first seal.");
}

// ---------------------------------------------------------------------------
// Step 3 — pass 1 failure never leaves a partial chapter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_transport_failure_on_pass_1_abandons_the_cycle_and_asks_for_backoff() {
    let e = engine(
        FakeGenerator::new().push_pass1(Err(EmberError::Transport {
            detail: "connection refused".into(),
        })),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    let out = e.run_cycle(0).await.unwrap();
    match &out {
        CycleOutcome::Abandoned {
            chapter, backoff, ..
        } => {
            assert_eq!(*chapter, 1);
            assert!(*backoff, "the caller must back off, not re-prompt");
        }
        other => panic!("expected Abandoned, got {other:?}"),
    }
    assert!(out.should_backoff());
    assert_eq!(
        e.generator().pass1_count(),
        1,
        "no retries against a dead socket"
    );
    assert_eq!(e.latest_number(), 0, "spec §10: no partial chapters");
    assert_eq!(e.generator().pass2_count(), 0);
}

#[tokio::test]
async fn a_four_hundred_on_pass_1_is_not_retried() {
    let e = engine(
        FakeGenerator::new().push_pass1(Err(EmberError::Status {
            status: 400,
            body: "JSON schema conversion failed".into(),
        })),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Abandoned { backoff, .. } => {
            assert!(
                !backoff,
                "our own bad request will not fix itself; do not back off"
            )
        }
        other => panic!("expected Abandoned, got {other:?}"),
    }
    assert_eq!(e.generator().pass1_count(), 1);
    assert_eq!(e.latest_number(), 0);
}

#[tokio::test]
async fn a_malformed_pass_1_is_retried_with_rising_temperature_then_succeeds() {
    let e = engine(
        FakeGenerator::new()
            .push_pass1(Err(EmberError::Malformed {
                body: String::new(),
                detail: "empty".into(),
            }))
            .push_pass1(Ok(DEFAULT_PROSE.to_string())),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    assert_eq!(e.run_cycle(0).await.unwrap().produced_chapter(), Some(1));
    let temps = e.generator().pass1_temperatures();
    assert_eq!(temps.len(), 2);
    assert!(
        temps[1] > temps[0],
        "the retry must jitter upward, got {temps:?}"
    );
    assert_eq!(temps, litrpg_engine::PASS1_TEMPERATURES[..2].to_vec());
}

#[tokio::test]
async fn empty_prose_exhausts_the_retries_and_then_abandons() {
    let e = engine(
        FakeGenerator::new().with_prose("   \n\n  "),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Abandoned { backoff, .. } => assert!(!backoff),
        other => panic!("expected Abandoned, got {other:?}"),
    }
    assert_eq!(
        e.generator().pass1_count(),
        litrpg_engine::PASS1_TEMPERATURES.len()
    );
    assert_eq!(e.latest_number(), 0);
}

// ---------------------------------------------------------------------------
// Step 8/9 — TTS failure ships the text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tts_failure_still_ships_the_text() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced {
            chapter,
            has_audio,
            state_dirty,
            applied,
            ..
        } => {
            assert_eq!(chapter, 1);
            assert!(!has_audio, "spec §10: text ships, has_audio = false");
            assert!(
                !state_dirty,
                "the extraction was fine; only the audio failed"
            );
            assert_eq!(applied, 0);
        }
        other => panic!("expected Produced, got {other:?}"),
    }
    assert!(!e.chapter_text(1).is_empty());
    assert!(!e.has_audio(1));
}

#[tokio::test]
async fn an_artifact_write_failure_still_ships_the_text() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::failing_on("mp3"),
    );

    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced { has_audio, .. } => assert!(!has_audio),
        other => panic!("expected Produced, got {other:?}"),
    }
    assert!(!e.chapter_text(1).is_empty());
    assert!(
        !e.has_audio(1),
        "audio must not be attached when the mp3 never landed"
    );
}

#[tokio::test]
async fn a_renderer_returning_the_wrong_number_of_buffers_does_not_attach_audio() {
    // A short buffer list would otherwise shift every later segment's offsets, and the
    // manifest would lie about where each voice starts.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::short_by_one(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::Produced { has_audio, .. } => assert!(!has_audio),
        other => panic!("expected Produced, got {other:?}"),
    }
    assert!(!e.has_audio(1));
}

// ---------------------------------------------------------------------------
// Step 0 — idempotent resume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_chapter_left_without_audio_is_re_rendered_not_regenerated() {
    // Cycle 1: TTS fails, so chapter 1 has prose and no audio -- the same state a crash
    // between insert_chapter and attach_audio leaves behind.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();
    let text_before = e.chapter_text(1);
    assert!(!e.has_audio(1));
    let pass1_calls_before = e.generator().pass1_count();

    // Cycle 2 with a working renderer: the resume path must fix the audio and leave the
    // prose untouched. Regenerating it would rewrite history that already shipped.
    let e2 = Engine::new(
        e.into_store(),
        FakeGenerator::new().with_prose("[narrator] COMPLETELY DIFFERENT PROSE."),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );

    match e2.run_cycle(0).await.unwrap() {
        CycleOutcome::ResumedRender { chapter, has_audio } => {
            assert_eq!(chapter, 1);
            assert!(has_audio);
        }
        other => panic!("expected ResumedRender, got {other:?}"),
    }

    assert_eq!(e2.chapter_text(1), text_before, "the prose must not change");
    assert!(!e2.chapter_text(1).contains("COMPLETELY DIFFERENT"));
    assert_eq!(
        e2.generator().pass1_count(),
        0,
        "a resume must not call Ember at all"
    );
    assert_eq!(
        pass1_calls_before, 1,
        "the first cycle's pass 1 succeeded first time, so it needed no retries"
    );
    assert!(e2.has_audio(1));
}

#[tokio::test]
async fn a_resumed_render_rebuilds_the_same_voices_from_the_persisted_cast() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();
    let cast_before = e.cast_pairs();

    let e2 = Engine::new(
        e.into_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );
    e2.run_cycle(0).await.unwrap();

    assert_eq!(
        e2.cast_pairs(),
        cast_before,
        "resume must not recast anyone"
    );

    // And the voices actually handed to the renderer match the cast.
    let voices = e2.renderer().last_voices();
    assert!(
        voices.contains(&"sherpa:piper-en_GB-cori:0".to_string()),
        "narrator"
    );
    assert!(voices.contains(&SYSTEM_VOICE.to_string()), "SYSTEM");
}

#[tokio::test]
async fn resume_runs_before_the_buffer_check_so_a_stuck_chapter_cannot_be_starved() {
    // Two good chapters (buffer_target is 3 here via `config()`... so use a target of 2)
    // and then a third that fails to render. Even when the buffer is at target, a
    // text-only chapter must be picked up, or it sits unrendered forever while the loop
    // cheerfully reports Idle.
    let cfg = EngineConfig {
        buffer_target: 2,
        ..config()
    };
    let e = Engine::new(
        store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        cfg.clone(),
    );
    e.run_cycle(0).await.unwrap(); // ch1, audio
    e.run_cycle(0).await.unwrap(); // ch2, audio -> depth is now 2 == target

    // `consumed_through = 2` frees the buffer so a third chapter is generated; its
    // render fails, leaving it text-only.
    let e = Engine::new(
        e.into_store(),
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        cfg.clone(),
    );
    assert_eq!(e.run_cycle(2).await.unwrap().produced_chapter(), Some(3));
    assert!(!e.has_audio(3));

    // Now the buffer is full again (chapters 1 and 2 both have audio, target 2), so a
    // naive implementation would idle and never fix chapter 3.
    let e = Engine::new(
        e.into_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        cfg,
    );
    match e.run_cycle(0).await.unwrap() {
        CycleOutcome::ResumedRender { chapter, has_audio } => {
            assert_eq!(chapter, 3);
            assert!(has_audio);
        }
        other => panic!("expected ResumedRender even with a full buffer, got {other:?}"),
    }
}

#[tokio::test]
async fn a_resume_replaces_segments_rather_than_duplicating_them() {
    // Chapter 1 lands text-only, then a resume attaches audio. Running again must move
    // on to chapter 2 and leave chapter 1's segment rows exactly as the resume wrote
    // them -- a second set of rows would make the manifest and the segments table
    // disagree, and every client derives Range requests from that pair.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();
    assert_eq!(e.segment_count(1), 0, "a failed render attaches nothing");

    let e = Engine::new(
        e.into_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );
    assert!(matches!(
        e.run_cycle(0).await.unwrap(),
        CycleOutcome::ResumedRender { .. }
    ));

    let count_after_resume = e.segment_count(1);
    let duration_after_resume = e.duration_ms(1);
    assert!(count_after_resume > 0);

    // The next cycle has nothing to resume, so it produces chapter 2 and leaves 1 alone.
    assert_eq!(e.run_cycle(0).await.unwrap().produced_chapter(), Some(2));
    assert_eq!(
        e.segment_count(1),
        count_after_resume,
        "segments were duplicated"
    );
    assert_eq!(e.duration_ms(1), duration_after_resume);
}

// ---------------------------------------------------------------------------
// Step 2 / 10 — notes are drained exactly once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_director_note_reaches_the_prompt_and_is_consumed_exactly_once() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.insert_note("introduce a rival", "cli");

    e.run_cycle(0).await.unwrap();
    assert!(
        e.generator()
            .all_pass1_prompts()
            .contains("introduce a rival"),
        "the note must reach the model"
    );
    assert!(
        e.pending_note_count() == 0,
        "the note must be marked consumed"
    );

    // A second cycle must not see it again -- a note applied twice would keep steering
    // the story after JP moved on.
    let before = e
        .generator()
        .all_pass1_prompts()
        .matches("introduce a rival")
        .count();
    e.run_cycle(0).await.unwrap();
    let after = e
        .generator()
        .all_pass1_prompts()
        .matches("introduce a rival")
        .count();
    assert_eq!(before, after, "the note leaked into a second chapter");
}

#[tokio::test]
async fn notes_are_consumed_even_when_the_render_fails() {
    // The note was honoured by the prose, and the prose is durable. Leaving it pending
    // would re-apply it to the next chapter as well.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.insert_note("give Sera a good line", "watch");
    e.run_cycle(0).await.unwrap();
    assert_eq!(e.pending_note_count(), 0);
}

#[tokio::test]
async fn notes_are_not_consumed_when_pass_1_fails() {
    // Nothing was written, so the note has not been honoured yet and must survive for
    // the next attempt.
    let e = engine(
        FakeGenerator::new().push_pass1(Err(EmberError::Transport {
            detail: "down".into(),
        })),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.insert_note("introduce a rival", "cli");
    e.run_cycle(0).await.unwrap();
    assert_eq!(
        e.pending_note_count(),
        1,
        "an abandoned cycle must not eat the note"
    );
}

// ---------------------------------------------------------------------------
// Prompt assembly through the cycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn matched_lore_reaches_the_prompt_and_unmatched_lore_does_not() {
    let library = FakeLibrary::new().with_lore(vec![
        lore_entry("Ashen Vale", "vale", false),
        lore_entry("Sunspire", "sunspire", false),
        lore_entry("World Rules", "nothing", true),
    ]);
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        library,
        FakeArtifacts::new(),
    );

    e.run_cycle(0).await.unwrap();
    let prompt = e.generator().all_pass1_prompts();
    assert!(
        prompt.contains("Body of World Rules."),
        "always_on must be injected"
    );
    assert!(
        !prompt.contains("Body of Sunspire."),
        "an unmatched entry must not burn context"
    );
}

#[tokio::test]
async fn the_previous_chapters_prose_is_used_for_retrieval_but_never_sent_to_the_model() {
    // §6.3: the scan window includes the last chapter's text, but raw prose must never
    // enter the prompt or the model starts mimicking its own cadence.
    let marker = "PECULIAR_INCANTATION_MARKER";
    let e = engine(
        FakeGenerator::new().with_prose(&format!("[narrator] {marker} and the vale went quiet.")),
        FakeRenderer::new(),
        FakeLibrary::new().with_lore(vec![lore_entry(
            "Marker Lore",
            "peculiar_incantation_marker",
            false,
        )]),
        FakeArtifacts::new(),
    );

    e.run_cycle(0).await.unwrap(); // chapter 1 contains the marker
    let prompts_after_one = e.generator().all_pass1_prompts();
    e.run_cycle(0).await.unwrap(); // chapter 2 scans chapter 1's text

    let chapter_two_prompt = e
        .generator()
        .all_pass1_prompts()
        .strip_prefix(&prompts_after_one)
        .expect("prompts accumulate")
        .to_string();

    assert!(
        chapter_two_prompt.contains("Body of Marker Lore."),
        "the marker in chapter 1's prose should have matched the lore keyword, proving \
         the scan window really includes the previous chapter"
    );
    assert!(
        !chapter_two_prompt.contains(marker),
        "chapter 1's prose leaked verbatim into chapter 2's prompt"
    );
}

#[tokio::test]
async fn recent_summaries_reach_the_prompt_and_older_ones_are_dropped() {
    // §6.3 budgets the last five. An unbounded history would grow the prompt without
    // bound and eventually crowd out the state snapshot, which is the one section that
    // must never be squeezed.
    let summaries: Vec<litrpg_ember::prompt::ChapterSummary> = (1..=8)
        .map(|c| litrpg_ember::prompt::ChapterSummary {
            chapter: c,
            body_md: format!("SUMMARY_MARKER_{c}"),
        })
        .collect();

    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new().with_summaries(summaries),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();

    let prompt = e.generator().all_pass1_prompts();
    for recent in 4..=8 {
        assert!(
            prompt.contains(&format!("SUMMARY_MARKER_{recent}")),
            "summary {recent} should be in the window"
        );
    }
    for old in 1..=3 {
        assert!(
            !prompt.contains(&format!("SUMMARY_MARKER_{old}")),
            "summary {old} is outside the five-chapter window and must be dropped"
        );
    }
}

#[tokio::test]
async fn the_renderer_is_called_exactly_once_per_chapter() {
    // One batch call per chapter is what lets Azure emit a single multi-voice SSML
    // request and sherpa fill its worker pool; calling per segment would defeat both.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();
    assert_eq!(e.renderer().call_count(), 1);

    e.run_cycle(u32::MAX).await.unwrap();
    assert_eq!(e.renderer().call_count(), 2);
}

#[tokio::test]
async fn the_prompt_hash_is_stable_across_chapters_with_an_unchanged_prompt() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(u32::MAX).await.unwrap();
    e.run_cycle(u32::MAX).await.unwrap();
    assert_eq!(
        e.prompt_hash(1),
        e.prompt_hash(2),
        "an unchanged story prompt must hash identically, or §9.3 provenance is useless"
    );
    assert!(!e.prompt_hash(1).is_empty());
}

// ---------------------------------------------------------------------------
// The manifest invariant, through the whole cycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_persisted_manifest_matches_the_audio_exactly() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();

    let segs = e.segments(1);
    assert!(!segs.is_empty());
    assert_eq!(segs[0].start_ms, 0, "the first segment must start at zero");
    for w in segs.windows(2) {
        assert_eq!(w[0].end_ms, w[1].start_ms, "segments must be contiguous");
    }
    let total: u32 = segs.last().unwrap().end_ms;
    assert_eq!(e.duration_ms(1), total);
    for s in &segs {
        assert_eq!(s.start_byte(), s.start_ms as u64 * 32);
    }
}

#[tokio::test]
async fn every_segment_gets_the_voice_its_speaker_was_cast_with() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();

    let cast = e.cast_pairs();
    for s in e.segments(1) {
        let expected = cast
            .iter()
            .find(|(sp, _)| sp.eq_ignore_ascii_case(&s.speaker))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("speaker {} was never cast", s.speaker));
        assert_eq!(
            s.voice_ref, expected,
            "speaker {} got the wrong voice",
            s.speaker
        );
    }
}

#[tokio::test]
async fn narration_after_a_system_block_is_not_read_in_the_robot_voice() {
    // The block-shaped output Ember actually produces (measured 2026-07-29): a [SYSTEM]
    // block, a blank line, then untagged narration. This is only ever *audible*, so it
    // is pinned here at the pipeline level too.
    let prose = "\
[SYSTEM]
Quest Updated: The Ashen Covenant
Seal integrity at 98%.

Kaelen adjusted the strap of his gauntlet, and the leather creaked.

[Kaelen]
\"One down.\"";

    let e = engine(
        FakeGenerator::new().with_prose(prose),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(0).await.unwrap();

    let segs = e.segments(1);
    let narration = segs
        .iter()
        .find(|s| s.text.contains("gauntlet"))
        .expect("the narration paragraph must survive");
    assert_eq!(narration.speaker, "narrator");
    assert_ne!(
        narration.voice_ref, SYSTEM_VOICE,
        "narration must never carry the SYSTEM voice"
    );
    assert_eq!(narration.voice_ref, "sherpa:piper-en_GB-cori:0");
}
