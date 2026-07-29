//! The Azure SSML builder and credential loading. Pure string/parse assertions —
//! nothing here touches the network. The live call lives in `azure_live.rs`.

use litrpg_core::SpeakerKind;
use litrpg_tts::azure::{AzureConfig, build_multi_voice_ssml, build_ssml, xml_escape};
use litrpg_tts::{RenderRequest, TtsBackend, TtsError};

fn req(idx: u32, voice_ref: &str, text: &str) -> RenderRequest {
    RenderRequest::parse(idx, voice_ref, text, SpeakerKind::Character).unwrap()
}

// ------------------------------------------------------------------- escaping

#[test]
fn escapes_the_five_xml_entities() {
    assert_eq!(xml_escape("a & b"), "a &amp; b");
    assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
    assert_eq!(xml_escape("say \"hi\""), "say &quot;hi&quot;");
    assert_eq!(xml_escape("don't"), "don&apos;t");
}

#[test]
fn ampersand_is_escaped_before_the_others_so_entities_are_not_double_escaped() {
    // Naive ordering turns `<` into `&lt;` and then the `&` into `&amp;lt;`.
    assert_eq!(xml_escape("<&>"), "&lt;&amp;&gt;");
    assert_eq!(xml_escape("&amp;"), "&amp;amp;");
}

#[test]
fn dialogue_punctuation_survives_the_round_trip_into_ssml() {
    // Apostrophes and quotes are guaranteed to appear in dialogue.
    let ssml = build_ssml(&req(
        0,
        "azure:en-GB-Ada:DragonHDLatestNeural",
        "\"Don't,\" she said & left.",
    ));
    assert!(
        ssml.contains("&quot;Don&apos;t,&quot; she said &amp; left."),
        "escaped body missing from: {ssml}"
    );
    assert!(!ssml.contains("Don't"), "raw apostrophe leaked: {ssml}");
}

// -------------------------------------------------------------- single segment

#[test]
fn single_segment_ssml_is_well_formed_and_has_exactly_one_voice() {
    let ssml = build_ssml(&req(
        0,
        "azure:en-GB-Ada:DragonHDLatestNeural",
        "The vale smelled of iron.",
    ));
    assert!(ssml.starts_with("<speak version=\"1.0\""));
    assert!(ssml.contains("xmlns=\"http://www.w3.org/2001/10/synthesis\""));
    assert!(ssml.ends_with("</speak>"));
    assert_eq!(ssml.matches("<voice name=").count(), 1);
    assert!(ssml.contains("<voice name=\"en-GB-Ada:DragonHDLatestNeural\">"));
    assert!(ssml.contains("The vale smelled of iron.</voice>"));
}

#[test]
fn the_voice_name_is_the_remainder_after_the_first_colon() {
    // `split_once(':')` — the DragonHD suffix is part of the Azure voice name and
    // must survive intact. A naive three-way split would send "en-GB-Ada".
    let ssml = build_ssml(&req(0, "azure:en-GB-Ada:DragonHDLatestNeural", "x"));
    assert!(ssml.contains("name=\"en-GB-Ada:DragonHDLatestNeural\""));
}

// --------------------------------------------------------------- multi segment

#[test]
fn multi_voice_ssml_emits_one_voice_element_per_segment_in_order() {
    let reqs = vec![
        req(0, "azure:en-GB-Ada:DragonHDLatestNeural", "Narration."),
        req(1, "azure:en-US-Ava:DragonHDLatestNeural", "Dialogue."),
        req(2, "azure:en-GB-Ada:DragonHDLatestNeural", "More narration."),
    ];
    let ssml = build_multi_voice_ssml(&reqs);

    assert_eq!(ssml.matches("<voice name=").count(), 3);
    assert_eq!(ssml.matches("<speak").count(), 1);
    assert_eq!(ssml.matches("</speak>").count(), 1);

    let ada = ssml.find("Narration.").unwrap();
    let ava = ssml.find("Dialogue.").unwrap();
    let more = ssml.find("More narration.").unwrap();
    assert!(ada < ava && ava < more, "segment order must be preserved");
    assert!(ssml.contains("<voice name=\"en-US-Ava:DragonHDLatestNeural\">Dialogue.</voice>"));
}

#[test]
fn empty_input_yields_a_valid_but_voiceless_document() {
    let ssml = build_multi_voice_ssml(&[]);
    assert!(ssml.starts_with("<speak"));
    assert!(ssml.ends_with("</speak>"));
    assert!(!ssml.contains("<voice"));
}

