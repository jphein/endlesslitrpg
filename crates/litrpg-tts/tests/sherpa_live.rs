//! Live sherpa-onnx renders. Requires the `sherpa` feature **and** models on
//! disk, so every test is `#[ignore]`.
//!
//! ```text
//! LITRPG_SHERPA_MODELS=~/tts-spike/models \
//!   cargo test -p litrpg-tts --features sherpa --test sherpa_live -- --ignored --nocapture
//! ```
//!
//! This is spec §11's "Mixed-model stitch: opt-in — cori + Kokoro + SYSTEM in one
//! chapter, assert continuity and exact byte count".

#![cfg(feature = "sherpa")]

use litrpg_core::{BYTES_PER_MS, Manifest, Segment, SpeakerKind};
use litrpg_tts::sherpa::{SherpaBackend, SherpaConfig};
use litrpg_tts::{Availability, Pcm16k, RenderRequest, TtsBackend};

fn backend() -> SherpaBackend {
    let b = SherpaBackend::new(SherpaConfig::default());
    if let Availability::Missing { reason } = b.available() {
        panic!("sherpa unavailable: {reason}");
    }
    b
}

fn peak(pcm: &Pcm16k) -> u16 {
    pcm.as_bytes()
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
        .max()
        .unwrap_or(0)
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn the_narrator_renders_at_the_16k_contract() {
    let b = backend();
    let pcm = b
        .render(
            &RenderRequest::parse(
                0,
                &b.config().narrator_voice_ref(),
                "The vale smelled of iron and wet ash, and nothing moved.",
                SpeakerKind::Narrator,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(!pcm.is_empty());
    assert_eq!(pcm.len() % 2, 0);
    assert_ne!(&pcm.as_bytes()[..4], b"RIFF", "must be headerless");
    // A 22 050 Hz stream mistaken for 16 kHz would be ~1.38x too long.
    let ms = pcm.duration_ms();
    assert!((1_500..12_000).contains(&ms), "implausible duration: {ms} ms");
    assert!(peak(&pcm) > 1_000, "output looks like silence");
    eprintln!("cori: {} B = {ms} ms, peak {}", pcm.len(), peak(&pcm));
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn distinct_sids_produce_distinct_audio_with_no_model_reload() {
    let b = backend();
    let text = "We don't have long.";
    let mut out = Vec::new();
    for sid in [18, 26, 27] {
        out.push(
            b.render(
                &RenderRequest::parse(
                    0,
                    &format!("sherpa:kokoro-multi-lang-v1_0:{sid}"),
                    text,
                    SpeakerKind::Character,
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    assert_ne!(out[0].as_bytes(), out[1].as_bytes(), "sid was ignored");
    assert_ne!(out[1].as_bytes(), out[2].as_bytes(), "sid was ignored");
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn the_system_stage_colours_the_same_speaker_differently() {
    let b = backend();
    let voice = b.config().system_voice_ref();
    let text = "You have gained a level.";
    let plain = b
        .render(&RenderRequest::parse(0, &voice, text, SpeakerKind::Character).unwrap())
        .await
        .unwrap();
    let robot = b
        .render(&RenderRequest::parse(1, &voice, text, SpeakerKind::System).unwrap())
        .await
        .unwrap();

    assert!(!robot.is_empty());
    let n = plain.len().min(robot.len());
    assert_ne!(
        &plain.as_bytes()[..n],
        &robot.as_bytes()[..n],
        "the SYSTEM ffmpeg chain was a no-op"
    );
    // asetrate + atempo preserve duration; a >5% swing means the chain broke.
    let drift = robot.len().abs_diff(plain.len());
    assert!(drift * 20 < plain.len(), "SYSTEM drifted {drift} B");
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn a_mixed_model_chapter_stitches_to_exactly_32000_bytes_per_second() {
    let b = backend();
    // cori (22 050 Hz) + Kokoro (24 000 Hz) + Kokoro-with-FX, interleaved — the
    // three sources Reverie stitched in spike Part 2 §2.4.
    let rows: &[(&str, &str, SpeakerKind)] = &[
        ("sherpa:piper-en_GB-cori:0", "The vale smelled of iron and wet ash.", SpeakerKind::Narrator),
        ("sherpa:kokoro-multi-lang-v1_0:26", "\"We don't have long,\" Kael said.", SpeakerKind::Character),
        ("sherpa:kokoro-multi-lang-v1_0:18", "You have gained a level. Strength increased.", SpeakerKind::System),
        ("sherpa:piper-en_GB-cori:0", "Nothing moved for a long moment.", SpeakerKind::Narrator),
    ];
    let reqs: Vec<RenderRequest> = rows
        .iter()
        .enumerate()
        .map(|(i, (v, t, k))| RenderRequest::parse(i as u32, v, *t, *k).unwrap())
        .collect();

    // One render_batch call: the pool shards these across workers.
    let parts: Vec<Pcm16k> = b
        .render_batch(&reqs)
        .await
        .unwrap()
        .into_iter()
        .map(Pcm16k::padded_to_whole_ms)
        .collect();
    assert_eq!(parts.len(), 4);
    for (i, p) in parts.iter().enumerate() {
        assert!(!p.is_empty(), "segment {i} rendered empty");
        assert!(peak(p) > 500, "segment {i} looks like silence");
    }

    let mut cursor = 0u32;
    let segments: Vec<Segment> = reqs
        .iter()
        .zip(&parts)
        .map(|(r, pcm)| {
            let start_ms = cursor;
            cursor += pcm.duration_ms();
            Segment {
                idx: r.idx,
                speaker: format!("s{}", r.idx),
                kind: r.kind,
                voice_ref: r.voice.to_string(),
                text: r.text.clone(),
                start_ms,
                end_ms: cursor,
            }
        })
        .collect();

    let manifest = Manifest::new(1, segments);
    let joined = Pcm16k::concat(&parts);

    assert!(manifest.is_contiguous());
    assert!(joined.is_whole_ms());
    assert_eq!(joined.len() as u32, joined.duration_ms() * BYTES_PER_MS);
    assert_eq!(manifest.total_bytes(), joined.len() as u64);
    assert_eq!(
        joined.len() as f64,
        32_000.0 * (joined.duration_ms() as f64 / 1000.0)
    );

    for (seg, pcm) in manifest.segments.iter().zip(&parts) {
        let slice = &joined.as_bytes()[seg.start_byte() as usize..seg.end_byte() as usize];
        assert_eq!(slice, pcm.as_bytes(), "segment {} offset drifted", seg.idx);
    }

    eprintln!(
        "mixed chapter: {} segments, {} B = {:.3} s",
        manifest.segments.len(),
        joined.len(),
        joined.duration_secs_f64()
    );
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn a_bad_cast_assignment_costs_no_synthesis() {
    let b = backend();
    // sid past the speaker count, and an unconfigured model.
    for bad in [
        "sherpa:piper-en_GB-cori:7",
        "sherpa:no-such-model:0",
        "sherpa:kokoro-multi-lang-v1_0:999",
    ] {
        let r = RenderRequest::parse(0, bad, "x", SpeakerKind::Character).unwrap();
        assert!(b.render(&r).await.is_err(), "{bad} should have failed");
    }
}
