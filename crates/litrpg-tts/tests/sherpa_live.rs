//! Live sherpa-onnx renders through `sherpa-rs` 0.6.8. Requires the `sherpa`
//! feature **and** models on disk, so every test is `#[ignore]`.
//!
//! ```text
//! cargo test -p litrpg-tts --features sherpa --test sherpa_live \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Models default to `~/.local/share/litrpg/models`; override with
//! `LITRPG_SHERPA_MODELS`. Needs `vits-piper-en_GB-cori-high` and
//! `kokoro-multi-lang-v1_0` from the sherpa-onnx `tts-models` bucket.
//!
//! **Why this file exists:** Reverie verified sherpa-onnx **1.13.4 via the Python
//! wheel**. `sherpa-rs` 0.6.8 bundles an older core and had never been exercised
//! here. "Should be fine" is what produced the `enable_thinking` trap in
//! `litrpg-ember`, where a green HTTP 200 hid empty output — so every claim below
//! is checked against generated audio, not against a model card.

#![cfg(feature = "sherpa")]

use litrpg_core::{BYTES_PER_MS, Manifest, Segment, SpeakerKind};
use litrpg_tts::sherpa::{SherpaBackend, SherpaConfig};
use litrpg_tts::{
    Availability, DEFAULT_GAP_MS, Pcm16k, PostProcessor, RenderRequest, TtsBackend, assemble,
};

const CORI: &str = "piper-en_GB-cori-high:0";
const KOKORO: &str = "kokoro-multi-lang-v1_0";

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

/// Integrated loudness (LUFS) of a raw 16 kHz PCM buffer, via ffmpeg's ebur128.
///
/// `-v error` would silently suppress the summary (it logs at INFO), reporting
/// nothing rather than failing — so the verbosity here is deliberate.
fn measure_lufs(pcm: &Pcm16k, tag: &str) -> f64 {
    let path = std::env::temp_dir().join(format!("litrpg-loudness-{tag}.pcm"));
    std::fs::write(&path, pcm.as_bytes()).unwrap();
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostats",
            "-f",
            "s16le",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-i",
        ])
        .arg(&path)
        .args(["-af", "ebur128", "-f", "null", "-"])
        .output()
        .expect("ffmpeg");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_file(&path);

    // The summary block's "I:  -20.0 LUFS" is the integrated figure.
    let idx = stderr
        .rfind("Integrated loudness")
        .unwrap_or_else(|| panic!("no ebur128 summary in:\n{stderr}"));
    let tail = &stderr[idx..];
    let i_line = tail
        .lines()
        .find(|l| l.trim_start().starts_with("I:"))
        .unwrap_or_else(|| panic!("no I: line in:\n{tail}"));
    i_line
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("could not parse: {i_line}"))
}

