//! The native-rate → 16 kHz seam (spec §7.5).
//!
//! sherpa models emit 22 050 Hz (Piper/cori) or 24 000 Hz (Kokoro). Getting this
//! wrong makes the watch play chipmunks, so the seam is tested directly and
//! **without** the `sherpa` feature — it is pure `ffmpeg` process I/O, no models,
//! no native libs. The invocation is Reverie's verified one (spike Part 2 §2.3),
//! adapted to read f32 samples from a pipe instead of a `.wav` file.

use litrpg_tts::resample::{FfmpegPostProcessor, LoudnormStats, PostProcess, PostProcessor};

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
    assert_eq!(
        pcm.len(),
        48_000,
        "1.5 s must be 48 000 B at 16 kHz mono s16le"
    );
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
    assert_eq!(
        pcm.len(),
        16_000,
        "a 44-byte RIFF header would show up here"
    );
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
        .process(
            &tone(24_000, 2.0),
            24_000,
            PostProcess::plain().with_loudnorm(),
        )
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
fn output_is_always_whole_millisecond_aligned() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    // Sample counts chosen so the naive resample lands off a 32-byte boundary:
    // 1000 samples @ 24k -> ~667 samples out = 1334 B, and 1334 % 32 == 22.
    // Padding at this boundary is what keeps duration_ms() * 32 == len() true for
    // everything the engine ever sees, so it cannot forget to do it.
    for n in [1_000usize, 1_001, 1_337, 4_003, 51] {
        let samples: Vec<f32> = (0..n)
            .map(|i| 0.3 * (std::f32::consts::TAU * 220.0 * i as f32 / 24_000.0).sin())
            .collect();
        for pp_mode in [
            PostProcess::plain(),
            PostProcess::normalized(),
            PostProcess::system_voice(),
        ] {
            let pcm = pp.process(&samples, 24_000, pp_mode).unwrap();
            assert!(
                pcm.is_whole_ms(),
                "{n} samples with {pp_mode:?} gave {} B ({} past a ms)",
                pcm.len(),
                pcm.remainder_bytes()
            );
            assert_eq!(pcm.duration_ms() * 32, pcm.len() as u32);
        }
    }
}

#[test]
fn alignment_padding_is_under_a_millisecond_so_it_cannot_drift_timings() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    // 10 s of audio: if alignment were adding anything material, it would show.
    let samples: Vec<f32> = (0..240_000)
        .map(|i| 0.3 * (std::f32::consts::TAU * 220.0 * i as f32 / 24_000.0).sin())
        .collect();
    let pcm = pp.process(&samples, 24_000, PostProcess::plain()).unwrap();
    let expected = 10_000u32;
    assert!(
        pcm.duration_ms().abs_diff(expected) <= 1,
        "10 s became {} ms",
        pcm.duration_ms()
    );
}

#[test]
fn empty_input_yields_empty_output_without_invoking_a_subprocess() {
    let pp = FfmpegPostProcessor::default();
    let pcm = pp.process(&[], 24_000, PostProcess::plain()).unwrap();
    assert!(pcm.is_empty());
}

#[test]
fn loudnorm_is_never_fused_into_the_fx_filter_graph() {
    // ⚠️ REGRESSION GUARD. Appending `loudnorm` downstream of
    // `acompressor,highpass,lowpass` in one graph made the SYSTEM segment land at
    // -23.1 LUFS instead of -20 — a 3.3 LU spread, audible as "the system voice is
    // oddly quiet". Reverie's spike fuses FX with the *resample* (§2.5) and runs
    // loudnorm standalone (§2.6); conflating those is what broke it.
    //
    // If someone "optimises" the two invocations back into one, this fails.
    for pp in [
        PostProcess::system_voice(),
        PostProcess::normalized(),
        PostProcess::plain().with_loudnorm(),
    ] {
        let chain = pp.filter_chain(24_000);
        assert!(
            !chain.contains("loudnorm"),
            "loudnorm must be its own ffmpeg pass, not part of the stage-1 chain: {chain}"
        );
    }
}

