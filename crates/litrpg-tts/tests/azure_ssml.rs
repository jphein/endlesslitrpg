//! The Azure SSML builder and credential loading. Pure string/parse assertions —
//! nothing here touches the network. The live call lives in `azure_live.rs`.

use litrpg_core::SpeakerKind;
use litrpg_tts::azure::{AzureConfig, build_multi_voice_ssml, build_ssml, xml_escape};
use litrpg_tts::{RenderRequest, TtsError};

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
    let ssml = build_ssml(&req(0, "azure:en-GB-Ada:DragonHDLatestNeural", "\"Don't,\" she said & left."));
    assert!(
        ssml.contains("&quot;Don&apos;t,&quot; she said &amp; left."),
        "escaped body missing from: {ssml}"
    );
    assert!(!ssml.contains("Don't"), "raw apostrophe leaked: {ssml}");
}

// -------------------------------------------------------------- single segment

#[test]
fn single_segment_ssml_is_well_formed_and_has_exactly_one_voice() {
    let ssml = build_ssml(&req(0, "azure:en-GB-Ada:DragonHDLatestNeural", "The vale smelled of iron."));
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
    assert!(dbg.contains("redacted"), "expected a redaction marker: {dbg}");
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