// ---------------------------------------------------------- native rates & load

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn cori_loads_and_the_engine_itself_reports_22050_hz() {
    let b = backend();
    let n = b
        .probe(
            CORI,
            "The vale smelled of iron and wet ash, and nothing moved.",
        )
        .await
        .unwrap();

    // From the Rust API at runtime, not from the .onnx.json model card.
    assert_eq!(n.sample_rate, 22_050, "cori must report 22.05 kHz");
    assert!(!n.samples.is_empty());
    assert!(n.peak_i16() > 1_000, "silence, peak {}", n.peak_i16());
    assert!(
        (1.5..12.0).contains(&n.audio_secs()),
        "implausible: {:.2} s",
        n.audio_secs()
    );
    eprintln!(
        "cori-high: {} Hz, {:.3} s audio in {:.3} s wall -> RTF {:.2}x, peak {}",
        n.sample_rate,
        n.audio_secs(),
        n.synth_wall.as_secs_f64(),
        n.rtf(),
        n.peak_i16()
    );
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn kokoro_loads_and_the_engine_itself_reports_24000_hz() {
    let b = backend();
    let n = b
        .probe(
            &format!("{KOKORO}:26"),
            "We don't have long, and the ward is already failing.",
        )
        .await
        .unwrap();
    assert_eq!(n.sample_rate, 24_000, "Kokoro must report 24 kHz");
    assert!(n.peak_i16() > 1_000);
    eprintln!(
        "kokoro sid 26: {} Hz, {:.3} s audio in {:.3} s wall -> RTF {:.2}x, peak {}",
        n.sample_rate,
        n.audio_secs(),
        n.synth_wall.as_secs_f64(),
        n.rtf(),
        n.peak_i16()
    );
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn cori_is_single_speaker_and_kokoro_addresses_its_full_range() {
    // `sherpa-rs` 0.6.8 has no `num_speakers()` accessor, so the speaker range is
    // probed behaviourally — which is the thing that actually matters: can the cast
    // table address the sid it wants, and does an out-of-range sid fail loudly?
    let b = backend();

    // cori: sid 0 is the only one configured, and the config gate rejects more.
    assert!(b.probe(CORI, "One.").await.is_ok());
    let r = RenderRequest::parse(
        0,
        "sherpa:piper-en_GB-cori-high:1",
        "One.",
        SpeakerKind::Narrator,
    )
    .unwrap();
    assert!(
        b.render(&r).await.is_err(),
        "sid 1 on a single-speaker model must be refused"
    );

    // Kokoro: the extremes of the declared 53-speaker range both synthesize.
    for sid in [0, 27, 52] {
        let n = b
            .probe(&format!("{KOKORO}:{sid}"), "The gate held.")
            .await
            .unwrap_or_else(|e| panic!("kokoro sid {sid} failed: {e}"));
        assert!(!n.samples.is_empty(), "kokoro sid {sid} produced nothing");
        assert!(n.peak_i16() > 500, "kokoro sid {sid} produced silence");
    }
    // And 53 is past the end.
    let r = RenderRequest::parse(
        0,
        &format!("sherpa:{KOKORO}:53"),
        "x",
        SpeakerKind::Character,
    )
    .unwrap();
    assert!(b.render(&r).await.is_err(), "sid 53 must be refused");
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn distinct_kokoro_sids_produce_genuinely_different_audio() {
    // A silently-ignored `sid` is a real failure mode: every character would get
    // the same voice and the cast would look like a bug in the story engine.
    let b = backend();
    let text = "We don't have long.";
    let mut renders = Vec::new();
    for sid in [3, 18, 26] {
        renders.push(
            b.probe(&format!("{KOKORO}:{sid}"), text)
                .await
                .unwrap_or_else(|e| panic!("sid {sid}: {e}")),
        );
    }

    for (a, c) in [(0, 1), (1, 2), (0, 2)] {
        assert_ne!(
            renders[a].samples, renders[c].samples,
            "sid {} and sid {} produced identical samples - sid was ignored",
            renders[a].sid, renders[c].sid
        );
    }
    // Distinct durations too, as Reverie observed in Python.
    let durations: Vec<f64> = renders.iter().map(|r| r.audio_secs()).collect();
    assert!(
        durations.windows(2).any(|w| (w[0] - w[1]).abs() > 0.01),
        "all durations identical: {durations:?}"
    );
    for r in &renders {
        eprintln!(
            "kokoro sid {:>2}: {:.3} s, peak {}",
            r.sid,
            r.audio_secs(),
            r.peak_i16()
        );
    }
}

// -------------------------------------------------------- the real render path

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
    eprintln!(
        "SYSTEM colouring: {} B plain vs {} B coloured ({} B drift)",
        plain.len(),
        robot.len(),
        drift
    );
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn a_real_mixed_model_chapter_stitches_to_exactly_32000_bytes_per_second() {
    let b = backend();
    // The three sources Reverie stitched in spike Part 2 §2.4, now through the
    // actual Rust pipeline: synthesis -> resample -> [SYSTEM FX] -> loudnorm ->
    // whole-ms align -> assemble.
    let rows: &[(&str, &str, SpeakerKind)] = &[
        (
            CORI,
            "The vale smelled of iron and wet ash, and nothing moved.",
            SpeakerKind::Narrator,
        ),
        (
            "kokoro-multi-lang-v1_0:26",
            "\"We don't have long,\" Kael said, & the ward flickered.",
            SpeakerKind::Character,
        ),
        (
            "kokoro-multi-lang-v1_0:18",
            "You have gained a level. Strength increased by two.",
            SpeakerKind::System,
        ),
    ];
    let reqs: Vec<RenderRequest> = rows
        .iter()
        .enumerate()
        .map(|(i, (v, t, k))| {
            RenderRequest::parse(i as u32, &format!("sherpa:{v}"), *t, *k).unwrap()
        })
        .collect();

    // One render_batch call: the pool shards these across workers.
    let started = std::time::Instant::now();
    let parts = b.render_batch(&reqs).await.unwrap();
    let batch_wall = started.elapsed();
    assert_eq!(parts.len(), 3);

    for (i, p) in parts.iter().enumerate() {
        assert!(!p.is_empty(), "segment {i} rendered empty");
        assert!(
            peak(p) > 500,
            "segment {i} looks like silence, peak {}",
            peak(p)
        );
        // Every buffer crossing the plugin boundary is already aligned.
        assert!(p.is_whole_ms(), "segment {i} left the plugin misaligned");
    }

    let chapter = assemble(&parts, DEFAULT_GAP_MS);
    let segments: Vec<Segment> = reqs
        .iter()
        .zip(&chapter.spans)
        .map(|(r, s)| Segment {
            idx: r.idx,
            speaker: format!("s{}", r.idx),
            kind: r.kind,
            voice_ref: r.voice.to_string(),
            text: r.text.clone(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
        })
        .collect();
    let manifest = Manifest::new(1, segments);

    // The contract.
    assert!(chapter.pcm.is_whole_ms());
    assert_eq!(
        chapter.pcm.len() as u32,
        chapter.pcm.duration_ms() * BYTES_PER_MS
    );
    assert_eq!(
        chapter.pcm.len() as f64,
        32_000.0 * (chapter.pcm.duration_ms() as f64 / 1000.0)
    );
    assert!(manifest.is_contiguous());
    assert_eq!(manifest.total_bytes(), chapter.pcm.len() as u64);

    // Slicing at manifest offsets byte-matches the sources.
    for (seg, pcm) in manifest.segments.iter().zip(&parts) {
        let slice = &chapter.pcm.as_bytes()[seg.start_byte() as usize..seg.end_byte() as usize];
        assert_eq!(slice, pcm.as_bytes(), "segment {} offset drifted", seg.idx);
    }
    for ms in [
        0,
        1,
        chapter.pcm.duration_ms() / 2,
        chapter.pcm.duration_ms() - 1,
    ] {
        assert!(
            manifest.segment_at_ms(ms).is_some(),
            "no segment at {ms} ms"
        );
    }

    // Loudness: Reverie measured a 0.7 LU spread and -20.0 LUFS overall after
    // loudnorm. Confirm this code lands in the same place.
    let per_segment: Vec<f64> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| measure_lufs(p, &format!("seg{i}")))
        .collect();
    let whole = measure_lufs(&chapter.pcm, "chapter");
    let spread = per_segment.iter().cloned().fold(f64::MIN, f64::max)
        - per_segment.iter().cloned().fold(f64::MAX, f64::min);

    eprintln!(
        "\nmixed chapter: {} segments, {} B = {:.3} s, batch wall {:.2} s",
        manifest.segments.len(),
        chapter.pcm.len(),
        chapter.pcm.duration_secs_f64(),
        batch_wall.as_secs_f64()
    );
    for (i, l) in per_segment.iter().enumerate() {
        eprintln!("  segment {i}: {l:.1} LUFS");
    }
    eprintln!("  spread {spread:.1} LU, whole chapter {whole:.1} LUFS");

    // ACX audiobook window is -18..-23 LUFS; loudnorm targets I=-20. This is the
    // figure a listener actually experiences, and it is stable: -20.6 / -20.0 / -20.5
    // across three runs.
    assert!(
        (-23.0..=-17.0).contains(&whole),
        "chapter loudness {whole:.1} LUFS is outside the ACX window"
    );

    // This chapter's three segments carry *different text of different lengths*, so a
    // spread across them mixes the pipeline's level-matching (what we care about) with
    // R128's reduced precision on short clips (what we don't). The level-matching
    // assertion therefore lives in `loudness_lands_on_target_for_every_engine_and_the
    // _system_path`, where all three engines get identical text and the only variable
    // is the chain — measured there at 0.1 LU.
    //
    // What this test asserts about loudness is the figure a listener actually
    // experiences: whole-chapter integrated loudness, which is stable across runs.
    eprintln!(
        "  (per-segment figures are diagnostic here - different text and lengths; \
         the level-matching assertion is in the per-engine test)"
    );
    let _ = spread;
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn a_bad_cast_assignment_costs_no_synthesis() {
    let b = backend();
    for bad in [
        "sherpa:piper-en_GB-cori-high:7",
        "sherpa:no-such-model:0",
        "sherpa:kokoro-multi-lang-v1_0:999",
        "sherpa:kokoro-multi-lang-v1_0:bm_george",
    ] {
        let r = RenderRequest::parse(0, bad, "x", SpeakerKind::Character).unwrap();
        assert!(b.render(&r).await.is_err(), "{bad} should have failed");
    }
}

// ------------------------------------------------------- loudness per engine

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn loudness_lands_on_target_for_every_engine_and_the_system_path() {
    // The lead asked whether the SYSTEM path is uniquely affected or whether all
    // three engines were hurt by fusing loudnorm into the FX graph. Answered by
    // measuring each in isolation.
    //
    // Synthesis is stochastic (`noise_scale`), so each engine is probed **once** and
    // the same samples are reused for every variant below — that removes synthesis
    // variance and isolates the chain composition, which is the thing under test.
    let b = backend();
    let pp = litrpg_tts::FfmpegPostProcessor::default();

    let cases: Vec<(&str, &str, litrpg_tts::PostProcess)> = vec![
        (
            "cori-high narrator",
            CORI,
            litrpg_tts::PostProcess::normalized(),
        ),
        (
            "kokoro bm_george",
            "kokoro-multi-lang-v1_0:26",
            litrpg_tts::PostProcess::normalized(),
        ),
        (
            "kokoro am_puck +SYSTEM FX",
            "kokoro-multi-lang-v1_0:18",
            litrpg_tts::PostProcess::system_voice(),
        ),
    ];

    let text = "You have gained a level. Strength increased by two, and the ward is failing.";
    let mut normalized = Vec::new();
    eprintln!();
    for (label, voice, mode) in &cases {
        let native = b.probe(voice, text).await.unwrap();

        let plain = pp
            .process(&native.samples, native.sample_rate, mode.without_loudnorm())
            .unwrap();
        let normed = pp
            .process(&native.samples, native.sample_rate, *mode)
            .unwrap();

        let before = measure_lufs(&plain, "before");
        let after = measure_lufs(&normed, "after");
        eprintln!(
            "{label:<26} {before:>7.1} -> {after:>7.1} LUFS  ({:.2} s)",
            normed.duration_secs_f64()
        );
        normalized.push((*label, after));
    }

    let after: Vec<f64> = normalized.iter().map(|(_, l)| *l).collect();
    let spread = after.iter().cloned().fold(f64::MIN, f64::max)
        - after.iter().cloned().fold(f64::MAX, f64::min);
    eprintln!("spread across engines: {spread:.1} LU");

    for (label, l) in &normalized {
        assert!(
            (-23.0..=-17.0).contains(l),
            "{label} normalized to {l:.1} LUFS, outside the ACX window"
        );
        assert!(
            (l - -20.0).abs() < 2.0,
            "{label} is {l:.1} LUFS, more than 2 LU off the -20 target"
        );
    }
    // ⚠️ THE level-matching assertion, and it lives here rather than in the mixed
    // chapter test because *this* is the controlled comparison: identical text, so
    // the only variable is the engine and its post-processing chain. It answers the
    // question directly — is the SYSTEM path uniquely affected, or was fusing hurting
    // all three? Measured: cori -19.8, Kokoro -19.8, Kokoro+SYSTEM FX -19.9. All
    // three were hurt by fusing; none is special once loudnorm runs as its own pass.
    //
    // Regression guard: re-fusing loudnorm into the FX graph pushes the SYSTEM
    // segment to ~-23 and this fails.
    assert!(
        spread < 2.0,
        "cross-engine loudness spread {spread:.1} LU - segments will step in level at \
         joins. Has loudnorm been fused back into the FX chain?"
    );

    // Characterize the residual: how much does *segment length* alone move the
    // result? Same voice, same chain, short vs long. Chapters contain both.
    eprintln!("\nlength sensitivity (same voice + chain, SYSTEM path):");
    let mut by_len = Vec::new();
    for (tag, t) in [
        ("short", "You have gained a level."),
        (
            "medium",
            "You have gained a level. Strength increased by two.",
        ),
        (
            "long",
            "You have gained a level. Strength increased by two, and the ward is failing. \
             Three hostiles remain within the perimeter, and the gate will not hold.",
        ),
    ] {
        let n = b.probe("kokoro-multi-lang-v1_0:18", t).await.unwrap();
        let out = pp
            .process(
                &n.samples,
                n.sample_rate,
                litrpg_tts::PostProcess::system_voice(),
            )
            .unwrap();
        let l = measure_lufs(&out, tag);
        eprintln!(
            "  {tag:<7} {:.2} s -> {l:>7.1} LUFS",
            out.duration_secs_f64()
        );
        by_len.push((tag, out.duration_secs_f64(), l));
    }
    // Every length must still land inside the ACX window; the target-accuracy bound
    // above is asserted on comparable-length segments, since a very short segment has
    // few R128 gating blocks and is measured less precisely by construction.
    for (tag, secs, l) in &by_len {
        assert!(
            (-23.0..=-17.0).contains(l),
            "{tag} segment ({secs:.2} s) normalized to {l:.1} LUFS, outside the ACX window"
        );
    }
}

// ------------------------------------------------- blast radius (spec §10)

/// Build a fake model root whose Kokoro directory symlinks the real assets, so a
/// broken-model scenario costs no disk. `omit` drops assets; `swap_dict` points
/// `dict` somewhere wrong instead.
fn fake_kokoro_root(tag: &str, omit: &[&str], swap_dict: Option<&str>) -> std::path::PathBuf {
    let real = SherpaConfig::default().model_root.join(KOKORO);
    let root = std::env::temp_dir().join(format!("litrpg-blast-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join(KOKORO);
    std::fs::create_dir_all(&dir).unwrap();

    for entry in std::fs::read_dir(&real).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if omit.contains(&name_str.as_str()) {
            continue;
        }
        let target = match swap_dict {
            Some(other) if name_str == "dict" => real.join(other),
            _ => entry.path(),
        };
        std::os::unix::fs::symlink(&target, dir.join(&name)).unwrap();
    }
    root
}

fn kokoro_only_config(model_root: std::path::PathBuf) -> SherpaConfig {
    let json = format!(
        r#"{{ "model_root": {}, "workers": 1,
               "models": [{{"id": "{KOKORO}", "family": "kokoro", "dir": "{KOKORO}",
                            "native_rate": 24000, "speakers": 53,
                            "model_file": "model.onnx"}}],
               "voices": [{{"voice": "{KOKORO}:26", "label": "t", "lang": "en-GB",
                            "gender": "male"}}] }}"#,
        serde_json::to_string(&model_root.to_string_lossy()).unwrap()
    );
    SherpaConfig::from_json_str(&json).unwrap()
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn a_missing_dict_dir_is_a_catchable_error_not_a_process_abort() {
    // The containment that matters: sherpa-onnx aborts on this, so the check has to
    // happen in Rust first. If this test returns at all, containment works — an
    // abort would take the whole test binary with it.
    let cfg = kokoro_only_config(fake_kokoro_root("no-dict", &["dict"], None));
    assert!(
        !cfg.availability().is_ready(),
        "a model missing dict/ must not report Ready"
    );
    let reason = cfg.availability().reason().unwrap().to_string();
    assert!(
        reason.contains("dict"),
        "reason must name the asset: {reason}"
    );

    let b = SherpaBackend::new(cfg);
    let err = b
        .probe(&format!("{KOKORO}:26"), "Does this survive?")
        .await
        .expect_err("must be an Err, not an abort");
    assert!(
        matches!(err, litrpg_tts::TtsError::ModelMissing(_)),
        "expected ModelMissing, got {err:?}"
    );
    eprintln!("missing dict/ -> survivable: {err}");
}

#[tokio::test]
#[ignore = "needs sherpa models on disk"]
async fn a_missing_lexicon_is_a_catchable_error_not_a_process_abort() {
    let cfg = kokoro_only_config(fake_kokoro_root(
        "no-lex",
        &["lexicon-us-en.txt", "lexicon-gb-en.txt", "lexicon-zh.txt"],
        None,
    ));
    let b = SherpaBackend::new(cfg);
    let err = b
        .probe(&format!("{KOKORO}:26"), "Does this survive?")
        .await
        .expect_err("must be an Err, not an abort");
    assert!(
        matches!(err, litrpg_tts::TtsError::ModelMissing(_)),
        "{err:?}"
    );
    eprintln!("missing lexicons -> survivable: {err}");
}

#[test]
#[ignore = "needs sherpa models on disk; spawns child processes that may abort"]
fn residual_fatal_paths_are_measured_in_a_child_process() {
    // Cases Rust cannot pre-validate: assets that *exist* but are wrong, and an sid
    // past the model's real speaker count. Run in children so an exit() cannot take
    // this suite down — which is exactly the property under test.
    if std::env::var("LITRPG_BLAST_CASE").is_ok() {
        return blast_child();
    }

    for case in ["wrong-dict", "sid-out-of-range"] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "--test-threads=1",
                "--nocapture",
                "residual_fatal_paths_are_measured_in_a_child_process",
            ])
            .env("LITRPG_BLAST_CASE", case)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let survived = stdout.contains("CHILD-SURVIVED");
        eprintln!(
            "{case:<18} exit {:>4?} | {}",
            out.status.code(),
            if survived {
                "SURVIVABLE (returned an Err)"
            } else {
                "FATAL (process aborted - engine must pre-flight)"
            }
        );
        // Either outcome is a legitimate finding; what is not acceptable is the
        // parent dying, so reaching here at all is the assertion. Record which.
        assert!(
            out.status.code().is_some() || !survived,
            "{case}: child was killed by a signal with no exit code"
        );
    }
}

fn blast_child() {
    let case = std::env::var("LITRPG_BLAST_CASE").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = match case.as_str() {
        // dict/ exists but points at unrelated data.
        "wrong-dict" => {
            let cfg =
                kokoro_only_config(fake_kokoro_root("wrong-dict", &[], Some("espeak-ng-data")));
            let b = SherpaBackend::new(cfg);
            rt.block_on(b.probe(&format!("{KOKORO}:26"), "Wrong dict."))
                .map(|n| n.samples.len())
        }
        // sid past the real 53, with the config gate widened so sherpa sees it.
        "sid-out-of-range" => {
            let mut cfg = kokoro_only_config(SherpaConfig::default().model_root.clone());
            // Widen the gate: claim more speakers than the model has.
            let json = serde_json::to_string(&cfg)
                .unwrap()
                .replace("\"speakers\":53", "\"speakers\":999");
            cfg = SherpaConfig::from_json_str(&json).unwrap();
            let b = SherpaBackend::new(cfg);
            rt.block_on(b.probe(&format!("{KOKORO}:200"), "Out of range."))
                .map(|n| n.samples.len())
        }
        other => panic!("unknown case {other}"),
    };
    // Printing this is the survival signal; an exit() never gets here.
    match result {
        Ok(n) => println!("CHILD-SURVIVED ok, {n} samples"),
        Err(e) => println!("CHILD-SURVIVED err: {e}"),
    }
}

// ------------------------------------------------------------------ throughput

/// Load average, so a throughput figure is interpretable rather than just a number.
fn load_avg() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(f64::NAN)
}

