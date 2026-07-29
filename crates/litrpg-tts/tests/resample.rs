//! The native-rate → 16 kHz seam (spec §7.5).
//!
//! sherpa models emit 22 050 Hz (Piper/cori) or 24 000 Hz (Kokoro). Getting this
//! wrong makes the watch play chipmunks, so the seam is tested directly and
//! **without** the `sherpa` feature — it is pure `ffmpeg` process I/O, no models,
//! no native libs. The invocation is Reverie's verified one (spike Part 2 §2.3),
//! adapted to read f32 samples from a pipe instead of a `.wav` file.

use litrpg_tts::resample::{FfmpegPostProcessor, PostProcess, PostProcessor};

/// A 220 Hz tone. Real audio, not zeros — a silent buffer would pass a resampler
/// test that a broken filter chain would also pass.
fn tone(rate: u32, seconds: f64) -> Vec<f32> {
    let n = (rate as f64 * seconds).round() as usize;
    (0..n)
        .map(|i| 0.3 * (std::f32::consts::TAU * 220.0 * i as f32 / rate as f32).sin())
        .collect()
}

fn ffmpeg_or_skip() -> Option<FfmpegPostProcessor> {
    let pp = FfmpegPostProcessor::default();
    if pp.is_available() {
        Some(pp)
    } else {
        eprintln!("SKIP: ffmpeg not on PATH");
        None
    }
}

#[test]
fn kokoro_24k_resamples_to_exactly_32000_bytes_per_second() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    let pcm = pp
        .process(&tone(24_000, 1.5), 24_000, PostProcess::plain())
        .unwrap();
    assert_eq!(pcm.len(), 48_000, "1.5 s must be 48 000 B at 16 kHz mono s16le");
    assert_eq!(pcm.duration_ms(), 1_500);
    assert!(pcm.is_whole_ms());
}

#[test]
fn piper_22050_resamples_to_exactly_32000_bytes_per_second() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    let pcm = pp
        .process(&tone(22_050, 1.5), 22_050, PostProcess::plain())
        .unwrap();
    assert_eq!(
        pcm.len(),
        48_000,
        "22 050 -> 16 000 is a non-integer ratio and must still land on the contract"
    );
    assert_eq!(pcm.duration_ms(), 1_500);
}

#[test]
fn the_output_is_headerless() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    let pcm = pp
        .process(&tone(24_000, 0.5), 24_000, PostProcess::plain())
        .unwrap();
    assert_eq!(pcm.len(), 16_000, "a 44-byte RIFF header would show up here");
    assert_ne!(&pcm.as_bytes()[..4], b"RIFF");
}

#[test]
fn the_resampled_audio_is_not_silence() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    let pcm = pp
        .process(&tone(24_000, 1.0), 24_000, PostProcess::plain())
        .unwrap();
    let peak = pcm
        .as_bytes()
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 5_000, "expected a real waveform, peak was {peak}");
}

#[test]
fn loudnorm_holds_the_byte_contract_and_changes_the_level() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    let plain = pp
        .process(&tone(24_000, 2.0), 24_000, PostProcess::plain())
        .unwrap();
    let normed = pp
        .process(&tone(24_000, 2.0), 24_000, PostProcess::plain().with_loudnorm())
        .unwrap();

    // Reverie measured loudnorm's filter delay shifting length by ~100 B on real
    // speech (spike Part 2 §2.6), so this is a tolerance, not an equality — and
    // it is why offsets must be computed *after* normalization.
    let drift = normed.len().abs_diff(plain.len());
    assert!(
        drift * 200 < plain.len(),
        "loudnorm drifted {drift} B of {} — more than 0.5%",
        plain.len()
    );
    assert_eq!(normed.len() % 2, 0);
    assert!(!normed.is_empty());
}

#[test]
fn the_system_voice_chain_preserves_duration_within_a_hair() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    // asetrate drops the pitch, atempo compensates, so duration survives.
    let plain = pp
        .process(&tone(24_000, 2.0), 24_000, PostProcess::plain())
        .unwrap();
    let robot = pp
        .process(&tone(24_000, 2.0), 24_000, PostProcess::system_voice())
        .unwrap();

    let drift = robot.len().abs_diff(plain.len());
    assert!(
        drift * 100 < plain.len(),
        "SYSTEM chain drifted {drift} B of {} — more than 1%",
        plain.len()
    );
    assert_eq!(robot.len() % 2, 0);
}

#[test]
fn the_system_voice_chain_actually_changes_the_signal() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    let plain = pp
        .process(&tone(24_000, 1.0), 24_000, PostProcess::plain())
        .unwrap();
    let robot = pp
        .process(&tone(24_000, 1.0), 24_000, PostProcess::system_voice())
        .unwrap();
    let n = plain.len().min(robot.len());
    assert_ne!(
        &plain.as_bytes()[..n],
        &robot.as_bytes()[..n],
        "the filter chain was a no-op — SYSTEM would sound like the narrator"
    );
}

#[test]
fn empty_input_yields_empty_output_without_invoking_a_subprocess() {
    let pp = FfmpegPostProcessor::default();
    let pcm = pp.process(&[], 24_000, PostProcess::plain()).unwrap();
    assert!(pcm.is_empty());
}

#[test]
fn the_filter_chain_is_built_in_the_verified_order() {
    // Order matters: FX colouring, then loudness normalization, then resample.
    // Normalizing before the compressor would be undone by it.
    let chain = PostProcess::system_voice().with_loudnorm().filter_chain(24_000);
    let fx = chain.find("tremolo").expect("FX stage missing");
    let ln = chain.find("loudnorm").expect("loudnorm stage missing");
    assert!(fx < ln, "loudnorm must come after the SYSTEM colouring: {chain}");
    assert!(chain.contains("asetrate=24000*0.92"));
    assert!(chain.contains("aresample=24000"));
    assert!(chain.contains("atempo=1/0.92"));
    assert!(chain.contains("highpass=f=180"));
    assert!(chain.contains("lowpass=f=5200"));
    assert!(chain.contains("acompressor"));
    assert!(chain.contains("loudnorm=I=-20:TP=-2:LRA=7"));
}

#[test]
fn the_system_chain_is_parameterized_by_the_models_native_rate() {
    // Reverie verified the chain against 24 kHz Kokoro. Hardcoding 24000 would
    // pitch-shift a 22 050 Hz Piper render by 8.8%.
    let chain = PostProcess::system_voice().filter_chain(22_050);
    assert!(chain.contains("asetrate=22050*0.92"), "{chain}");
    assert!(chain.contains("aresample=22050"), "{chain}");
}

#[test]
fn a_plain_pass_has_no_filters_at_all() {
    assert!(PostProcess::plain().filter_chain(24_000).is_empty());
}
