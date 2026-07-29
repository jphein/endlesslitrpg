//! Live Azure calls. **All `#[ignore]`** — they spend real quota.
//!
//! ```text
//! cargo test -p litrpg-tts --test azure_live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Budgeted at **exactly two network requests** for the whole file: one
//! single-voice render carrying every XML-escapable character, and one
//! multi-voice document. Each is a couple of seconds of audio, so a full
//! verification run costs a fraction of a cent.

use litrpg_core::SpeakerKind;
use litrpg_tts::azure::{AzureBackend, OUTPUT_FORMAT, build_multi_voice_ssml, build_ssml};
use litrpg_tts::{Availability, Pcm16k, RenderRequest, TtsBackend};

/// Dialogue that is invalid SSML unless every entity is escaped: a double quote,
/// an apostrophe, and a bare ampersand. All three are guaranteed to appear in
/// LitRPG dialogue.
const NASTY: &str = "\"Don't,\" she said & the gate fell.";

fn backend() -> AzureBackend {
    AzureBackend::from_default_config().expect("azure credentials not resolvable")
}

fn req(idx: u32, voice: &str, text: &str, kind: SpeakerKind) -> RenderRequest {
    RenderRequest::parse(idx, voice, text, kind).unwrap()
}

fn peak(pcm: &Pcm16k) -> u16 {
    pcm.as_bytes()
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
        .max()
        .unwrap_or(0)
}

#[test]
#[ignore = "reads ~/.config/speech-to-cli/config.json; environment-dependent"]
fn credentials_resolve_from_the_speech_to_cli_config() {
    // Zero network calls.
    let b = backend();
    assert!(matches!(b.available(), Availability::Ready));
    assert!(!b.voices().is_empty());
    // Never assert on the key itself; assert only that Debug stays clean.
    let dbg = format!("{:?}", b.config());
    assert!(dbg.contains("redacted"), "key not redacted: {dbg}");
    assert!(
        dbg.contains("eastus"),
        "DragonHD is region-limited to eastus"
    );
}

/// **Network call 1 of 2.**
#[tokio::test]
#[ignore = "spends Azure quota"]
async fn escaped_dialogue_round_trips_and_returns_raw_16k_pcm() {
    let r = req(
        0,
        "azure:en-GB-Ada:DragonHDLatestNeural",
        NASTY,
        SpeakerKind::Narrator,
    );

    // Prove locally that what we are about to send is escaped, so a 200 below is
    // evidence that Azure accepted *escaped* markup rather than that the text
    // happened to be harmless.
    let ssml = build_ssml(&r);
    assert!(ssml.contains("&quot;Don&apos;t,&quot; she said &amp; the gate fell."));
    assert!(
        !ssml.contains("said & the"),
        "bare ampersand would be invalid XML"
    );

    let pcm = backend().render(&r).await.unwrap();

    // A 400 Bad Request is the failure mode unescaped markup produces; reaching
    // here at all means the document parsed server-side.
    assert!(!pcm.is_empty(), "empty PCM back from Azure");
    assert_eq!(pcm.len() % 2, 0, "odd byte count means a truncated stream");
    assert_ne!(&pcm.as_bytes()[..4], b"RIFF", "expected headerless PCM");
    assert_eq!(OUTPUT_FORMAT, "raw-16khz-16bit-mono-pcm");

    // The byte/duration identity the watch's Range requests rest on.
    assert!(pcm.is_whole_ms());
    assert_eq!(pcm.len() as u32, pcm.duration_ms() * 32);
    assert_eq!(
        pcm.len() as f64,
        32_000.0 * (pcm.duration_ms() as f64 / 1000.0)
    );

    // ~2-4 s of speech for this sentence. A stream mistaken for 24 kHz would read
    // 1.5x long here.
    let ms = pcm.duration_ms();
    assert!(
        (1_000..8_000).contains(&ms),
        "implausible duration: {ms} ms"
    );
    assert!(
        peak(&pcm) > 1_000,
        "audio looks like silence, peak {}",
        peak(&pcm)
    );

    eprintln!(
        "[call 1] escaped dialogue: {} B = {ms} ms, peak {}, whole_ms {}",
        pcm.len(),
        peak(&pcm),
        pcm.is_whole_ms()
    );
}

