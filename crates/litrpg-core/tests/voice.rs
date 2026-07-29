use litrpg_core::voice::{VoiceRef, VoiceRefError};

#[test]
fn parses_sherpa_ref() {
    let v = VoiceRef::parse("sherpa:piper-en_GB-cori:0").unwrap();
    assert_eq!(v.backend, "sherpa");
    assert_eq!(v.remainder, "piper-en_GB-cori:0");
}

#[test]
fn azure_voice_name_keeps_its_own_colon() {
    // The bug this test exists to prevent: a naive split(':') into three parts
    // truncates the Azure voice name at "en-GB-Ada".
    let v = VoiceRef::parse("azure:en-GB-Ada:DragonHDLatestNeural").unwrap();
    assert_eq!(v.backend, "azure");
    assert_eq!(v.remainder, "en-GB-Ada:DragonHDLatestNeural");
}

#[test]
fn round_trips_through_display() {
    let raw = "azure:en-GB-Ada:DragonHDLatestNeural";
    assert_eq!(VoiceRef::parse(raw).unwrap().to_string(), raw);
}

#[test]
fn rejects_malformed_refs() {
    assert_eq!(VoiceRef::parse("sherpa"), Err(VoiceRefError::MissingColon));
    assert_eq!(VoiceRef::parse(":piper"), Err(VoiceRefError::EmptyBackend));
    assert_eq!(VoiceRef::parse("sherpa:"), Err(VoiceRefError::EmptyRemainder));
}