#[tokio::test]
#[ignore = "needs sherpa models on disk; slow"]
async fn measured_rtf_from_rust_is_reported_for_comparison_with_python() {
    // Reverie's Python figures at 8 threads on an idle Ryzen 9 3900X (12C/24T):
    // cori-high 7.55x, Kokoro 5.28x. We run 4 threads per worker (the measured pool
    // optimum), so a lower single-stream RTF is expected.
    //
    // ⚠️ **This test measures and reports; it deliberately does not gate on wall
    // clock.** An earlier version asserted `rtf() > 1.0` and proved flaky: the same
    // Kokoro render measured 2.54x on a quiet run and 0.74x when other agents were
    // building concurrently (load average 13-15 on 8 cores). A throughput threshold
    // on a contended shared dev box asserts the state of the machine, not the state
    // of the code. Performance gating belongs on the deployment host under
    // controlled load. What is asserted here is correctness: real audio, right
    // rates, plausible durations.
    let b = backend();
    eprintln!(
        "\n[context] {} workers x {} threads, provider {}, load average {:.2}",
        b.workers(),
        b.config().threads_per_worker,
        b.config().provider,
        load_avg()
    );
    let para = "The vale smelled of iron and wet ash. Nothing moved for a long moment, \
                and then the ward broke with a sound like a bell cracking. She counted \
                three heartbeats before the first of them came through the gap, and by \
                then it was already too late to run for the treeline.";

    for (label, voice) in [
        ("cori-high", CORI),
        ("kokoro sid 26", "kokoro-multi-lang-v1_0:26"),
    ] {
        // One warm-up so model load time is not charged to the measurement.
        let _ = b.probe(voice, "Warm up.").await.unwrap();
        let n = b.probe(voice, para).await.unwrap();
        eprintln!(
            "{label:<14} {} Hz | {:.3} s audio | {:.3} s wall | RTF {:.2}x | 13-min chapter ~= {:.0} s",
            n.sample_rate,
            n.audio_secs(),
            n.synth_wall.as_secs_f64(),
            n.rtf(),
            780.0 / n.rtf()
        );
        // Correctness, not throughput: real audio at the right rate.
        assert!(!n.samples.is_empty(), "{label} produced no samples");
        assert!(n.peak_i16() > 500, "{label} produced silence");
        assert!(
            (10.0..30.0).contains(&n.audio_secs()),
            "{label}: this paragraph should be 10-30 s, got {:.2} s",
            n.audio_secs()
        );
    }

    // Pool throughput: the same paragraph as 8 segments through render_batch.
    let reqs: Vec<RenderRequest> = (0..8)
        .map(|i| {
            let voice = if i % 2 == 0 {
                format!("sherpa:{CORI}")
            } else {
                format!("sherpa:{KOKORO}:26")
            };
            RenderRequest::parse(i, &voice, para, SpeakerKind::Narrator).unwrap()
        })
        .collect();
    let started = std::time::Instant::now();
    let parts = b.render_batch(&reqs).await.unwrap();
    let wall = started.elapsed();
    let audio_ms: u32 = parts.iter().map(|p| p.duration_ms()).sum();
    let aggregate = (audio_ms as f64 / 1000.0) / wall.as_secs_f64();
    eprintln!(
        "pool ({} workers x {} threads = {} threads on {} cores): {} segments, \
         {:.1} s audio in {:.2} s wall -> aggregate RTF {aggregate:.2}x (incl. ffmpeg), \
         load average {:.2}",
        b.workers(),
        b.config().threads_per_worker,
        b.workers() as i32 * b.config().threads_per_worker,
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0),
        parts.len(),
        audio_ms as f64 / 1000.0,
        wall.as_secs_f64(),
        load_avg()
    );
    // Again: correctness only. Every segment must have produced real, aligned audio.
    assert_eq!(parts.len(), 8);
    for (i, p) in parts.iter().enumerate() {
        assert!(!p.is_empty(), "pool segment {i} empty");
        assert!(p.is_whole_ms(), "pool segment {i} misaligned");
        assert!(peak(p) > 500, "pool segment {i} silent");
    }
}