#[test]
fn the_system_filter_chain_is_built_in_the_verified_order() {
    let chain = PostProcess::system_voice().filter_chain(24_000);
    // Reverie's verified colouring, in her order.
    let set = chain.find("asetrate").expect("asetrate missing");
    let trem = chain.find("tremolo").expect("tremolo missing");
    let comp = chain.find("acompressor").expect("acompressor missing");
    assert!(set < trem && trem < comp, "FX order changed: {chain}");
    assert!(chain.contains("asetrate=24000*0.92"));
    assert!(chain.contains("aresample=24000"));
    assert!(chain.contains("atempo=1/0.92"));
    assert!(chain.contains("highpass=f=180"));
    assert!(chain.contains("lowpass=f=5200"));
    assert!(chain.contains("acompressor"));
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

// ------------------------------------------------------- two-pass loudnorm stats

#[test]
fn loudnorm_stats_parse_from_a_real_ffmpeg_json_block() {
    // Verbatim shape of what `loudnorm=...:print_format=json` writes to stderr.
    let stderr = r#"[Parsed_loudnorm_0 @ 0x5555]
{
	"input_i" : "-25.10",
	"input_tp" : "-9.35",
	"input_lra" : "1.80",
	"input_thresh" : "-35.23",
	"output_i" : "-20.01",
	"output_tp" : "-2.00",
	"output_lra" : "1.70",
	"output_thresh" : "-30.14",
	"normalization_type" : "dynamic",
	"target_offset" : "0.12"
}
"#;
    let s = LoudnormStats::parse(stderr).expect("should parse");
    assert_eq!(s.input_i, -25.10);
    assert_eq!(s.input_tp, -9.35);
    assert_eq!(s.input_lra, 1.80);
    assert_eq!(s.input_thresh, -35.23);
    assert_eq!(s.target_offset, 0.12);
}

#[test]
fn non_finite_measurements_are_refused_so_the_caller_can_fall_back() {
    // A silent or near-silent buffer measures as -inf. Feeding that into pass two
    // produces garbage, so it must read as "unmeasurable", not as a number.
    let stderr = r#"{
	"input_i" : "-inf",
	"input_tp" : "-120.00",
	"input_lra" : "0.00",
	"input_thresh" : "-inf",
	"target_offset" : "0.00"
}"#;
    assert!(
        LoudnormStats::parse(stderr).is_none(),
        "-inf must not be accepted as a measurement"
    );
}

#[test]
fn a_missing_json_block_is_refused_rather_than_guessed() {
    assert!(LoudnormStats::parse("").is_none());
    assert!(LoudnormStats::parse("ffmpeg version 6.1.1\nno stats here").is_none());
    // Present but incomplete: missing target_offset.
    assert!(
        LoudnormStats::parse(r#"{"input_i":"-25.1","input_tp":"-9.3"}"#).is_none(),
        "a partial block must not be half-applied"
    );
}

/// Integrated loudness of a 16 kHz PCM buffer, via ffmpeg's `ebur128`.
fn measure_lufs(pcm: &[u8]) -> Option<f64> {
    let path = std::env::temp_dir().join("litrpg-resample-loudness.pcm");
    std::fs::write(&path, pcm).ok()?;
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner", "-nostats", "-f", "s16le", "-ar", "16000", "-ac", "1", "-i",
        ])
        .arg(&path)
        .args(["-af", "ebur128", "-f", "null", "-"])
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    let idx = stderr.rfind("Integrated loudness")?;
    stderr[idx..]
        .lines()
        .find(|l| l.trim_start().starts_with("I:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[test]
fn two_pass_loudnorm_hits_the_target_on_a_controlled_signal() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    // A deliberately quiet, long-enough signal. 20 s gives EBU R128's gating plenty
    // of blocks, so this measures the *normalizer*, not the measurement noise that
    // makes a 2-3 s clip's integrated loudness unreliable.
    let quiet: Vec<f32> = (0..24_000 * 20)
        .map(|i| {
            0.02 * (std::f32::consts::TAU * 180.0 * i as f32 / 24_000.0).sin()
                * (0.6 + 0.4 * (std::f32::consts::TAU * 3.0 * i as f32 / 24_000.0).sin())
        })
        .collect();

    let plain = pp.process(&quiet, 24_000, PostProcess::plain()).unwrap();
    let normed = pp
        .process(&quiet, 24_000, PostProcess::normalized())
        .unwrap();

    let Some(before) = measure_lufs(plain.as_bytes()) else {
        eprintln!("SKIP: no ebur128 summary");
        return;
    };
    let after = measure_lufs(normed.as_bytes()).expect("normalized measurement");
    eprintln!("two-pass loudnorm: {before:.1} LUFS -> {after:.1} LUFS (target -20)");

    assert!(
        before < -25.0,
        "test signal should start well below target, was {before:.1}"
    );
    assert!(
        (after - -20.0).abs() < 1.0,
        "two-pass loudnorm should land within 1 LU of -20, got {after:.1}"
    );
}

#[test]
fn a_silent_buffer_still_produces_valid_pcm_via_the_single_pass_fallback() {
    let Some(pp) = ffmpeg_or_skip() else { return };
    // Digital silence measures -inf, exercising the fallback path end to end.
    let silence = vec![0.0f32; 24_000];
    let pcm = pp
        .process(&silence, 24_000, PostProcess::normalized())
        .unwrap();
    assert!(!pcm.is_empty(), "silence must still yield a buffer");
    assert!(pcm.is_whole_ms());
    assert_eq!(pcm.duration_ms() * 32, pcm.len() as u32);
}
