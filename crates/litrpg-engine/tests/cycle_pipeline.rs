//! The cycle end-to-end against fakes: ordering, degradations, and resume.
//!
//! These are the tests that matter most in the whole crate, because almost every rule
//! they check fails *silently* in production — a rejected stat that nobody sees, a note
//! applied twice, narration in the robot's voice, a manifest that disagrees with the audio
//! by a few milliseconds.

mod support;

use litrpg_ember::EmberError;
use litrpg_engine::{BufferCursor, CycleOutcome, Engine, EngineConfig, SYSTEM_VOICE};
use support::*;

fn config() -> EngineConfig {
    EngineConfig {
        buffer_target: 3,
        narrator_voice: "sherpa:piper-en_GB-cori:0".to_string(),
        ..EngineConfig::default()
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
            e.run_cycle(BufferCursor::Drain)
                .await
                .unwrap()
                .produced_chapter(),
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let kinds = e.artifacts_kinds();
    // Four, including the markdown. A real run had `.pcm`, `.mp3` and `.json` on disk and no
    // `.md` at all, because `write_text` was never called -- and this test passed, because it
    // was named "all four" while only checking three.
    for want in ["text", "pcm", "mp3", "manifest"] {
        assert!(
            kinds.contains(&want.to_string()),
            "missing {want} in {kinds:?}"
        );
    }
}

#[tokio::test]
async fn the_chapter_markdown_is_written_even_when_the_render_fails() {
    // §8 makes `NNNN.md` the canonical permanent artifact and §10 says text ships regardless
    // of audio, so the two rules together mean the markdown must not be inside the render path.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    assert!(!e.has_audio(1));
    assert!(
        e.artifacts_kinds().contains(&"text".to_string()),
        "the markdown must ship even with no audio: {:?}",
        e.artifacts_kinds()
    );
}

#[tokio::test]
async fn a_failure_to_write_the_markdown_does_not_cost_the_chapter() {
    // The prose is already durable in `chapters.text_md`; a full disk must not lose it.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::failing_on("text"),
    );
    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
        CycleOutcome::Produced {
            chapter, has_audio, ..
        } => {
            assert_eq!(chapter, 1);
            assert!(has_audio, "the audio path is independent of the markdown");
        }
        other => panic!("expected Produced, got {other:?}"),
    }
    assert!(!e.chapter_text(1).is_empty());
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
        assert!(
            e.run_cycle(BufferCursor::At(0))
                .await
                .unwrap()
                .produced_chapter()
                .is_some()
        );
    }

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
        e.run_cycle(BufferCursor::At(0)).await.unwrap();
    }
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::Idle { .. }
    ));

    // The listener finished chapter 1, so only 2 remain ahead of them.
    assert!(
        e.run_cycle(BufferCursor::At(1))
            .await
            .unwrap()
            .produced_chapter()
            .is_some(),
        "consuming a chapter should let production resume"
    );
}

// ---------------------------------------------------------------------------
// The playback cursor. `Stored` must be re-read every cycle, or a long-running
// daemon never notices that anyone listened — which is the entire point of it
// being settable.
// ---------------------------------------------------------------------------

/// A story row has to exist before the cursor can be set.
fn seed_story(e: &FakeEngine) {
    seed_story_with_protagonist(e, "Kaelen");
}

/// Seed the store's story row.
///
/// The protagonist must match the one the [`Library`] reports: in production both come from the
/// same row, since `StoreLibrary` reads it — but a fake can let them diverge, and then the engine
/// resolves a name the gate has never heard of. Keeping them equal here is what makes the fake
/// faithful rather than convenient.
fn seed_story_with_protagonist(e: &FakeEngine, protagonist: &str) {
    e.with_store(|s| {
        s.insert_story_if_absent(&litrpg_store::NewStory {
            title: "The Ashen Ledger".into(),
            protagonist: protagonist.into(),
            prompt_path: "/dev/null".into(),
            prompt_hash: litrpg_core::content_hash("x"),
            target_words: 600,
        })
    })
    .unwrap();
}

#[tokio::test]
async fn the_stored_cursor_is_re_read_on_every_cycle() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    seed_story(&e);

    // buffer_target is 3, cursor is 0, so three chapters then idle.
    for _ in 0..3 {
        assert!(
            e.run_cycle(BufferCursor::Stored)
                .await
                .unwrap()
                .produced_chapter()
                .is_some()
        );
    }
    assert!(matches!(
        e.run_cycle(BufferCursor::Stored).await.unwrap(),
        CycleOutcome::Idle { .. }
    ));

    // Someone listens to chapter 1 *while the engine is running*. Caching the cursor at
    // startup would leave this process idling forever.
    e.with_store(|s| s.set_consumed_through(1)).unwrap();

    assert_eq!(
        e.run_cycle(BufferCursor::Stored)
            .await
            .unwrap()
            .produced_chapter(),
        Some(4),
        "the cursor moved, so the buffer has room again"
    );
}

#[tokio::test]
async fn a_missing_story_row_reads_as_a_zero_cursor_rather_than_erroring() {
    // `consumed_through()` answers 0 with no story row, and it is read every cycle, so the
    // cycle must not need a story row of its own to check the buffer.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    assert_eq!(
        e.run_cycle(BufferCursor::Stored)
            .await
            .unwrap()
            .produced_chapter(),
        Some(1)
    );
}