/// **Network call 2 of 2.** The core design claim: N segments, one HTTP request.
#[tokio::test]
#[ignore = "spends Azure quota"]
async fn one_multi_voice_document_returns_one_joined_stream() {
    let reqs = vec![
        req(
            0,
            "azure:en-GB-Ada:DragonHDLatestNeural",
            "She turned to go.",
            SpeakerKind::Narrator,
        ),
        req(
            1,
            "azure:en-US-Ava:DragonHDLatestNeural",
            "\"Don't.\"",
            SpeakerKind::Character,
        ),
        req(
            2,
            "azure:en-GB-Ada:DragonHDLatestNeural",
            "The gate held.",
            SpeakerKind::Narrator,
        ),
    ];

    // Locally: three voices, one document. Two distinct Azure voice names, both
    // carrying the colon that a naive split would truncate.
    let ssml = build_multi_voice_ssml(&reqs);
    assert_eq!(ssml.matches("<voice name=").count(), 3);
    assert_eq!(ssml.matches("<speak").count(), 1);
    assert!(ssml.contains("name=\"en-GB-Ada:DragonHDLatestNeural\""));
    assert!(ssml.contains("name=\"en-US-Ava:DragonHDLatestNeural\""));

    // One request for all three segments.
    let joined = backend().render_joined(&reqs).await.unwrap();

    assert!(!joined.is_empty());
    assert_eq!(joined.len() % 2, 0);
    assert_ne!(&joined.as_bytes()[..4], b"RIFF");
    assert!(joined.is_whole_ms());
    assert_eq!(joined.len() as u32, joined.duration_ms() * 32);
    assert_eq!(
        joined.len() as f64,
        32_000.0 * (joined.duration_ms() as f64 / 1000.0)
    );

    // Three short sentences across two voices.
    let ms = joined.duration_ms();
    assert!(
        (1_500..15_000).contains(&ms),
        "implausible duration: {ms} ms"
    );
    assert!(peak(&joined) > 1_000, "joined stream looks like silence");

    // Voices switching inside one stream means the audio cannot be a single
    // uninterrupted utterance: there is real signal in the back half too, i.e. the
    // request did not silently render only the first <voice>.
    let half = joined.len() / 2 / 2 * 2;
    let tail_peak = joined.as_bytes()[half..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(
        tail_peak > 1_000,
        "second half is silent - only the first <voice> was rendered? peak {tail_peak}"
    );

    eprintln!(
        "[call 2] one multi-voice document, 3 segments/2 voices: {} B = {ms} ms, \
         peak {} head / {tail_peak} tail",
        joined.len(),
        peak(&joined)
    );
}

// ------------------------------------------------ curated-list verification

/// **One catalog request, validates every advertised voice.**
///
/// This is the check that should have existed before
/// `en-GB-OllieMultilingual:DragonHDLatestNeural` shipped. It costs a single
/// non-synthesis request and is authoritative rather than curated, so it scales to
/// the whole table for free — unlike probing each voice with a render.
#[tokio::test]
#[ignore = "one Azure catalog request (no synthesis)"]
async fn every_curated_voice_exists_in_the_azure_catalog() {
    let b = backend();
    let catalog = b.fetch_catalog().await.unwrap();
    assert!(
        catalog.len() > 100,
        "catalog looks truncated: {} entries",
        catalog.len()
    );
    eprintln!(
        "azure catalog: {} voices in {}, {} DragonHD",
        catalog.len(),
        b.config().region,
        catalog.iter().filter(|v| v.contains("DragonHD")).count()
    );

    let mut bad = Vec::new();
    for v in b.voices() {
        let verdict = b.preflight_one(&v.voice_ref, &catalog);
        eprintln!(
            "  {} {}",
            if verdict.is_ok() { "OK  " } else { "FAIL" },
            verdict
        );
        if !verdict.is_ok() {
            bad.push(verdict.to_string());
        }
    }
    assert!(
        bad.is_empty(),
        "advertised voices that Azure does not list — these return HTTP 400 and would \
         cost a chapter's audio:\n  {}",
        bad.join("\n  ")
    );

    // The configured `speech-to-cli` default is assignable here too, so it must also
    // be real.
    let configured = format!("azure:{}", b.config().default_voice);
    let verdict = b.preflight_one(&configured, &catalog);
    assert!(
        verdict.is_ok(),
        "configured default voice is not real: {verdict}"
    );
}

/// Per-voice synthesis, one short word each. Belt and braces over the catalog check:
/// catalog presence is necessary but not sufficient — a voice could be listed and
/// still refuse a request shape.
#[tokio::test]
#[ignore = "spends Azure quota: one short render per advertised voice"]
async fn every_curated_voice_actually_synthesizes() {
    let b = backend();
    let mut failures = Vec::new();
    for v in b.voices() {
        let r = req(0, &v.voice_ref, "Ready.", SpeakerKind::Character);
        match b.render(&r).await {
            Ok(pcm) => {
                let ok = !pcm.is_empty() && pcm.is_whole_ms() && peak(&pcm) > 500;
                eprintln!(
                    "  {} {:<46} {:>6} B  peak {}",
                    if ok { "OK  " } else { "WEAK" },
                    v.voice_ref,
                    pcm.len(),
                    peak(&pcm)
                );
                if !ok {
                    failures.push(format!("{}: empty or silent output", v.voice_ref));
                }
            }
            Err(e) => {
                eprintln!("  FAIL {:<46} {e}", v.voice_ref);
                failures.push(format!("{}: {e}", v.voice_ref));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "advertised voices that do not synthesize:\n  {}",
        failures.join("\n  ")
    );
}
