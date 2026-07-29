//! The sherpa cast table and voice-reference parsing.
//!
//! Deliberately **not** behind the `sherpa` feature: none of this needs a model
//! or a native library, and treating voices as config rather than code is what
//! lets the narrator change without a code change (spec §4.4 assumption A1).

use litrpg_core::SpeakerKind;
use litrpg_tts::sherpa::{ModelFamily, SherpaConfig, VoiceSel};
use litrpg_tts::{PostProcess, RenderRequest, TtsError};

// ------------------------------------------------------- voice_ref remainders

#[test]
fn a_remainder_is_model_id_then_sid() {
    let v = VoiceSel::parse("kokoro-multi-lang-v1_0:18").unwrap();
    assert_eq!(v.model_id, "kokoro-multi-lang-v1_0");
    assert_eq!(v.sid, 18);
}

#[test]
fn a_single_speaker_model_still_names_its_sid() {
    let v = VoiceSel::parse("piper-en_GB-cori:0").unwrap();
    assert_eq!(v.model_id, "piper-en_GB-cori");
    assert_eq!(v.sid, 0);
}

#[test]
fn a_remainder_with_no_sid_defaults_to_speaker_zero() {
    // Convenience for single-speaker models like cori.
    let v = VoiceSel::parse("piper-en_GB-cori").unwrap();
    assert_eq!(v.model_id, "piper-en_GB-cori");
    assert_eq!(v.sid, 0);
}

#[test]
fn a_non_numeric_sid_is_a_typed_error_not_a_silent_zero() {
    // Silently rendering speaker 0 would ship the wrong voice for the whole
    // chapter and read as a casting bug, not a config bug.
    match VoiceSel::parse("kokoro-multi-lang-v1_0:bm_george") {
        Err(TtsError::UnknownVoice { voice, reason, .. }) => {
            assert!(voice.contains("bm_george"));
            assert!(
                reason.contains("sid"),
                "reason should name the sid: {reason}"
            );
        }
        other => panic!("expected UnknownVoice, got {other:?}"),
    }
}

#[test]
fn a_negative_sid_is_rejected() {
    assert!(VoiceSel::parse("kokoro-multi-lang-v1_0:-1").is_err());
}

#[test]
fn the_full_voice_ref_is_parsed_through_the_backend_id_first() {
    // Two-stage parse: litrpg-core splits off "sherpa", the plugin owns the rest.
    let r = RenderRequest::parse(
        0,
        "sherpa:kokoro-multi-lang-v1_0:18",
        "Hello.",
        SpeakerKind::Character,
    )
    .unwrap();
    assert_eq!(r.voice.backend, "sherpa");
    assert_eq!(r.voice.remainder, "kokoro-multi-lang-v1_0:18");
    let v = VoiceSel::parse(r.voice_remainder()).unwrap();
    assert_eq!(v.sid, 18);
}

// ---------------------------------------------------------------- default cast

#[test]
fn the_default_narrator_is_cori_high_at_sid_zero() {
    // cori-high runs at 7.55x RTF — faster than Kokoro's 5.28x — so the narrator,
    // the largest share of any chapter, can use the better variant without
    // becoming the bottleneck (~103 s for a 13-minute narration).
    let cfg = SherpaConfig::default();
    let narrator = cfg.narrator_voice_ref();
    assert_eq!(narrator, "sherpa:piper-en_GB-cori-high:0");

    let v = VoiceSel::parse(narrator.strip_prefix("sherpa:").unwrap()).unwrap();
    assert_eq!(v.sid, 0, "cori is single-speaker: sid 0 only");
    let model = cfg
        .model(&v.model_id)
        .expect("cori model must be configured");
    assert_eq!(model.family, ModelFamily::Piper);
    assert_eq!(
        model.native_rate, 22_050,
        "both cori variants are 22.05 kHz"
    );
    assert_eq!(model.speakers, 1);
}