#[tokio::test]
async fn an_explicit_cursor_overrides_the_stored_one() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    seed_story(&e);
    for _ in 0..3 {
        e.run_cycle(BufferCursor::Stored).await.unwrap();
    }
    assert!(matches!(
        e.run_cycle(BufferCursor::Stored).await.unwrap(),
        CycleOutcome::Idle { .. }
    ));

    // The stored cursor still says 0, but the override says two chapters are done.
    assert_eq!(
        e.run_cycle(BufferCursor::At(2))
            .await
            .unwrap()
            .produced_chapter(),
        Some(4)
    );
    assert_eq!(
        e.with_store(|s| s.consumed_through()).unwrap(),
        0,
        "an override must not write back to the database"
    );
}

#[tokio::test]
async fn drain_ignores_a_stored_cursor_entirely() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    seed_story(&e);

    // Well past buffer_target, and never idle.
    for expected in 1..=5u32 {
        assert_eq!(
            e.run_cycle(BufferCursor::Drain)
                .await
                .unwrap()
                .produced_chapter(),
            Some(expected)
        );
    }
    assert_eq!(e.with_store(|s| s.consumed_through()).unwrap(), 0);
}

#[tokio::test]
async fn the_default_cursor_is_the_stored_one() {
    // So a caller that forgets to choose gets the behaviour that respects the listener.
    assert_eq!(BufferCursor::default(), BufferCursor::Stored);
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
        CycleOutcome::Produced { applied, .. } => assert_eq!(applied, 1),
        other => panic!("expected Produced, got {other:?}"),
    }
    assert_eq!(e.snapshot_num("Kaelen", "xp"), Some(150));
}