#[test]
fn segments_with_blank_text_are_skipped_not_emitted_as_empty_voices() {
    let reqs = vec![
        req(0, "azure:en-GB-Ada:DragonHDLatestNeural", "Kept."),
        req(1, "azure:en-GB-Ada:DragonHDLatestNeural", "   "),
        req(2, "azure:en-GB-Ada:DragonHDLatestNeural", ""),
    ];
    let ssml = build_multi_voice_ssml(&reqs);
    assert_eq!(ssml.matches("<voice name=").count(), 1);
    assert!(ssml.contains("Kept."));
}

#[test]
fn the_system_kind_gets_no_special_ssml_treatment_on_azure() {
    // Azure has real expressive voices; the ffmpeg robot chain is a sherpa-only
    // workaround. SYSTEM on Azure is just a voice choice.
    let r = RenderRequest::parse(
        0,
        "azure:en-US-Ava:DragonHDLatestNeural",
        "You have gained a level.",
        SpeakerKind::System,
    )
    .unwrap();
    let ssml = build_ssml(&r);
    assert_eq!(ssml.matches("<voice name=").count(), 1);
    assert!(!ssml.contains("<prosody"));
}

// ------------------------------------------------------------ voice validation

#[test]
fn a_voice_name_that_could_inject_markup_is_rejected_not_silently_defaulted() {
    // Substituting a default voice would ship the wrong narrator silently; the
    // pipeline would rather fail at cast-assignment time.
    let r = req(0, "azure:bad\"><voice name=\"evil", "x");
    match AzureConfig::validate_voice_name(&r.voice.remainder) {
        Err(TtsError::InvalidVoiceName(v)) => assert!(v.contains("evil")),
        other => panic!("expected InvalidVoiceName, got {other:?}"),
    }
}

#[test]
fn legitimate_azure_voice_names_pass_validation() {
    for v in [
        "en-GB-Ada:DragonHDLatestNeural",
        "en-US-Ava:DragonHDLatestNeural",
        "en-US-AvaNeural",
        "en-Multitalker",
    ] {
        assert!(
            AzureConfig::validate_voice_name(v).is_ok(),
            "rejected a real Azure voice: {v}"
        );
    }
}

// -------------------------------------------------------------- config loading

#[test]
fn reads_tts_key_and_tts_region_from_the_speech_to_cli_config() {
    let json = r#"{
        "key": "stt-key-value",
        "tts_key": "tts-key-value",
        "region": "westus",
        "tts_region": "eastus",
        "voice": "en-US-Ava:DragonHDLatestNeural"
    }"#;
    let cfg = AzureConfig::from_json_str(json).unwrap();
    assert_eq!(cfg.region, "eastus", "tts_region wins over region");
    assert_eq!(cfg.key, "tts-key-value", "tts_key wins over key");
    assert_eq!(cfg.default_voice, "en-US-Ava:DragonHDLatestNeural");
}

#[test]
fn tts_key_falls_back_to_key_and_tts_region_to_region() {
    // This is the shape of JP's real config: `key` + `tts_region`, no `tts_key`.
    let json = r#"{"key": "shared-key", "region": "westus", "tts_region": "eastus"}"#;
    let cfg = AzureConfig::from_json_str(json).unwrap();
    assert_eq!(cfg.key, "shared-key");
    assert_eq!(cfg.region, "eastus");

    let json = r#"{"key": "shared-key", "region": "westus"}"#;
    let cfg = AzureConfig::from_json_str(json).unwrap();
    assert_eq!(cfg.region, "westus");
}

#[test]
fn a_config_without_any_key_is_an_error_not_an_empty_key() {
    let json = r#"{"region": "westus"}"#;
    assert!(matches!(
        AzureConfig::from_json_str(json),
        Err(TtsError::MissingCredential(_))
    ));
}

#[test]
fn debug_never_prints_the_key() {
    let json = r#"{"key": "SUPER-SECRET-abcdef123456", "tts_region": "eastus"}"#;
    let cfg = AzureConfig::from_json_str(json).unwrap();
    let dbg = format!("{cfg:?}");
    assert!(
        !dbg.contains("SUPER-SECRET"),
        "the key leaked into Debug output: {dbg}"
    );
    assert!(
        dbg.contains("redacted"),
        "expected a redaction marker: {dbg}"
    );
    // Region and voice are not secret and should stay visible for diagnostics.
    assert!(dbg.contains("eastus"));
}

#[test]
fn the_endpoint_is_the_region_scoped_cognitiveservices_v1_url() {
    let json = r#"{"key": "k", "tts_region": "eastus"}"#;
    let cfg = AzureConfig::from_json_str(json).unwrap();
    assert_eq!(
        cfg.endpoint(),
        "https://eastus.tts.speech.microsoft.com/cognitiveservices/v1"
    );
}