#[test]
fn both_cori_variants_are_configured_and_only_medium_and_high_exist() {
    // `-low` does not exist upstream (404), so it must not be in the table.
    let cfg = SherpaConfig::default();
    let medium = cfg.model("piper-en_GB-cori").expect("cori-medium");
    let high = cfg.model("piper-en_GB-cori-high").expect("cori-high");
    assert_eq!(medium.dir, "vits-piper-en_GB-cori-medium");
    assert_eq!(high.dir, "vits-piper-en_GB-cori-high");
    for m in [medium, high] {
        assert_eq!(m.native_rate, 22_050);
        assert_eq!(m.speakers, 1);
    }
    assert!(
        cfg.models().iter().all(|m| !m.dir.contains("cori-low")),
        "cori-low does not exist upstream"
    );
}

#[test]
fn cori_is_labelled_as_uk_english_female() {
    // Flagged so a male-narrator assumption cannot be discovered late.
    let cfg = SherpaConfig::default();
    for id in ["piper-en_GB-cori:0", "piper-en_GB-cori-high:0"] {
        let v = cfg.voices().iter().find(|v| v.voice == id).unwrap();
        assert_eq!(v.lang, "en-GB");
        assert_eq!(v.gender, litrpg_tts::Gender::Female);
    }
}

#[test]
fn the_narrator_can_be_pinned_to_cori_medium_for_faster_renders() {
    // 25.03x vs 7.55x RTF, when a preview matters more than quality.
    let cfg = SherpaConfig::from_json_str(r#"{"narrator": "piper-en_GB-cori:0"}"#).unwrap();
    assert_eq!(cfg.narrator_voice_ref(), "sherpa:piper-en_GB-cori:0");
}

#[test]
fn the_kokoro_english_cast_is_present_with_reveries_measured_sids() {
    let cfg = SherpaConfig::default();
    let by_ref = |r: &str| cfg.voices().iter().any(|v| v.voice == r);
    // Spot-check the four Reverie called out as a starting cast.
    assert!(by_ref("kokoro-multi-lang-v1_0:3"), "af_heart");
    assert!(by_ref("kokoro-multi-lang-v1_0:18"), "am_puck");
    assert!(by_ref("kokoro-multi-lang-v1_0:26"), "bm_george");
    assert!(by_ref("kokoro-multi-lang-v1_0:27"), "bm_lewis");
    // And the two named as the A1 fallback for the narrator.
    assert!(by_ref("kokoro-multi-lang-v1_0:21"), "bf_emma");
}

#[test]
fn every_default_voice_resolves_to_a_configured_model() {
    let cfg = SherpaConfig::default();
    for v in cfg.voices() {
        let sel = VoiceSel::parse(&v.voice).unwrap_or_else(|e| panic!("{}: {e}", v.voice));
        let model = cfg
            .model(&sel.model_id)
            .unwrap_or_else(|| panic!("{} names an unconfigured model", v.voice));
        assert!(
            (sel.sid as u32) < model.speakers,
            "{} exceeds {}'s {} speakers",
            v.voice,
            model.id,
            model.speakers
        );
    }
}

#[test]
fn kokoro_is_configured_at_24k_and_53_speakers() {
    let cfg = SherpaConfig::default();
    let m = cfg.model("kokoro-multi-lang-v1_0").unwrap();
    assert_eq!(m.family, ModelFamily::Kokoro);
    assert_eq!(m.native_rate, 24_000);
    assert_eq!(m.speakers, 53);
}

// ---------------------------------------------- config, not code (assumption A1)

#[test]
fn the_narrator_can_be_swapped_to_kokoro_with_no_code_change() {
    // Assumption A1 in spec §4.4: if cori sourcing had failed, bf_emma (21) or
    // bm_george (26) takes over. That must be a config edit only.
    let json = r#"{
        "narrator": "kokoro-multi-lang-v1_0:21",
        "workers": 2,
        "threads_per_worker": 4
    }"#;
    let cfg = SherpaConfig::from_json_str(json).unwrap();
    assert_eq!(cfg.narrator_voice_ref(), "sherpa:kokoro-multi-lang-v1_0:21");
    assert_eq!(cfg.workers, 2);
    // Unspecified fields keep their defaults, so a partial override is legal.
    assert!(cfg.model("piper-en_GB-cori").is_some());
}