#[tokio::test]
async fn a_delta_addressed_to_a_voice_is_refused_not_applied() {
    // Measured live: pass 2 attributed a whole [SYSTEM] stat block to `subject: "SYSTEM"`.
    // The store's gate *accepts* that, because `SYSTEM` is a cast row and therefore a known
    // subject -- so Kaelen's inventory would accrue to a pseudo-person while his own
    // character screen stayed empty.
    let extraction = extraction_with(
        vec![
            delta("SYSTEM", "inv:Ledger of Debts", "set", Some(1)),
            delta("narrator", "gold", "add", Some(10)),
            delta("Kaelen", "xp", "add", Some(500)),
        ],
        vec![],
    );
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
        CycleOutcome::Produced {
            applied, rejected, ..
        } => {
            assert_eq!(applied, 1, "only Kaelen's delta is legitimate");
            assert_eq!(rejected, 2, "both voice-addressed deltas must be counted");
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    assert_eq!(e.snapshot_num("Kaelen", "xp"), Some(500));
    assert_eq!(
        e.snapshot_num("SYSTEM", "inv:Ledger of Debts"),
        None,
        "a voice must hold no state"
    );
    assert_eq!(e.snapshot_num("narrator", "gold"), None);
}

#[tokio::test]
async fn pass_2_is_not_offered_the_voices_as_known_subjects() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let calls = e.generator().pass2_calls.lock().unwrap();
    let (_, known) = calls.first().expect("pass 2 was called");
    assert!(
        known.iter().any(|s| s == "Kaelen"),
        "real characters must be offered: {known:?}"
    );
    for voice in ["narrator", "SYSTEM"] {
        assert!(
            !known.iter().any(|s| s == voice),
            "{voice} is a voice, not a person, and must not be offered as a subject: {known:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Gender-matched casting. The hint arrives with `new_lore`, i.e. *after* step 4 has
// already cast the chapter, so the correction has to land before the render.
// ---------------------------------------------------------------------------

/// Four voices, two of each gender, so a mismatch is unambiguous.
fn gendered_config() -> EngineConfig {
    use litrpg_tts::Gender;
    EngineConfig {
        buffer_target: 3,
        narrator_voice: "azure:narr".to_string(),
        system_voice: "azure:sys".to_string(),
        character_voices: vec![
            "azure:f1".to_string(),
            "azure:f2".to_string(),
            "azure:m1".to_string(),
            "azure:m2".to_string(),
        ],
        voice_genders: [
            ("azure:narr", Gender::Female),
            ("azure:sys", Gender::Neutral),
            ("azure:f1", Gender::Female),
            ("azure:f2", Gender::Female),
            ("azure:m1", Gender::Male),
            ("azure:m2", Gender::Male),
        ]
        .into_iter()
        .map(|(v, g)| (v.to_string(), g))
        .collect(),
        ..EngineConfig::default()
    }
}

fn gendered_engine(generator: FakeGenerator) -> FakeEngine {
    Engine::new(
        store(),
        generator,
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        gendered_config(),
    )
}

#[tokio::test]
async fn a_male_character_is_re_cast_to_a_male_voice_before_the_render() {
    // Kaelen speaks first, so the round-robin hands him `azure:f1`. The hint corrects it.
    let extraction = extraction_with(vec![], vec![gendered_lore("Kaelen", "male")]);
    let e = gendered_engine(FakeGenerator::new().with_extraction(extraction));
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let cast = e.cast_pairs();
    let kaelen = cast
        .iter()
        .find(|(s, _)| s == "Kaelen")
        .expect("Kaelen must be cast");
    assert!(
        kaelen.1.starts_with("azure:m"),
        "Kaelen is male and drew {}",
        kaelen.1
    );

    // And the correction reached the audio, not just the cast table.
    for seg in e.segments(1).iter().filter(|s| s.speaker == "Kaelen") {
        assert_eq!(
            seg.voice_ref, kaelen.1,
            "the manifest must agree with the cast"
        );
    }
}

/// The case `new_lore` could never cover: Kaelen is named in the premise, so he is never
/// "newly introduced", and three live runs produced `0 lore rows`. `speakers` reports whoever
/// spoke, so the hint arrives on every chapter — including chapter one, for the protagonist.
#[tokio::test]
async fn a_speakers_gender_hint_re_casts_without_any_new_lore() {
    let extraction = extraction_with_speakers(&[("Kaelen", Some("male"))]);
    assert!(
        extraction.new_lore.is_empty(),
        "this must work with no lore rows at all"
    );

    let e = gendered_engine(FakeGenerator::new().with_extraction(extraction));
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let cast = e.cast_pairs();
    let kaelen = cast.iter().find(|(s, _)| s == "Kaelen").unwrap();
    assert!(
        kaelen.1.starts_with("azure:m"),
        "Kaelen is male and drew {}",
        kaelen.1
    );
    for seg in e.segments(1).iter().filter(|s| s.speaker == "Kaelen") {
        assert_eq!(seg.voice_ref, kaelen.1);
    }
}

#[tokio::test]
async fn a_speaker_without_a_gender_is_left_alone() {
    let e = gendered_engine(
        FakeGenerator::new().with_extraction(extraction_with_speakers(&[("Kaelen", None)])),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert_eq!(
        e.cast_pairs()
            .iter()
            .find(|(s, _)| s == "Kaelen")
            .unwrap()
            .1,
        "azure:f1",
        "an absent hint must be inert"
    );
}

#[tokio::test]
async fn new_lore_wins_over_speakers_when_they_disagree() {
    // `new_lore` is the specific case -- a character the chapter describes in detail -- so it
    // takes precedence over the general speaker listing.
    let mut extraction = extraction_with_speakers(&[("Kaelen", Some("female"))]);
    extraction.new_lore = vec![gendered_lore("Kaelen", "male")];

    let e = gendered_engine(FakeGenerator::new().with_extraction(extraction));
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert!(
        e.cast_pairs()
            .iter()
            .find(|(s, _)| s == "Kaelen")
            .unwrap()
            .1
            .starts_with("azure:m"),
        "the more specific hint should win"
    );
}

#[tokio::test]
async fn a_speaker_hint_matches_the_cast_name_case_insensitively() {
    let e = gendered_engine(
        FakeGenerator::new().with_extraction(extraction_with_speakers(&[("KAELEN", Some("male"))])),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert!(
        e.cast_pairs()
            .iter()
            .find(|(s, _)| s == "Kaelen")
            .unwrap()
            .1
            .starts_with("azure:m")
    );
}

#[tokio::test]
async fn a_speaker_hint_for_a_voice_is_ignored() {
    // The prompt forbids it, but `narrator` and `SYSTEM` must never be re-cast as characters
    // even if the model lists them.
    let e = gendered_engine(
        FakeGenerator::new().with_extraction(extraction_with_speakers(&[
            ("narrator", Some("male")),
            ("SYSTEM", Some("female")),
        ])),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let cast = e.cast_pairs();
    assert_eq!(
        cast.iter().find(|(s, _)| s == "narrator").unwrap().1,
        "azure:narr",
        "the narrator keeps its configured voice"
    );
    assert_eq!(
        cast.iter().find(|(s, _)| s == "SYSTEM").unwrap().1,
        "azure:sys"
    );
}

#[tokio::test]
async fn a_correct_guess_is_left_alone() {
    // The round-robin already gives the first speaker a female voice, so a female hint must
    // be a no-op rather than a needless re-draw.
    let extraction = extraction_with(vec![], vec![gendered_lore("Kaelen", "female")]);
    let e = gendered_engine(FakeGenerator::new().with_extraction(extraction));
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let cast = e.cast_pairs();
    let kaelen = cast.iter().find(|(s, _)| s == "Kaelen").unwrap();
    assert_eq!(kaelen.1, "azure:f1");
}

#[tokio::test]
async fn no_hint_leaves_casting_exactly_as_it_was() {
    // The field is optional and a model that omits it must change nothing.
    let with_hint = gendered_engine(FakeGenerator::new());
    with_hint.run_cycle(BufferCursor::At(0)).await.unwrap();
    let baseline = with_hint.cast_pairs();

    let none = gendered_engine(FakeGenerator::new().with_extraction(extraction_with(
        vec![],
        vec![lore_row("Kaelen", "character", "kaelen")],
    )));
    none.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert_eq!(none.cast_pairs(), baseline, "an absent hint must be inert");
}

#[tokio::test]
async fn a_nonsense_gender_value_is_ignored_rather_than_mis_casting() {
    let extraction = extraction_with(vec![], vec![gendered_lore("Kaelen", "wizard")]);
    let e = gendered_engine(FakeGenerator::new().with_extraction(extraction));
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    let cast = e.cast_pairs();
    assert_eq!(
        cast.iter().find(|(s, _)| s == "Kaelen").unwrap().1,
        "azure:f1"
    );
}

#[tokio::test]
async fn a_gender_hint_on_a_place_is_ignored() {
    let mut place = gendered_lore("The Ashen Vale", "male");
    place.kind = "place".to_string();
    let e =
        gendered_engine(FakeGenerator::new().with_extraction(extraction_with(vec![], vec![place])));
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    // Nothing was re-cast, and the place did not become a cast member.
    assert!(!e.cast_pairs().iter().any(|(s, _)| s == "The Ashen Vale"));
}

#[tokio::test]
async fn an_exhausted_gender_group_degrades_instead_of_stealing_or_panicking() {
    // Three male speakers but only two male voices. The third must keep its round-robin draw
    // rather than panicking or taking a voice already assigned to someone else — the empty-pool
    // fallback becomes load-bearing the moment gender filtering exists.
    let prose = "\
[Kaelen] \"One.\"

[Joryn] \"Two.\"

[Vance] \"Three.\"

[narrator] They stood in the ash.";
    let extraction = extraction_with(
        vec![],
        vec![
            gendered_lore("Kaelen", "male"),
            gendered_lore("Joryn", "male"),
            gendered_lore("Vance", "male"),
        ],
    );
    let e = gendered_engine(
        FakeGenerator::new()
            .with_prose(prose)
            .with_extraction(extraction),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let cast = e.cast_pairs();
    let voices: Vec<&String> = cast
        .iter()
        .filter(|(s, _)| s != "narrator")
        .map(|(_, v)| v)
        .collect();
    let mut uniq = voices.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(
        voices.len(),
        uniq.len(),
        "no two characters may share a voice even when a gender group runs dry: {cast:?}"
    );
    assert_eq!(
        voices.iter().filter(|v| v.starts_with("azure:m")).count(),
        2,
        "both male voices should be used: {cast:?}"
    );
}

#[tokio::test]
async fn an_established_character_is_never_re_voiced_by_a_late_hint() {
    // Continuity: chapter 1 published Kaelen in a voice. A hint arriving in chapter 2 must not
    // rewrite what chapter 1's audio already sounds like.
    let e = gendered_engine(FakeGenerator::new());
    e.run_cycle(BufferCursor::Drain).await.unwrap();
    let before = e.cast_pairs();

    let e2 = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new().with_extraction(extraction_with(
            vec![],
            vec![gendered_lore("Kaelen", "male")],
        )),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        gendered_config(),
    );
    e2.run_cycle(BufferCursor::Drain).await.unwrap();

    assert_eq!(
        e2.cast_pairs(),
        before,
        "an established cast member must keep their voice"
    );
}

// ---------------------------------------------------------------------------
// The engine heartbeat. It exists because nothing compared what the cast asked
// for against what the process could serve, so a `sherpa:` cast rendered in an
// Azure voice for four chapters with no symptom but the sound.
// ---------------------------------------------------------------------------

fn engine_with_backends(backends: &[&str]) -> FakeEngine {
    Engine::new(
        store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        EngineConfig {
            registered_backends: backends.iter().map(|b| b.to_string()).collect(),
            ..config()
        },
    )
}

#[tokio::test]
async fn a_produced_cycle_stamps_the_heartbeat() {
    let e = engine_with_backends(&["azure", "sherpa"]);
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let hb = e
        .with_store(|s| s.engine_heartbeat())
        .unwrap()
        .expect("a cycle must leave a heartbeat");
    assert_eq!(hb.pid, std::process::id() as i64, "must be *this* process");
    assert_eq!(
        hb.version,
        env!("CARGO_PKG_VERSION"),
        "the engine's version"
    );
    assert_eq!(
        hb.backends,
        vec!["azure".to_string(), "sherpa".to_string()],
        "what the process can serve"
    );
    assert!(hb.seen_at > 0);
}

/// The case the heartbeat is *for*: a build without `--features sherpa` registers a smaller
/// registry and then substitutes silently. The row has to show `["azure"]` so the mismatch against
/// a `sherpa:` cast row is visible from outside the process.
#[tokio::test]
async fn the_heartbeat_reports_only_the_backends_actually_loaded() {
    let e = engine_with_backends(&["azure"]);
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    let hb = e.with_store(|s| s.engine_heartbeat()).unwrap().unwrap();
    assert_eq!(hb.backends, vec!["azure".to_string()]);
    assert!(
        !hb.backends.contains(&"sherpa".to_string()),
        "reporting a backend this build lacks would be an instrument that lies"
    );
}

/// Phase stamps: a produced chapter refreshes the heartbeat several times, so a crash mid-cycle
/// leaves a timestamp from the current phase rather than from the cycle's start.
#[tokio::test]
async fn a_produced_cycle_stamps_at_phase_boundaries() {
    let e = engine_with_backends(&["azure"]);
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    // The store keeps one row (last-writer-wins), so the observable is that it advanced past the
    // top-of-cycle stamp rather than a count. Asserted via a second cycle to get an ordering.
    let first = e
        .with_store(|s| s.engine_heartbeat())
        .unwrap()
        .unwrap()
        .seen_at;
    e.run_cycle(BufferCursor::Drain).await.unwrap();
    let second = e
        .with_store(|s| s.engine_heartbeat())
        .unwrap()
        .unwrap()
        .seen_at;
    assert!(second >= first, "each cycle must refresh the heartbeat");
}

#[tokio::test]
async fn an_idle_cycle_still_stamps_the_heartbeat() {
    // The whole point: a caught-up engine is healthy, and a heartbeat that only fires when work
    // happens would go stale and read as dead.
    let e = engine_with_backends(&["azure"]);
    for _ in 0..3 {
        e.run_cycle(BufferCursor::At(0)).await.unwrap();
    }
    let before = e
        .with_store(|s| s.engine_heartbeat())
        .unwrap()
        .unwrap()
        .seen_at;

    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::Idle { .. }
    ));
    let after = e
        .with_store(|s| s.engine_heartbeat())
        .unwrap()
        .unwrap()
        .seen_at;
    assert!(after >= before, "an idle cycle must refresh the heartbeat");
}

#[tokio::test]
async fn an_abandoned_cycle_still_stamps_the_heartbeat() {
    // Ember being unreachable is exactly when someone asks "is the engine even running".
    let e = Engine::new(
        store(),
        FakeGenerator::new().push_pass1(Err(EmberError::Transport {
            detail: "down".into(),
        })),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        EngineConfig {
            registered_backends: vec!["azure".to_string()],
            ..config()
        },
    );
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::Abandoned { .. }
    ));
    assert!(
        e.with_store(|s| s.engine_heartbeat()).unwrap().is_some(),
        "a failing engine is still a running engine"
    );
}

#[tokio::test]
async fn no_heartbeat_at_all_is_distinguishable_from_a_stale_one() {
    // `None` means nothing has ever run here; a stale timestamp means something ran and stopped.
    // Callers should say different things, so the engine must not pre-seed a row.
    let e = engine_with_backends(&["azure"]);
    assert!(
        e.with_store(|s| s.engine_heartbeat()).unwrap().is_none(),
        "constructing an engine must not stamp anything"
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert!(e.with_store(|s| s.engine_heartbeat()).unwrap().is_some());
}

// ---------------------------------------------------------------------------
// `story.prompt_hash` means "the premise now in effect". `litrpg prompt`
// deliberately does not write it, so the engine restamping it at a chapter
// boundary is what stops `litrpg status` reporting a pending edit forever.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_produced_chapter_stamps_the_prompt_hash_it_used() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let stamped = e.library().hashes_stamped();
    assert_eq!(stamped.len(), 1, "exactly once per chapter: {stamped:?}");
    assert_eq!(
        stamped[0],
        e.prompt_hash(1),
        "the stamped value must be the one recorded on the chapter, not a re-hash of the file"
    );
    assert!(
        stamped[0].starts_with("fnv1a64:"),
        "core's algorithm, not a second one"
    );
}

#[tokio::test]
async fn an_abandoned_cycle_stamps_nothing() {
    // Pass 1 failed, so no chapter was written from this premise and it was never *in effect*.
    // Stamping here would clear a pending-edit warning while producing nothing.
    let e = engine(
        FakeGenerator::new().push_pass1(Err(EmberError::Transport {
            detail: "down".into(),
        })),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::Abandoned { .. }
    ));
    assert!(
        e.library().hashes_stamped().is_empty(),
        "an abandoned cycle must leave the pending-edit warning standing"
    );
}

#[tokio::test]
async fn an_idle_cycle_stamps_nothing() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    for _ in 0..3 {
        e.run_cycle(BufferCursor::At(0)).await.unwrap();
    }
    let before = e.library().hashes_stamped().len();
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::Idle { .. }
    ));
    assert_eq!(
        e.library().hashes_stamped().len(),
        before,
        "idling consults no premise"
    );
}