#[test]
fn the_output_format_header_requests_raw_16khz_pcm_so_no_decode_is_needed() {
    assert_eq!(
        litrpg_tts::azure::OUTPUT_FORMAT,
        "raw-16khz-16bit-mono-pcm",
        "16 kHz native from DragonHD is why the Azure path has no resampler"
    );
}

// ---------------------------------------------- cast preflight (no network here)

fn backend_for_preflight() -> litrpg_tts::azure::AzureBackend {
    litrpg_tts::azure::AzureBackend::new(
        AzureConfig::from_json_str(r#"{"key":"k","tts_region":"eastus"}"#).unwrap(),
    )
}

/// A stand-in for Azure's real catalog response.
fn catalog() -> Vec<String> {
    [
        "en-GB-Ada:DragonHDLatestNeural",
        "en-GB-Ollie:DragonHDLatestNeural",
        "en-GB-OllieMultilingualNeural",
        "en-US-Ava:DragonHDLatestNeural",
        "en-US-AvaNeural",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn preflight_accepts_a_voice_azure_actually_lists() {
    let v =
        backend_for_preflight().preflight_one("azure:en-GB-Ada:DragonHDLatestNeural", &catalog());
    assert!(v.is_ok(), "{v}");
}

#[test]
fn preflight_catches_the_invented_voice_that_cost_a_chapter() {
    // `en-GB-OllieMultilingual:DragonHDLatestNeural` was `en-GB-OllieMultilingualNeural`
    // (real, non-DragonHD) spliced onto the DragonHD suffix. Azure answers 400. This
    // is the whole point of the preflight: catch it at cast-assignment time, for one
    // catalog request, instead of losing a chapter's audio at render time.
    let v = backend_for_preflight().preflight_one(
        "azure:en-GB-OllieMultilingual:DragonHDLatestNeural",
        &catalog(),
    );
    match &v {
        litrpg_tts::VoicePreflight::NotInCatalog {
            region, suggestion, ..
        } => {
            assert_eq!(region, "eastus");
            assert!(
                suggestion.is_some(),
                "a near-miss should suggest the real name: {v}"
            );
        }
        other => panic!("expected NotInCatalog, got {other:?}"),
    }
    // And the message is actionable.
    assert!(v.to_string().contains("not in the Azure catalog"), "{v}");
}

#[test]
fn preflight_rejects_a_ref_owned_by_another_backend() {
    let v = backend_for_preflight().preflight_one("sherpa:piper-en_GB-cori-high:0", &catalog());
    assert!(matches!(v, litrpg_tts::VoicePreflight::WrongBackend { .. }));
    assert!(!v.is_ok());
}

#[test]
fn preflight_rejects_malformed_and_unsafe_refs() {
    let b = backend_for_preflight();
    assert!(matches!(
        b.preflight_one("no-colon", &catalog()),
        litrpg_tts::VoicePreflight::Malformed { .. }
    ));
    assert!(matches!(
        b.preflight_one("azure:bad\"><voice", &catalog()),
        litrpg_tts::VoicePreflight::Malformed { .. }
    ));
}

#[test]
fn every_curated_voice_would_pass_preflight_against_a_catalog_containing_it() {
    // Guards the shape of the table: fully-qualified, attribute-safe, azure-owned.
    let b = backend_for_preflight();
    let full: Vec<String> = b
        .voices()
        .iter()
        .map(|v| v.voice_ref.trim_start_matches("azure:").to_string())
        .collect();
    for v in b.voices() {
        let verdict = b.preflight_one(&v.voice_ref, &full);
        assert!(verdict.is_ok(), "curated voice failed preflight: {verdict}");
    }
}

#[test]
fn the_curated_table_no_longer_contains_the_invented_ollie_name() {
    // Regression guard for the exact entry that cost a chapter's audio.
    let b = backend_for_preflight();
    assert!(
        !b.voices()
            .iter()
            .any(|v| v.voice_ref.contains("OllieMultilingual")),
        "en-GB-OllieMultilingual:DragonHDLatestNeural does not exist in Azure"
    );
    assert!(
        b.voices()
            .iter()
            .any(|v| v.voice_ref == "azure:en-GB-Ollie:DragonHDLatestNeural"),
        "the real en-GB-Ollie DragonHD voice should be advertised"
    );
}

#[test]
fn the_fallback_is_off_by_default() {
    // Voices are persisted per character (spec §7.3); silently recasting one breaks
    // the continuity that persistence exists to provide. Opt in deliberately.
    let b = backend_for_preflight();
    assert!(!b.fallback_on_reject(), "must be opt-in");
    assert!(b.with_fallback_on_reject(true).fallback_on_reject());
}

// ---------------------------------------------------- the curated character pool

#[test]
fn the_pool_is_large_enough_that_a_big_cast_keeps_distinct_voices() {
    // Was 8 entries -> 6 usable for characters, and a four-character chapter already
    // had two characters drawing the same voice. The cast table's whole value is that
    // a voice stays a person's voice.
    let n = backend_for_preflight().voices().len();
    assert!(n >= 16, "character pool is only {n} voices; needs >= 16");
}

#[test]
fn the_pool_is_balanced_across_gender_and_accent() {
    // The engine's assigner interleaves gender and accent groups, so a lopsided pool
    // defeats it and characters double up on the heavy side.
    use litrpg_tts::Gender;
    let voices = backend_for_preflight().voices();
    let f = voices.iter().filter(|v| v.gender == Gender::Female).count();
    let m = voices.iter().filter(|v| v.gender == Gender::Male).count();
    let gb = voices.iter().filter(|v| v.lang == "en-GB").count();
    let us = voices.iter().filter(|v| v.lang == "en-US").count();

    assert!(f >= 6 && m >= 6, "unbalanced gender: {f}F / {m}M");
    assert!(f.abs_diff(m) <= 2, "gender skew too large: {f}F / {m}M");
    assert!(gb >= 4, "only {gb} en-GB voices; accent variety is scarce");
    assert!(us >= 8, "only {us} en-US voices");
}

#[test]
fn no_voice_appears_twice_in_the_pool() {
    let voices = backend_for_preflight().voices();
    let mut seen = std::collections::HashSet::new();
    for v in &voices {
        assert!(
            seen.insert(v.voice_ref.clone()),
            "duplicate: {}",
            v.voice_ref
        );
    }
}

#[test]
fn preview_and_numeric_variants_are_excluded_from_the_pool() {
    // All verified working, all excluded on purpose: two near-identical voices are
    // worse than a smaller pool, because the assigner hands out both and they read as
    // one person. Distinguishing them needs a listening test.
    for v in backend_for_preflight().voices() {
        let name = v.voice_ref.trim_start_matches("azure:");
        let stem = name.split(':').next().unwrap();
        assert!(
            !stem.ends_with("-Preview"),
            "preview variant in the pool: {name}"
        );
        assert!(
            !stem.chars().last().unwrap().is_ascii_digit(),
            "numeric-suffix variant in the pool: {name}"
        );
    }
}

#[test]
fn only_one_dragonhd_series_is_in_the_default_pool() {
    // Flash and Omni are verified working but kept out: mixing model series inside one
    // chapter is a timbre-and-loudness consistency risk of the same kind the fused
    // loudnorm defect turned out to be.
    for v in backend_for_preflight().voices() {
        assert!(
            v.voice_ref.contains("DragonHDLatestNeural"),
            "mixed model series in the default pool: {}",
            v.voice_ref
        );
        assert!(!v.voice_ref.contains("DragonHDFlash"));
        assert!(!v.voice_ref.contains("DragonHDOmni"));
    }
}

#[test]
fn davis_is_excluded_because_it_is_jps_orchestrator_voice() {
    // Not a technical constraint: hearing it narrate a character would muddy the
    // by-voice classification JP relies on when agents speak.
    assert!(
        !backend_for_preflight()
            .voices()
            .iter()
            .any(|v| v.voice_ref.contains("Davis")),
        "en-US-Davis is reserved as the orchestrator voice"
    );
}

#[test]
fn ada_stays_available_as_the_narrator() {
    // Mirror image of the Davis exclusion: Ada is Ember's roster voice, so the local
    // model narrating in it preserves the accent signature rather than violating it.
    assert!(
        backend_for_preflight()
            .voices()
            .iter()
            .any(|v| v.voice_ref == "azure:en-GB-Ada:DragonHDLatestNeural"),
        "en-GB-Ada must remain assignable as the narrator"
    );
}

#[test]
fn multitalker_and_novelty_ids_are_not_in_the_pool() {
    for v in backend_for_preflight().voices() {
        assert!(!v.voice_ref.contains("Multitalker"), "{}", v.voice_ref);
        let stem = v.voice_ref.trim_start_matches("azure:");
        assert!(
            stem.starts_with("en-US-") || stem.starts_with("en-GB-"),
            "unexpected voice id shape: {stem}"
        );
    }
}