#[test]
fn a_config_can_add_a_model_and_voices_the_binary_never_heard_of() {
    let json = r#"{
        "models": [
            {"id": "piper-en_US-libritts_r", "family": "piper",
             "dir": "vits-piper-en_US-libritts_r-medium",
             "native_rate": 22050, "speakers": 904}
        ],
        "voices": [
            {"voice": "piper-en_US-libritts_r:500", "label": "incidental-a",
             "lang": "en-US", "gender": "unknown"}
        ]
    }"#;
    let cfg = SherpaConfig::from_json_str(json).unwrap();
    let m = cfg.model("piper-en_US-libritts_r").unwrap();
    assert_eq!(m.speakers, 904);
    assert!(cfg.voices().iter().any(|v| v.label == "incidental-a"));
}

#[test]
fn advertised_voice_refs_are_fully_qualified_and_free() {
    use litrpg_tts::CostClass;
    let descs = SherpaConfig::default().voice_descs();
    assert!(!descs.is_empty());
    for d in &descs {
        assert!(
            d.voice_ref.starts_with("sherpa:"),
            "not fully qualified: {}",
            d.voice_ref
        );
        assert_eq!(
            d.cost_class,
            CostClass::Free,
            "local inference is unmetered"
        );
    }
}

// --------------------------------------------------------- throughput settings

#[test]
fn the_pool_defaults_match_the_measured_optimum_not_the_core_count() {
    // Reverie: 4 workers x 4 threads beat 1 x 8 by 48-86%, and 12 threads
    // regressed from contention with the GPU-resident llama.cpp service.
    let cfg = SherpaConfig::default();
    assert_eq!(cfg.workers, 4);
    assert_eq!(cfg.threads_per_worker, 4);
    assert_eq!(cfg.provider, "cpu", "the GPU belongs to qwen3-coder");
    assert!(
        cfg.threads_per_worker <= 8,
        "never set threads to core count"
    );
}