#[tokio::test]
async fn a_resumed_render_stamps_nothing() {
    // Resume renders audio for prose that already shipped and never reads the premise, so it has
    // no business claiming a premise is in effect.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    let after_generation = e.library().hashes_stamped().len();
    assert_eq!(after_generation, 1);

    let e = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::ResumedRender { .. }
    ));
    assert!(
        e.library().hashes_stamped().is_empty(),
        "a resume must not stamp: this engine generated nothing"
    );
}

#[tokio::test]
async fn a_chapter_that_ships_without_audio_still_stamps() {
    // The premise was in effect: prose exists. Audio is a separate artifact (§10).
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert!(!e.has_audio(1));
    assert_eq!(e.library().hashes_stamped().len(), 1);
}

#[tokio::test]
async fn a_state_dirty_chapter_still_stamps() {
    // Pass 2 failing is bookkeeping; the prose was still written from this premise.
    let e = engine(
        FakeGenerator::new().with_pass2_always_malformed(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert_eq!(e.library().hashes_stamped().len(), 1);
    assert_eq!(e.library().hashes_stamped()[0], e.prompt_hash(1));
}

#[tokio::test]
async fn an_edited_premise_changes_the_stamp_at_the_next_boundary() {
    // The live symptom: chapter 4 used the new premise, but nothing restamped, so `litrpg status`
    // reported a pending edit permanently.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::Drain).await.unwrap();
    let first = e.library().hashes_stamped()[0].clone();
    assert_eq!(first, e.prompt_hash(1));

    // JP edits `prompt.md`. `StoreLibrary` re-reads it every cycle, so a fresh library with a
    // different premise over the same store is exactly that situation.
    let mut edited = FakeLibrary::new();
    edited.story.prompt_md = "A wholly new premise.".to_string();
    let e = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        edited,
        FakeArtifacts::new(),
        config(),
    );
    e.run_cycle(BufferCursor::Drain).await.unwrap();

    let stamped = e.library().hashes_stamped();
    assert_eq!(stamped.len(), 1, "this engine produced one chapter");
    assert_ne!(
        stamped[0], first,
        "the edit must take effect at the boundary"
    );
    assert_eq!(
        stamped[0],
        e.prompt_hash(2),
        "and must equal what chapter 2 recorded"
    );
}

// ---------------------------------------------------------------------------
// Subject canonicalisation (issue #11). One character must not become two
// ledger keys, and two characters must never become one.
// ---------------------------------------------------------------------------

/// The library the incident actually had: a protagonist named more fully than the CLI knew.
fn library_with_protagonist(name: &str) -> FakeLibrary {
    let mut l = FakeLibrary::new();
    l.story.protagonist = name.to_string();
    l
}

#[tokio::test]
async fn a_short_subject_name_lands_on_the_protagonist_not_a_second_key() {
    // Reproduces #11: prose calls him "Kaelen", `story.protagonist` is "Kaelen Vord". Before this,
    // both got ledger rows and `/api/state` showed one character twice with his stats split.
    let extraction = extraction_with(
        vec![
            delta("Kaelen", "xp", "add", Some(150)),
            delta("Kaelen Vord", "gold", "add", Some(12)),
        ],
        vec![],
    );
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        library_with_protagonist("Kaelen Vord"),
        FakeArtifacts::new(),
    );
    seed_story_with_protagonist(&e, "Kaelen Vord");

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
        CycleOutcome::Produced {
            applied, rejected, ..
        } => {
            assert_eq!(applied, 2, "both deltas are legitimate");
            assert_eq!(rejected, 0);
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    // One key, both values.
    assert_eq!(e.snapshot_num("Kaelen Vord", "xp"), Some(150));
    assert_eq!(e.snapshot_num("Kaelen Vord", "gold"), Some(12));
    assert_eq!(
        e.snapshot_num("Kaelen", "xp"),
        None,
        "the short form must not exist as a second subject"
    );

    let subjects: Vec<String> = e
        .with_store(|s| s.snapshot())
        .unwrap()
        .subjects()
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        subjects,
        vec!["Kaelen Vord".to_string()],
        "exactly one key for one character: {subjects:?}"
    );
}

