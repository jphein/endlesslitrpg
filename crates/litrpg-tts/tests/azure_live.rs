//! Live Azure calls. **All `#[ignore]`** — they spend real quota.
//!
//! ```text
//! cargo test -p litrpg-tts --test azure_live -- --ignored --nocapture
//! ```
//! Kept deliberately short (a few words each) so a verification run costs
//! effectively nothing.

use litrpg_core::SpeakerKind;
use litrpg_tts::azure::AzureBackend;
use litrpg_tts::{Availability, Pcm16k, RenderRequest, TtsBackend};

fn backend() -> AzureBackend {
    AzureBackend::from_default_config().expect("azure credentials not resolvable")
}

fn req(idx: u32, voice: &str, text: &str, kind: SpeakerKind) -> RenderRequest {
    RenderRequest::parse(idx, voice, text, kind).unwrap()
}

#[test]
#[ignore = "reads ~/.config/speech-to-cli/config.json; environment-dependent"]
fn credentials_resolve_from_the_speech_to_cli_config() {
    let b = backend();
    assert!(matches!(b.available(), Availability::Ready));
    assert!(!b.voices().is_empty());
    // Never assert on the key itself; assert only that Debug stays clean.
    assert!(!format!("{:?}", b.config()).contains("Ocp-Apim"));
}

#[tokio::test]
#[ignore = "spends Azure quota"]
async fn one_short_render_returns_raw_16k_pcm() {
    let b = backend();
    let pcm = b
        .render(&req(
            0,
            "azure:en-GB-Ada:DragonHDLatestNeural",
            "The vale smelled of iron.",
            SpeakerKind::Narrator,
        ))
        .await
        .unwrap();

    assert!(!pcm.is_empty(), "empty PCM back from Azure");
    assert_eq!(pcm.len() % 2, 0, "odd byte count means a truncated stream");
    assert_ne!(&pcm.as_bytes()[..4], b"RIFF", "expected headerless PCM");

    // ~1-3 s of speech for this sentence.
    let ms = pcm.duration_ms();
    assert!((500..8_000).contains(&ms), "implausible duration: {ms} ms");

    let peak = pcm
        .as_bytes()
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 1_000, "audio looks like silence, peak {peak}");
    eprintln!("azure render: {} B = {} ms, peak {peak}", pcm.len(), ms);
}

#[tokio::test]
#[ignore = "spends Azure quota"]
async fn one_multi_voice_request_returns_a_single_joined_stream() {
    let b = backend();
    let reqs = vec![
        req(0, "azure:en-GB-Ada:DragonHDLatestNeural", "She turned.", SpeakerKind::Narrator),
        req(1, "azure:en-US-Ava:DragonHDLatestNeural", "\"Don't.\"", SpeakerKind::Character),
    ];

    let joined = b.render_joined(&reqs).await.unwrap();
    assert!(!joined.is_empty());
    assert_eq!(joined.len() % 2, 0);
    eprintln!(
        "one multi-voice request: {} B = {} ms",
        joined.len(),
        joined.duration_ms()
    );

    // Per-segment renders must sum to roughly the same audio; the joined path
    // exists precisely because it is one request instead of two.
    let parts = b.render_batch(&reqs).await.unwrap();
    assert_eq!(parts.len(), 2);
    let summed = Pcm16k::concat(&parts).len();
    let ratio = summed as f64 / joined.len() as f64;
    assert!(
        (0.6..1.6).contains(&ratio),
        "joined and per-segment totals diverge wildly: {ratio}"
    );
}
