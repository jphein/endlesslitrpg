//! One real chapter, end to end. `#[ignore]` — it costs GPU time and Azure money.
//!
//! ```text
//! cargo test -p litrpg-engine --test live_end_to_end -- --ignored --nocapture
//! ```
//!
//! Real everything: `EmberClient` against `familiar:8091`, a real SQLite file in a temp
//! dir, `FsArtifacts` writing `.md`/`.pcm`/`.mp3`/`.json`, and **Azure DragonHD** for TTS.
//!
//! # Why Azure and not sherpa
//!
//! sherpa is currently unusable from here: `sherpa-rs` 0.6.8 bundles a core that *requires*
//! `kokoro-dict-dir`, and on that failure sherpa-onnx calls `exit()` rather than returning
//! an error — so a sherpa render would take the whole test process down with it, with no
//! recoverable failure to report. Azure is verified working.
//!
//! # Cost control
//!
//! `target_words` is set to a few hundred, not the spec's 2000. One short chapter.
//!
//! # Logging is not optional here
//!
//! The cycle *degrades* rather than failing: a render failure leaves the text published,
//! sets `has_audio = false`, and says why in a `warn!` (§10). With no subscriber installed
//! that reason goes nowhere, and the test reports "audio failed" while destroying the only
//! explanation — the exact silent-failure shape the design exists to prevent. Every test
//! here calls [`init_logging`] first.

use std::sync::{Arc, Mutex};

use litrpg_core::BYTES_PER_MS;
use litrpg_engine::{
    CycleOutcome, EmberGenerator, Engine, EngineConfig, FsArtifacts, RegistryRenderer, StoreLibrary,
};
use litrpg_store::{NewStory, Store};
use litrpg_tts::{TtsBackend, TtsRegistry, azure::AzureBackend};

/// A small all-Azure cast. The Kokoro pool cannot be used here: a `voice_ref` names its
/// backend (§7.3), so a sherpa ref against an Azure-only registry fails at render time.
const AZURE_NARRATOR: &str = "azure:en-GB-Ada:DragonHDLatestNeural";
const AZURE_SYSTEM: &str = "azure:en-US-Steffan:DragonHDLatestNeural";
/// Four character voices, so a four-person cast does not have to double up.
///
/// The en-GB male is `en-GB-Ollie:DragonHDLatestNeural`. It was briefly absent because the
/// curated list carried `en-GB-OllieMultilingual:DragonHDLatestNeural`, which Azure answers
/// **HTTP 400** for — aurora found why: `OllieMultilingual` is a real *non*-DragonHD voice, so
/// that entry was a valid voice name spliced onto the DragonHD suffix, naming nothing. Since
/// `render_all` fails the whole batch, one such name costs an entire chapter's audio.
const AZURE_CHARACTERS: &[&str] = &[
    "azure:en-US-Emma:DragonHDLatestNeural",
    "azure:en-GB-Ollie:DragonHDLatestNeural",
    "azure:en-US-Andrew:DragonHDLatestNeural",
    "azure:en-US-Ava:DragonHDLatestNeural",
];

/// Install a `tracing` subscriber so the cycle's `warn!` for a swallowed failure is visible.
///
/// Idempotent: `try_init` is used because several tests in one binary share a process, and a
/// second install would otherwise panic and mask whatever the test was actually checking.
/// `RUST_LOG` overrides the default.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    // Writes to stderr rather than through `with_test_writer()`: the point is that a live
    // diagnostic is visible unconditionally, not only when libtest happens to be forwarding
    // captured output.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("litrpg_engine=debug,litrpg_tts=debug")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

const PREMISE: &str = "\
Kaelen is a debt-collector for a dead god, working the Ashen Vale where the god's unpaid \
contracts still hold. Grim, dry, close third person, past tense. His associate Sera watches \
the exits and says the thing he does not want to hear.";