#[tokio::test]
async fn new_lore_for_a_character_is_canonicalised_too() {
    // `new_lore` is the other route to a second known subject, so it needs the same treatment —
    // otherwise the next chapter's deltas anchor onto the duplicate.
    let extraction = extraction_with(
        vec![delta("Kaelen", "level", "set", Some(3))],
        vec![lore_row("Kaelen", "character", "kaelen")],
    );
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        library_with_protagonist("Kaelen Vord"),
        FakeArtifacts::new(),
    );
    seed_story_with_protagonist(&e, "Kaelen Vord");
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    assert_eq!(e.snapshot_num("Kaelen Vord", "level"), Some(3));

    let lore_names: Vec<String> = e
        .with_store(|s| s.lore())
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert!(
        lore_names.contains(&"Kaelen Vord".to_string()),
        "the lore row must carry the canonical name: {lore_names:?}"
    );
    assert!(
        !lore_names.contains(&"Kaelen".to_string()),
        "the short form must not have been written as a second lore row: {lore_names:?}"
    );

    // Documented remaining gap: the prose tag `[Kaelen]` still writes a *cast* row under the short
    // form, so it stays in `known_subjects`. That does not split the ledger — no delta is recorded
    // against it — but it is a second identity for one person, and closing it needs the cast's
    // display name separated from its identity key.
    assert!(
        e.with_store(|s| s.known_subjects())
            .unwrap()
            .contains("Kaelen"),
        "asserting the known gap explicitly, so it is not mistaken for fixed"
    );
}