#[test]
fn worker_count_is_clamped_to_at_least_one() {
    let cfg = SherpaConfig::from_json_str(r#"{"workers": 0}"#).unwrap();
    assert_eq!(cfg.workers, 1);
}

#[test]
fn workers_are_not_sharded_by_model() {
    // Reverie measured zero reload penalty for alternating cori and Kokoro in one
    // process (spike Part 2 §2.7), so every worker takes any segment. Sharding by
    // model would idle workers on a dialogue-heavy or narration-heavy chapter.
    let cfg = SherpaConfig::default();
    assert!(
        !cfg.shard_by_model,
        "shard by segment, not by model - measured, spike Part 2 §2.7"
    );
}

// ------------------------------------------------------------- SYSTEM stage

#[test]
fn the_system_kind_selects_the_ffmpeg_colouring_stage() {
    // SYSTEM is a post-render stage, not a voice: no model ships a robotic
    // speaker, so a neutral speaker is coloured after synthesis.
    assert!(PostProcess::for_kind(SpeakerKind::System).system_fx);
    assert!(!PostProcess::for_kind(SpeakerKind::Narrator).system_fx);
    assert!(!PostProcess::for_kind(SpeakerKind::Character).system_fx);
}

#[test]
fn every_kind_is_loudness_normalized() {
    // The 4.1 LU spread across engines is audible at joins; normalize always.
    for k in [
        SpeakerKind::Narrator,
        SpeakerKind::Character,
        SpeakerKind::System,
    ] {
        assert!(PostProcess::for_kind(k).loudnorm, "{k:?} not normalized");
    }
}

#[test]
fn the_default_system_speaker_is_a_neutral_configured_voice() {
    let cfg = SherpaConfig::default();
    let sel = VoiceSel::parse(cfg.system_voice_ref().strip_prefix("sherpa:").unwrap()).unwrap();
    assert!(cfg.model(&sel.model_id).is_some());
}

// ------------------------------------------------------------------- sharding

#[test]
fn sharding_is_round_robin_over_segments_and_covers_every_index_once() {
    use litrpg_tts::sherpa::shard;
    let buckets = shard(10, 4);
    assert_eq!(buckets.len(), 4);
    assert_eq!(buckets[0], vec![0, 4, 8]);
    assert_eq!(buckets[1], vec![1, 5, 9]);
    assert_eq!(buckets[2], vec![2, 6]);
    assert_eq!(buckets[3], vec![3, 7]);

    let mut all: Vec<usize> = buckets.into_iter().flatten().collect();
    all.sort_unstable();
    assert_eq!(
        all,
        (0..10).collect::<Vec<_>>(),
        "no segment lost or doubled"
    );
}

#[test]
fn sharding_never_creates_idle_workers_or_panics_on_small_batches() {
    use litrpg_tts::sherpa::shard;
    assert_eq!(shard(1, 4).len(), 1, "one segment needs one worker");
    assert_eq!(shard(3, 4).len(), 3);
    assert_eq!(shard(0, 4).len(), 1);
    assert!(shard(0, 4)[0].is_empty());
    assert_eq!(shard(5, 1), vec![vec![0, 1, 2, 3, 4]]);
    assert!(
        !shard(5, 0).is_empty(),
        "worker count is clamped, never zero"
    );
}

#[test]
fn round_robin_spreads_clustered_work_rather_than_chunking_it() {
    use litrpg_tts::sherpa::shard;
    // A chapter's long narration blocks cluster together; contiguous chunks
    // would hand one worker all of them.
    let buckets = shard(8, 4);
    for b in &buckets {
        assert_eq!(b.len(), 2, "even split");
        assert_eq!(b[1] - b[0], 4, "each worker gets non-adjacent segments");
    }
}

// ------------------------------------------------------------ kokoro lexicons

#[test]
fn exactly_one_english_kokoro_lexicon_is_configured() {
    // sherpa's Kokoro lexicon is keyed by word with no language dimension, so
    // loading both lexicon-gb-en.txt and lexicon-us-en.txt logs
    // "Duplicated word: ... Ignore it." for every shared word and silently keeps
    // whichever loaded first. Globbing sorted filenames put gb-en first, which gave
    // the American voices British phonemes. Observed live 2026-07-29.
    let files = SherpaConfig::default().kokoro_lexicon_files.clone();
    let english: Vec<&String> = files.iter().filter(|f| f.contains("-en.")).collect();
    assert_eq!(
        english.len(),
        1,
        "exactly one English lexicon, got {english:?}"
    );
    assert_eq!(english[0], "lexicon-us-en.txt", "the benchmarked pairing");
    assert!(
        files.iter().any(|f| f == "lexicon-zh.txt"),
        "the multi-lang model wants its zh lexicon too"
    );
}

#[test]
fn the_lexicon_list_is_order_preserving_and_overridable() {
    // Load order decides which duplicate wins, so it must not be re-sorted.
    let json = r#"{"kokoro_lexicon_files": ["lexicon-gb-en.txt", "lexicon-zh.txt"]}"#;
    let cfg = SherpaConfig::from_json_str(json).unwrap();
    assert_eq!(
        cfg.kokoro_lexicon_files,
        vec!["lexicon-gb-en.txt", "lexicon-zh.txt"],
        "a British-majority cast can swap the English lexicon in config"
    );
}

#[test]
fn absent_lexicon_files_are_skipped_rather_than_passed_as_bad_paths() {
    // Relative entries resolve against the process CWD, not the model dir, so a
    // non-existent path must never reach sherpa.
    let json = r#"{"model_root": "/definitely/not/here"}"#;
    let cfg = SherpaConfig::from_json_str(json).unwrap();
    let m = cfg.model("kokoro-multi-lang-v1_0").unwrap();
    assert_eq!(
        cfg.kokoro_lexicons(m),
        "",
        "missing files must drop out, not produce dangling paths"
    );
}

// --------------------------------------------------------------- model paths

#[test]
fn model_paths_are_resolved_under_the_model_root() {
    let json = r#"{"model_root": "/srv/models"}"#;
    let cfg = SherpaConfig::from_json_str(json).unwrap();
    let m = cfg.model("kokoro-multi-lang-v1_0").unwrap();
    assert_eq!(
        cfg.model_dir(m),
        std::path::Path::new("/srv/models/kokoro-multi-lang-v1_0")
    );
}

#[test]
fn a_missing_model_root_is_reported_as_unavailable_with_the_path() {
    let json = r#"{"model_root": "/definitely/not/here"}"#;
    let cfg = SherpaConfig::from_json_str(json).unwrap();
    let avail = cfg.availability();
    assert!(!avail.is_ready());
    let reason = avail.reason().unwrap();
    assert!(
        reason.contains("/definitely/not/here"),
        "the reason must name the path so it is fixable: {reason}"
    );
}