#[tokio::test]
#[ignore = "costs GPU time on familiar and Azure credit"]
async fn one_real_chapter_end_to_end() {
    init_logging();
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("story.db");
    let media_dir = dir.path().join("media");
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, PREMISE).expect("write premise");

    // ---- store + story row -------------------------------------------------
    let store = Arc::new(Mutex::new(Store::open(&db_path).expect("open store")));
    store
        .lock()
        .unwrap()
        .insert_story_if_absent(&NewStory {
            title: "The Ashen Ledger".to_string(),
            protagonist: "Kaelen".to_string(),
            prompt_path: prompt_path.to_string_lossy().to_string(),
            prompt_hash: litrpg_core::content_hash(PREMISE),
            target_words: 300,
        })
        .expect("insert story");
    store
        .lock()
        .unwrap()
        .set_arc_outline("Arc 1: break the three seals of the Ashen Vale.")
        .expect("set arc outline");

    // Some lore, so retrieval has something to match on.
    store
        .lock()
        .unwrap()
        .upsert_lore(
            "The Ashen Vale",
            "place",
            "vale,ashen vale",
            "A basin of grey ash where a god died mid-sentence. Nothing rots here; it only \
             dries.",
            10,
            false,
            0,
        )
        .expect("insert lore");

    // ---- ports -------------------------------------------------------------
    let generator = EmberGenerator::from_config(&ember_config()).expect("ember client");

    let azure = match AzureBackend::from_default_config() {
        Ok(b) => b,
        Err(e) => panic!("Azure credentials unavailable ({e}); this test needs them"),
    };
    let registry = TtsRegistry::new().with(Box::new(azure));
    eprintln!("registry backends: {:?}", registry.ids());
    for (id, avail) in registry.availability() {
        eprintln!("  {id}: ready={} ({:?})", avail.is_ready(), avail.reason());
    }

    let library = StoreLibrary::new(Arc::clone(&store));
    let artifacts = FsArtifacts::new(&media_dir);

    let config = EngineConfig {
        buffer_target: 1,
        target_words: 300,
        narrator_voice: AZURE_NARRATOR.to_string(),
        system_voice: AZURE_SYSTEM.to_string(),
        character_voices: AZURE_CHARACTERS.iter().map(|s| s.to_string()).collect(),
        summary_window: 5,
    };

    let engine = Engine::with_shared_store(
        Arc::clone(&store),
        generator,
        RegistryRenderer::new(registry),
        library,
        artifacts,
        config,
    );

    // ---- one cycle ---------------------------------------------------------
    let outcome = engine.run_cycle(0).await.expect("cycle should not error");
    eprintln!("\n=== outcome: {outcome:?}\n");

    let (chapter, has_audio, state_dirty, applied, rejected) = match outcome {
        CycleOutcome::Produced {
            chapter,
            has_audio,
            state_dirty,
            applied,
            rejected,
        } => (chapter, has_audio, state_dirty, applied, rejected),
        CycleOutcome::Abandoned {
            reason, backoff, ..
        } => {
            panic!("pass 1 failed: {reason} (backoff={backoff})")
        }
        other => panic!("expected Produced on an empty database, got {other:?}"),
    };
    assert_eq!(chapter, 1);

    // ---- prose -------------------------------------------------------------
    let row = engine
        .with_store(|s| s.chapter(1))
        .expect("chapter row should exist");
    eprintln!("--- title: {:?}", row.title);
    eprintln!("--- prompt_hash: {}", row.prompt_hash);
    eprintln!(
        "--- text_md ({} bytes):\n{}",
        row.text_md.len(),
        row.text_md
    );

    assert!(
        !row.text_md.trim().is_empty(),
        "the chapter must have prose"
    );
    assert!(
        !row.title.trim().is_empty(),
        "the chapter must have a title"
    );
    assert_eq!(
        row.prompt_hash,
        litrpg_core::content_hash(PREMISE),
        "chapters.prompt_hash must match the story prompt's hash, or §9.3 provenance is \
         broken -- the CLI and the engine have to agree on the algorithm"
    );

    // ---- cast --------------------------------------------------------------
    let cast = engine.with_store(|s| s.cast()).expect("cast");
    eprintln!("--- cast:");
    for c in &cast {
        eprintln!("      {:<14} {:<52} {}", c.speaker, c.voice_ref, c.kind);
    }
    assert!(!cast.is_empty(), "at least the narrator must be cast");
    for c in &cast {
        assert!(
            c.voice_ref.starts_with("azure:"),
            "cast member {} drew a non-Azure voice {} -- the registry could not have \
             rendered it",
            c.speaker,
            c.voice_ref
        );
    }
    // No two speakers share a voice (the narrator/SYSTEM exclusions plus the pool).
    let mut voices: Vec<&String> = cast.iter().map(|c| &c.voice_ref).collect();
    voices.sort();
    let before = voices.len();
    voices.dedup();
    assert_eq!(before, voices.len(), "two speakers share a voice: {cast:?}");

    // ---- state -------------------------------------------------------------
    eprintln!("--- state_dirty={state_dirty} applied={applied} rejected={rejected}");
    let snapshot = engine.with_store(|s| s.snapshot()).expect("snapshot");
    eprintln!("--- snapshot: {:#?}", snapshot.values);
    if !snapshot.anomalies.is_empty() {
        eprintln!("--- ledger anomalies: {:?}", snapshot.anomalies);
    }
    for (code, n) in engine
        .with_store(|s| s.rejection_reasons())
        .expect("rejection reasons")
    {
        eprintln!("--- rejected {n}x: {code}");
    }
    let summaries = engine
        .with_store(|s| s.recent_chapter_summaries(5))
        .expect("summaries");
    eprintln!("--- summary: {:?}", summaries.first().map(|s| &s.body_md));

    // Re-run pass 2 on the published prose purely to *see* what it proposes. Rejections
    // surface from the cycle only as a count, and "2 rejected as UnknownField" is not
    // enough to fix a prompt. Ember-only, so this costs no Azure credit.
    {
        let diag = EmberGenerator::from_config(&ember_config()).expect("ember");
        let known: Vec<String> = engine
            .with_store(|s| s.known_subjects())
            .expect("known subjects")
            .into_iter()
            .collect();
        // Same input the cycle used: tags stripped. Feeding the tagged markdown makes
        // pass 2 treat `SYSTEM` as a character and propose `subject=SYSTEM`.
        let plain = litrpg_engine::plain_chapter_text(&planned_from(&row.text_md));
        match litrpg_engine::Generator::pass2(&diag, &plain, &known).await {
            Ok(e) => {
                eprintln!("--- pass 2 proposed {} deltas:", e.deltas.len());
                for d in &e.deltas {
                    eprintln!(
                        "      subject={:<12} field={:<18} op={:<4} num={:?} txt={:?}",
                        d.subject, d.field, d.op, d.value_num, d.value_txt
                    );
                }
                eprintln!("--- pass 2 proposed {} lore rows:", e.new_lore.len());
                for l in &e.new_lore {
                    eprintln!("      {} ({}) keywords={:?}", l.name, l.kind, l.keywords);
                }
            }
            Err(e) => eprintln!("--- diagnostic pass 2 failed: {e}"),
        }
    }

    if state_dirty {
        eprintln!(
            "!!! pass 2 failed -- the chapter shipped state_dirty, which is the documented \
             §10 degradation, but it means extraction needs looking at"
        );
    } else {
        assert!(
            !summaries.is_empty(),
            "a successful extraction must have written a summary"
        );
    }

    // ---- audio -------------------------------------------------------------
    assert!(
        has_audio,
        "the render failed; see the warn! log above for the reason"
    );

    let row = engine.with_store(|s| s.chapter(1)).expect("chapter row");
    let segments = engine.with_store(|s| s.segments(1)).expect("segments");
    eprintln!(
        "--- audio: duration_ms={} segments={} pcm={:?} mp3={:?}",
        row.duration_ms,
        segments.len(),
        row.pcm_path,
        row.mp3_path
    );

    assert!(!segments.is_empty(), "audio implies segment rows");
    assert_eq!(
        segments[0].start_ms, 0,
        "the first segment must start at zero"
    );
    for w in segments.windows(2) {
        assert_eq!(
            w[0].end_ms, w[1].start_ms,
            "segments must be contiguous or every Range request after the gap is wrong"
        );
    }
    assert_eq!(
        row.duration_ms,
        segments.last().unwrap().end_ms,
        "chapters.duration_ms must agree with the manifest's last segment"
    );

    // The invariant the whole watch playback path rests on, checked against the bytes
    // actually on disk rather than against anything we computed.
    let pcm_path = row.pcm_path.expect("pcm_path must be set when has_audio");
    let pcm_bytes = std::fs::read(&pcm_path).expect("read pcm");
    eprintln!(
        "--- pcm on disk: {} bytes; duration_ms * 32 = {}",
        pcm_bytes.len(),
        row.duration_ms as u64 * BYTES_PER_MS as u64
    );
    assert_eq!(
        pcm_bytes.len() as u64,
        row.duration_ms as u64 * BYTES_PER_MS as u64,
        "duration_ms * 32 != pcm length; every byte offset in the manifest is wrong"
    );

    // Per-segment byte offsets must address the real file.
    for s in &segments {
        assert_eq!(s.start_byte(), s.start_ms as u64 * 32);
        assert!(
            s.end_byte() <= pcm_bytes.len() as u64,
            "segment {} ends past the end of the audio",
            s.idx
        );
    }

    // ---- artifacts ---------------------------------------------------------
    let mp3_path = row.mp3_path.expect("mp3_path must be set when has_audio");
    let mp3 = std::fs::metadata(&mp3_path).expect("mp3 must exist on disk");
    assert!(mp3.len() > 0, "the mp3 is empty");
    eprintln!("--- mp3: {} bytes at {mp3_path}", mp3.len());

    let manifest_path = media_dir.join("0001.json");
    let manifest_json = std::fs::read_to_string(&manifest_path).expect("manifest must exist");
    let manifest: litrpg_core::Manifest =
        serde_json::from_str(&manifest_json).expect("manifest must be valid JSON");
    assert_eq!(manifest.chapter, 1);
    assert_eq!(manifest.sample_rate, 16_000);
    assert_eq!(manifest.bytes_per_ms, 32);
    assert!(manifest.is_contiguous());
    assert_eq!(manifest.duration_ms, row.duration_ms);
    assert_eq!(
        manifest.total_bytes(),
        pcm_bytes.len() as u64,
        "the published manifest disagrees with the published audio"
    );

    // No stray `.part` files from the atomic-write path.
    let leftovers: Vec<String> = std::fs::read_dir(&media_dir)
        .expect("read media dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );

    eprintln!(
        "\n=== a whole chapter exists: {} words of prose, {} segments, {:.1}s of audio, \
         {} cast members, {applied} deltas applied, {rejected} rejected\n",
        row.text_md.split_whitespace().count(),
        segments.len(),
        row.duration_ms as f64 / 1000.0,
        cast.len(),
    );
}