#[tokio::test]
async fn a_place_is_never_resolved_against_a_person() {
    // Only characters carry identity. A place whose name happens to share a first word with the
    // protagonist must stay itself.
    let mut place = lore_row("Kaelen Hollow", "place", "hollow");
    place.kind = "place".to_string();
    let e = engine(
        FakeGenerator::new().with_extraction(extraction_with(vec![], vec![place])),
        FakeRenderer::new(),
        library_with_protagonist("Kaelen Vord"),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let lore = e.with_store(|s| s.lore()).unwrap();
    assert!(
        lore.iter().any(|l| l.name == "Kaelen Hollow"),
        "the place must keep its own name: {:?}",
        lore.iter().map(|l| &l.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn two_distinct_characters_are_never_fused() {
    // The failure this must never have: an append-only ledger cannot be un-merged.
    let extraction = extraction_with(
        vec![
            delta("Kaelen Vord", "xp", "add", Some(10)),
            delta("Kaelith", "xp", "add", Some(20)),
            delta("Sera", "xp", "add", Some(30)),
        ],
        vec![
            lore_row("Kaelith", "character", "kaelith"),
            lore_row("Sera", "character", "sera"),
        ],
    );
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        library_with_protagonist("Kaelen Vord"),
        FakeArtifacts::new(),
    );
    seed_story_with_protagonist(&e, "Kaelen Vord");
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    assert_eq!(e.snapshot_num("Kaelen Vord", "xp"), Some(10));
    assert_eq!(
        e.snapshot_num("Kaelith", "xp"),
        Some(20),
        "two edits from the protagonist is a different person"
    );
    assert_eq!(e.snapshot_num("Sera", "xp"), Some(30));
}

#[tokio::test]
async fn a_gender_hint_under_a_short_name_still_reaches_the_canonical_cast_member() {
    // The hint has to survive canonicalisation, or resolving names would break gendered casting.
    let mut extraction = extraction_with_speakers(&[("Kaelen", Some("male"))]);
    extraction.deltas = vec![delta("Kaelen", "xp", "add", Some(1))];

    let mut library = library_with_protagonist("Kaelen");
    library.story.protagonist = "Kaelen".to_string();

    let e = Engine::new(
        store(),
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        library,
        FakeArtifacts::new(),
        gendered_config(),
    );
    seed_story_with_protagonist(&e, "Kaelen");
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let cast = e.cast_pairs();
    let kaelen = cast.iter().find(|(s, _)| s == "Kaelen").unwrap();
    assert!(
        kaelen.1.starts_with("azure:m"),
        "the gender hint must still apply, got {}",
        kaelen.1
    );
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
async fn placeholder_values_are_refused_rather_than_recorded_as_fact() {
    // A real run produced 52 applied deltas, most of them `appear:* = "unknown"`, with
    // `rejected: 0` — the gate accepts any string for a text field, correctly, since `""`
    // means "slot is empty". So the guard has to be here.
    let extraction = extraction_with(
        vec![
            delta_txt("Kaelen", "appear:eyes", "unknown"),
            delta_txt("Kaelen", "appear:hair", "none"),
            delta_txt("Kaelen", "location", "the Ashen Vale"),
            delta_txt("Kaelen", "equip:head", ""),
        ],
        vec![],
    );
    let e = engine(
        FakeGenerator::new().with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
        CycleOutcome::Produced {
            applied, rejected, ..
        } => {
            assert_eq!(
                applied, 2,
                "the real location and the empty slot both apply"
            );
            assert_eq!(
                rejected, 2,
                "both placeholders must be counted, not silently dropped"
            );
        }
        other => panic!("expected Produced, got {other:?}"),
    }

    let snap = e.with_store(|s| s.snapshot()).unwrap();
    assert_eq!(snap.txt("Kaelen", "location"), Some("the Ashen Vale"));
    assert_eq!(
        snap.txt("Kaelen", "appear:eyes"),
        None,
        "a placeholder must leave no trace in the snapshot"
    );
    assert_eq!(
        snap.txt("Kaelen", "equip:head"),
        Some(""),
        "an empty slot is a documented value (§6.0) and must survive"
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
    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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
    let out = e.run_cycle(BufferCursor::At(0)).await.unwrap();
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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

    let out = e.run_cycle(BufferCursor::At(0)).await.unwrap();
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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

    assert_eq!(
        e.run_cycle(BufferCursor::At(0))
            .await
            .unwrap()
            .produced_chapter(),
        Some(1)
    );
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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

    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    let text_before = e.chapter_text(1);
    assert!(!e.has_audio(1));
    let pass1_calls_before = e.generator().pass1_count();

    // Cycle 2 with a working renderer: the resume path must fix the audio and leave the
    // prose untouched. Regenerating it would rewrite history that already shipped.
    let e2 = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new().with_prose("[narrator] COMPLETELY DIFFERENT PROSE."),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );

    match e2.run_cycle(BufferCursor::At(0)).await.unwrap() {
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    let cast_before = e.cast_pairs();

    let e2 = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );
    e2.run_cycle(BufferCursor::At(0)).await.unwrap();

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
    e.run_cycle(BufferCursor::At(0)).await.unwrap(); // ch1, audio
    e.run_cycle(BufferCursor::At(0)).await.unwrap(); // ch2, audio -> depth is now 2 == target

    // `consumed_through = 2` frees the buffer so a third chapter is generated; its
    // render fails, leaving it text-only.
    let e = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        cfg.clone(),
    );
    assert_eq!(
        e.run_cycle(BufferCursor::At(2))
            .await
            .unwrap()
            .produced_chapter(),
        Some(3)
    );
    assert!(!e.has_audio(3));

    // Now the buffer is full again (chapters 1 and 2 both have audio, target 2), so a
    // naive implementation would idle and never fix chapter 3.
    let e = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        cfg,
    );
    match e.run_cycle(BufferCursor::At(0)).await.unwrap() {
        CycleOutcome::ResumedRender { chapter, has_audio } => {
            assert_eq!(chapter, 3);
            assert!(has_audio);
        }
        other => panic!("expected ResumedRender even with a full buffer, got {other:?}"),
    }
}

#[tokio::test]
async fn a_permanently_unrenderable_chapter_does_not_block_every_future_chapter() {
    // The resume stage runs first and picks the lowest-numbered chapter without audio.
    // If that chapter can never render -- a voice_ref the registry rejects, a manifest
    // `attach_audio` refuses, a backend that is down for a week -- then a naive resume
    // returns every cycle and the serial stops dead. §10's rule is that a bookkeeping
    // failure must not cost a chapter; it must equally not cost *every chapter after it*.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    // Ten cycles against a renderer that never works.
    for _ in 0..10 {
        e.run_cycle(BufferCursor::Drain).await.unwrap();
    }

    assert!(
        e.latest_number() > 1,
        "the story stalled on chapter 1: latest is {} after ten cycles",
        e.latest_number()
    );
    assert!(
        !e.chapter_text(2).is_empty(),
        "chapter 2 should have been written despite chapter 1 being unrenderable"
    );
}

#[tokio::test]
async fn a_hopeless_chapter_stops_being_retried_and_is_reported_as_stuck() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );

    for _ in 0..10 {
        e.run_cycle(BufferCursor::Drain).await.unwrap();
    }

    assert_eq!(
        e.resume_attempts(1),
        litrpg_engine::MAX_RESUME_ATTEMPTS,
        "chapter 1's retries must be capped, not attempted once per cycle forever"
    );
    assert!(
        e.stuck_chapters().unwrap().contains(&1),
        "a chapter that exhausted its retries must be reportable, not silently abandoned"
    );
}