/// Cheap isolation of the TTS leg: one short segment, no Ember, no GPU.
///
/// Exists because a render failure inside the cycle is deliberately swallowed (§10: the
/// text still ships), so `has_audio = false` tells you *that* it failed and nothing about
/// *why*. This prints the actual error.
#[tokio::test]
#[ignore = "costs a few seconds of Azure credit"]
async fn azure_renders_one_segment() {
    init_logging();
    use litrpg_core::SpeakerKind;
    use litrpg_engine::Renderer;
    use litrpg_tts::RenderRequest;

    let azure = AzureBackend::from_default_config().expect("azure credentials");
    eprintln!("azure endpoint: {}", azure.config().endpoint());
    // Fully qualified: `AzureBackend` has a private `voices` field that shadows the
    // trait method in ordinary method-call position.
    eprintln!("azure voices: {}", TtsBackend::voices(&azure).len());

    let renderer = RegistryRenderer::new(TtsRegistry::new().with(Box::new(azure)));

    for voice in std::iter::once(AZURE_NARRATOR)
        .chain(std::iter::once(AZURE_SYSTEM))
        .chain(AZURE_CHARACTERS.iter().copied())
    {
        let req = RenderRequest::parse(
            0,
            voice,
            "The vale smelled of iron and wet ash.",
            SpeakerKind::Narrator,
        )
        .expect("voice_ref should parse");

        match renderer.render_all(std::slice::from_ref(&req)).await {
            Ok(parts) => eprintln!(
                "  OK   {voice} -> {} bytes ({} ms)",
                parts[0].len(),
                parts[0].duration_ms()
            ),
            Err(e) => eprintln!("  FAIL {voice} -> {e}"),
        }
    }
}