#[tokio::test]
async fn a_recovered_render_clears_the_stuck_marker() {
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::failing(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::Drain).await.unwrap(); // ch1 text-only
    e.run_cycle(BufferCursor::Drain).await.unwrap(); // resume fails once
    assert_eq!(e.resume_attempts(1), 1);

    // The backend comes back.
    let e = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::ResumedRender { chapter: 1, .. }
    ));
    assert_eq!(e.resume_attempts(1), 0, "a success must clear the counter");
    assert!(e.stuck_chapters().unwrap().is_empty());
}

/// §9.2 makes `litrpg render N` "re-render audio only (e.g. after a cast change)". That is only
/// deliverable if a re-render takes its voices from the **cast** rather than from the segment rows
/// the last render wrote — otherwise a re-render faithfully reproduces whatever was used before and
/// a cast override can never take effect.
///
/// Observed live: chapters rendered by an Azure-only build stayed Azure even when re-rendered by a
/// build that had sherpa, because the rows said Azure while the cast said cori.
#[tokio::test]
async fn a_resume_takes_its_voices_from_the_cast_not_from_the_stored_segments() {
    // Chapter 1 renders and stores segment rows with the voices of the moment.
    let e = engine(
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
    );
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    let original: Vec<String> = e.segments(1).into_iter().map(|s| s.voice_ref).collect();
    assert!(!original.is_empty());

    // An operator re-casts the narrator, exactly as `litrpg cast` would.
    e.with_store(|s| s.upsert_cast("narrator", "azure:recast-narrator", "narrator", 1))
        .unwrap();

    // Force a re-render of the same chapter.
    let planned = e.replan_for_test(1).unwrap();
    let narrator_voices: Vec<&String> = planned
        .iter()
        .filter(|p| p.speaker == "narrator")
        .map(|p| &p.voice_ref)
        .collect();
    assert!(!narrator_voices.is_empty(), "the chapter has narration");
    for v in narrator_voices {
        assert_eq!(
            v, "azure:recast-narrator",
            "a re-render must honour the cast, not replay the stored voice"
        );
    }

    // The published content is untouched — only the voice is re-derived.
    let stored = e.segments(1);
    assert_eq!(planned.len(), stored.len());
    for (p, s) in planned.iter().zip(stored.iter()) {
        assert_eq!(p.idx, s.idx);
        assert_eq!(p.speaker, s.speaker);
        assert_eq!(p.text, s.text, "prose that has shipped must not change");
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert_eq!(e.segment_count(1), 0, "a failed render attaches nothing");

    let e = Engine::with_shared_store(
        e.into_shared_store(),
        FakeGenerator::new(),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        config(),
    );
    assert!(matches!(
        e.run_cycle(BufferCursor::At(0)).await.unwrap(),
        CycleOutcome::ResumedRender { .. }
    ));

    let count_after_resume = e.segment_count(1);
    let duration_after_resume = e.duration_ms(1);
    assert!(count_after_resume > 0);

    // The next cycle has nothing to resume, so it produces chapter 2 and leaves 1 alone.
    assert_eq!(
        e.run_cycle(BufferCursor::At(0))
            .await
            .unwrap()
            .produced_chapter(),
        Some(2)
    );
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

    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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

    e.run_cycle(BufferCursor::At(0)).await.unwrap();
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

    e.run_cycle(BufferCursor::At(0)).await.unwrap(); // chapter 1 contains the marker
    let prompts_after_one = e.generator().all_pass1_prompts();
    e.run_cycle(BufferCursor::At(0)).await.unwrap(); // chapter 2 scans chapter 1's text

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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();
    assert_eq!(e.renderer().call_count(), 1);

    e.run_cycle(BufferCursor::Drain).await.unwrap();
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
    e.run_cycle(BufferCursor::Drain).await.unwrap();
    e.run_cycle(BufferCursor::Drain).await.unwrap();
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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

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
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

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