/// A second cycle must never rewrite chapter 1's prose, whatever the audio did.
///
/// The load-bearing assertion is deliberately **independent of the render**: a chapter with
/// `has_audio = false` does not count toward the rendered-ahead buffer, so at
/// `buffer_target: 1` the engine is *right* to keep working rather than idle — an earlier
/// version of this test asserted `Idle` unconditionally and failed on the engine being
/// correct. What must hold either way is that prose which has already shipped is never
/// regenerated; the `Idle` check applies only when the buffer genuinely filled.
#[tokio::test]
#[ignore = "costs GPU time on familiar and Azure credit"]
async fn a_second_cycle_never_rewrites_published_prose() {
    init_logging();
    let dir = tempfile::tempdir().expect("temp dir");
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, PREMISE).expect("write premise");

    let store = Arc::new(Mutex::new(
        Store::open(&dir.path().join("story.db")).expect("open store"),
    ));
    store
        .lock()
        .unwrap()
        .insert_story_if_absent(&NewStory {
            title: "The Ashen Ledger".to_string(),
            protagonist: "Kaelen".to_string(),
            prompt_path: prompt_path.to_string_lossy().to_string(),
            prompt_hash: litrpg_core::content_hash(PREMISE),
            target_words: 250,
        })
        .expect("insert story");

    let azure = AzureBackend::from_default_config().expect("azure credentials");
    let engine = Engine::with_shared_store(
        Arc::clone(&store),
        EmberGenerator::from_config(&ember_config()).expect("ember"),
        RegistryRenderer::new(TtsRegistry::new().with(Box::new(azure))),
        StoreLibrary::new(Arc::clone(&store)),
        FsArtifacts::new(dir.path().join("media")),
        EngineConfig {
            buffer_target: 1,
            target_words: 250,
            narrator_voice: AZURE_NARRATOR.to_string(),
            system_voice: AZURE_SYSTEM.to_string(),
            character_voices: AZURE_CHARACTERS.iter().map(|s| s.to_string()).collect(),
            summary_window: 5,
        },
    );

    let first = engine.run_cycle(0).await.expect("first cycle");
    assert_eq!(first.produced_chapter(), Some(1), "got {first:?}");

    let after_first = engine.with_store(|s| s.chapter(1)).expect("chapter 1");
    let first_had_audio = after_first.has_audio;
    eprintln!("first cycle: has_audio={first_had_audio}");

    let second = engine.run_cycle(0).await.expect("second cycle");
    eprintln!("second cycle: {second:?}");

    // The assertion that matters, and it holds regardless of the audio path.
    let after_second = engine
        .with_store(|s| s.chapter(1))
        .expect("chapter 1 again");
    assert_eq!(
        after_second.text_md, after_first.text_md,
        "chapter 1's prose was rewritten -- published history must be immutable"
    );
    assert_eq!(
        after_second.prompt_hash, after_first.prompt_hash,
        "chapter 1's provenance changed"
    );

    match second {
        // Chapter 1 rendered, so the buffer is full at target 1.
        CycleOutcome::Idle { buffer_depth } if first_had_audio => {
            assert_eq!(buffer_depth, 1);
        }
        // Chapter 1 did not render, so it does not count toward the buffer and the engine
        // is correct to carry on: either it re-renders chapter 1 or it writes chapter 2.
        CycleOutcome::ResumedRender { chapter, .. } if !first_had_audio => {
            assert_eq!(chapter, 1, "only the unrendered chapter may be resumed");
        }
        CycleOutcome::Produced { chapter, .. } if !first_had_audio => {
            assert_eq!(chapter, 2, "a new chapter must be the next number");
        }
        other if first_had_audio => {
            panic!("chapter 1 has audio, so the buffer is full; expected Idle, got {other:?}")
        }
        other => panic!("unexpected outcome with an unrendered chapter 1: {other:?}"),
    }
}

/// Rebuild planned segments from stored markdown, for diagnostics only.
fn planned_from(text_md: &str) -> Vec<litrpg_engine::PlannedSegment> {
    let body: String = text_md
        .lines()
        .skip_while(|l| l.trim_start().starts_with('#') || l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    litrpg_ember::parse_tagged_prose(&body)
        .into_iter()
        .map(|s| litrpg_engine::PlannedSegment {
            idx: s.idx,
            speaker: s.speaker,
            kind: s.kind,
            voice_ref: String::new(),
            text: s.text,
        })
        .collect()
}

fn ember_config() -> litrpg_config::Config {
    litrpg_config::Config {
        ember_url: litrpg_ember::DEFAULT_BASE_URL.to_string(),
        ember_model: litrpg_ember::DEFAULT_MODEL.to_string(),
        ..Default::default()
    }
}
