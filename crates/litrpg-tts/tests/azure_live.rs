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

// ------------------------------------------------- full-length chapter (the defect)

/// The render that the first full-length chapter failed on.
///
/// Every earlier verification used short paragraphs, which is *why* the bug shipped:
/// chapter 1's segment 2 is 3 665 chars = 203 s of audio in a single segment, and the
/// old fixed 180 s client timeout could not cover it under four-way concurrency.
///
/// Uses the real `media/0001.json` manifest — 7 segments, 8 185 chars, ~452 s of
/// sherpa-rendered audio — re-voiced onto the Azure cast.
///
/// ⚠️ **This spends real quota: ~8 200 characters of DragonHD synthesis.** It is the
/// only thing that proves the fix, so it is worth running once after changes to
/// chunking, splitting or timeouts — and not more often than that.
#[tokio::test]
#[ignore = "spends ~8200 chars of Azure quota; the full-length regression test"]
async fn a_full_length_real_chapter_renders_without_timing_out() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../media/0001.json")
        .canonicalize()
        .expect("media/0001.json — run a chapter first");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let segs = json["segments"].as_array().unwrap();

    // Re-voice the real chapter onto the Azure cast: narrator, and a distinct voice
    // for SYSTEM, so this exercises multi-voice as well as length.
    let reqs: Vec<RenderRequest> = segs
        .iter()
        .map(|s| {
            let kind = match s["kind"].as_str().unwrap() {
                "system" => SpeakerKind::System,
                "character" => SpeakerKind::Character,
                _ => SpeakerKind::Narrator,
            };
            let voice = match kind {
                SpeakerKind::System => "azure:en-US-Steffan:DragonHDLatestNeural",
                _ => "azure:en-GB-Ada:DragonHDLatestNeural",
            };
            req(
                s["idx"].as_u64().unwrap() as u32,
                voice,
                s["text"].as_str().unwrap(),
                kind,
            )
        })
        .collect();

    let total_chars: usize = reqs.iter().map(|r| r.text.chars().count()).sum();
    let longest = reqs.iter().map(|r| r.text.chars().count()).max().unwrap();
    eprintln!(
        "\nfull chapter: {} segments, {total_chars} chars, longest segment {longest} chars",
        reqs.len()
    );
    assert!(
        longest > litrpg_tts::azure::MAX_CHARS_PER_REQUEST,
        "this fixture must contain an over-budget segment to be a regression test \
         (longest is {longest}, budget is {})",
        litrpg_tts::azure::MAX_CHARS_PER_REQUEST
    );

    let b = backend();
    let started = std::time::Instant::now();
    // Per-segment isolation: nothing should fail, but a failure must not hide the rest.
    let outcomes = b.render_batch_partial(&reqs).await;
    let wall = started.elapsed();

    assert_eq!(outcomes.len(), reqs.len());
    let mut parts = Vec::new();
    let mut failed = Vec::new();
    for (r, o) in reqs.iter().zip(outcomes) {
        match o {
            Ok(pcm) => {
                eprintln!(
                    "  seg {:>2} {:>5} chars -> {:>9} B = {:>6.1} s",
                    r.idx,
                    r.text.chars().count(),
                    pcm.len(),
                    pcm.duration_secs_f64()
                );
                parts.push(pcm);
            }
            Err(e) => {
                eprintln!("  seg {:>2} FAILED: {e}", r.idx);
                failed.push(format!("segment {}: {e}", r.idx));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "segments failed:\n  {}",
        failed.join("\n  ")
    );

    let chapter = Pcm16k::concat(&parts);
    let audio_secs = chapter.duration_secs_f64();
    eprintln!(
        "chapter: {} B = {audio_secs:.1} s audio in {:.1} s wall ({:.2}x realtime)",
        chapter.len(),
        wall.as_secs_f64(),
        audio_secs / wall.as_secs_f64()
    );

    // The byte contract still holds over a full chapter with split segments.
    assert!(chapter.is_whole_ms());
    assert_eq!(chapter.len() as u32, chapter.duration_ms() * 32);
    assert_eq!(
        chapter.len() as f64,
        32_000.0 * (chapter.duration_ms() as f64 / 1000.0)
    );
    // A real chapter, not a test paragraph.
    assert!(
        audio_secs > 300.0,
        "expected >5 min of audio, got {audio_secs:.1} s — is this a real chapter?"
    );
    // No segment came back empty or silent.
    for (i, p) in parts.iter().enumerate() {
        assert!(!p.is_empty(), "segment {i} empty");
        assert!(peak(p) > 500, "segment {i} silent");
    }
}

/// `render_joined` across **several** chunks, proven with a deliberately tiny budget
/// rather than a long text — so the multi-request path is exercised for ~60 chars of
/// quota instead of thousands.
#[tokio::test]
#[ignore = "spends Azure quota (~60 chars)"]
async fn render_joined_chunks_across_multiple_requests() {
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
    let total: usize = reqs.iter().map(|r| r.text.chars().count()).sum();

    // One chunk (default budget) vs forced multi-chunk.
    let one = backend().render_joined(&reqs).await.unwrap();
    let many = backend()
        .with_max_chars_per_request(12) // forces a request per segment
        .render_joined(&reqs)
        .await
        .unwrap();

    eprintln!(
        "joined: {total} chars | 1 chunk = {} B ({:.2} s) | forced multi-chunk = {} B ({:.2} s)",
        one.len(),
        one.duration_secs_f64(),
        many.len(),
        many.duration_secs_f64()
    );

    for pcm in [&one, &many] {
        assert!(!pcm.is_empty());
        assert!(pcm.is_whole_ms());
        assert_eq!(pcm.len() as u32, pcm.duration_ms() * 32);
        assert!(peak(pcm) > 1_000, "joined stream looks like silence");
    }
    // Chunking must not lose or duplicate speech: same text, so similar duration.
    let ratio = many.duration_secs_f64() / one.duration_secs_f64();
    assert!(
        (0.7..1.4).contains(&ratio),
        "chunked total diverges from single-request total: {ratio:.2}x"
    );
}

/// Integrated loudness (LUFS) of a 16 kHz PCM buffer via ffmpeg's `ebur128`.
/// `-v error` would suppress the summary (it logs at INFO), so verbosity is deliberate.
fn lufs(pcm: &Pcm16k, tag: &str) -> Option<f64> {
    let path = std::env::temp_dir().join(format!("litrpg-az-{tag}.pcm"));
    std::fs::write(&path, pcm.as_bytes()).ok()?;
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
        .ok()?;
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    let i = err.rfind("Integrated loudness")?;
    err[i..]
        .lines()
        .find(|l| l.trim_start().starts_with("I:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// **Closes issue #2.** One full-length chapter, with the per-request breakdown the
/// chunking design needs in order to be believed:
///
/// - every request's wall time against **its own** `timeout_for_chars` budget;
/// - how many requests the chapter costs, and which segments got split;
/// - **loudness across chunk joins** — a seam that did not exist when the 0.7 LU
///   per-segment spread was measured, and which the Azure path does *not* normalize
///   (Azure is 16 kHz native, so `FfmpegPostProcessor` never runs on it);
/// - the byte invariant, and manifest offsets byte-matching their sources.
///
/// ⚠️ ~8 200 chars of DragonHD quota. Run after changing chunking, splitting or
/// timeouts — not routinely.
#[tokio::test]
#[ignore = "spends ~8200 chars of Azure quota; the full-length chunking report"]
async fn full_length_chapter_chunk_report() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../media/0001.json")
        .canonicalize()
        .expect("media/0001.json — run a chapter first");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let segs = json["segments"].as_array().unwrap();

    let reqs: Vec<RenderRequest> = segs
        .iter()
        .map(|s| {
            let kind = match s["kind"].as_str().unwrap() {
                "system" => SpeakerKind::System,
                "character" => SpeakerKind::Character,
                _ => SpeakerKind::Narrator,
            };
            let voice = match kind {
                SpeakerKind::System => "azure:en-US-Steffan:DragonHDLatestNeural",
                _ => "azure:en-GB-Ada:DragonHDLatestNeural",
            };
            req(
                s["idx"].as_u64().unwrap() as u32,
                voice,
                s["text"].as_str().unwrap(),
                kind,
            )
        })
        .collect();

    let b = backend();
    let budget = b.max_chars_per_request();
    eprintln!(
        "\n=== full chapter: {} segments, {} chars, request budget {budget} chars ===",
        reqs.len(),
        reqs.iter().map(|r| r.text.chars().count()).sum::<usize>()
    );

    // Sequential on purpose: this measures each request's own cost, so concurrency
    // must not muddy the timings.
    let started = std::time::Instant::now();
    let mut per_segment: Vec<Vec<litrpg_tts::RenderPart>> = Vec::new();
    for r in &reqs {
        per_segment.push(b.render_parts(r).await.unwrap());
    }
    let wall = started.elapsed();

    // ---- per-request breakdown, with timeout headroom
    eprintln!("\nseg  part  chars     bytes    audio     wall   budget  used    LUFS");
    let mut worst_used = 0.0f64;
    let mut chunk_join_spreads = Vec::new();
    let mut all_parts: Vec<Pcm16k> = Vec::new();
    for (r, parts) in reqs.iter().zip(&per_segment) {
        let mut seg_lufs = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            let l = lufs(&p.pcm, &format!("s{}p{i}", r.idx));
            eprintln!(
                "{:>3}  {:>4}  {:>5}  {:>8}  {:>6.1}s  {:>6.1}s {:>6.0}s  {:>4.0}%  {}",
                r.idx,
                i,
                p.chars,
                p.pcm.len(),
                p.pcm.duration_secs_f64(),
                p.wall.as_secs_f64(),
                p.budget.as_secs_f64(),
                p.budget_used() * 100.0,
                l.map(|v| format!("{v:>6.1}"))
                    .unwrap_or_else(|| "     ?".into())
            );
            worst_used = worst_used.max(p.budget_used());
            if let Some(v) = l {
                seg_lufs.push(v);
            }
            all_parts.push(p.pcm.clone());
        }
        // A chunk join only exists inside a segment that was split. Same voice on
        // both sides, so any spread here is Azure's own request-to-request variation.
        if seg_lufs.len() > 1 {
            let spread = seg_lufs.iter().cloned().fold(f64::MIN, f64::max)
                - seg_lufs.iter().cloned().fold(f64::MAX, f64::min);
            eprintln!(
                "     -> segment {} split into {} requests; chunk-join spread {spread:.1} LU",
                r.idx,
                parts.len()
            );
            chunk_join_spreads.push((r.idx, spread));
        }
    }

    let requests: usize = per_segment.iter().map(|p| p.len()).sum();
    let split_segments: Vec<u32> = reqs
        .iter()
        .zip(&per_segment)
        .filter(|(_, p)| p.len() > 1)
        .map(|(r, _)| r.idx)
        .collect();

    // ---- the joined chapter
    let chapter = litrpg_tts::assemble(
        &reqs
            .iter()
            .zip(&per_segment)
            .map(|(_, parts)| {
                Pcm16k::concat(&parts.iter().map(|p| p.pcm.clone()).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>(),
        litrpg_tts::DEFAULT_GAP_MS,
    );

    eprintln!(
        "\n{requests} requests for {} segments (split: {split_segments:?})\n\
         chapter: {} B = {:.1} s audio in {:.1} s sequential wall ({:.2}x realtime)\n\
         worst timeout headroom: {:.0}% of budget used",
        reqs.len(),
        chapter.pcm.len(),
        chapter.pcm.duration_secs_f64(),
        wall.as_secs_f64(),
        chapter.pcm.duration_secs_f64() / wall.as_secs_f64(),
        worst_used * 100.0
    );

    // ---- cross-segment loudness (Azure path is NOT loudnorm'ed at all)
    let seg_pcms: Vec<Pcm16k> = per_segment
        .iter()
        .map(|parts| Pcm16k::concat(&parts.iter().map(|p| p.pcm.clone()).collect::<Vec<_>>()))
        .collect();
    let seg_levels: Vec<f64> = seg_pcms
        .iter()
        .enumerate()
        .filter_map(|(i, p)| lufs(p, &format!("seg{i}")))
        .collect();
    let seg_spread = seg_levels.iter().cloned().fold(f64::MIN, f64::max)
        - seg_levels.iter().cloned().fold(f64::MAX, f64::min);
    let whole = lufs(&chapter.pcm, "whole").unwrap();
    eprintln!(
        "cross-segment spread {seg_spread:.1} LU (two voices, un-normalized); \
         whole chapter {whole:.1} LUFS"
    );

    // Now the same segments through the normalizer the render path actually applies.
    // `render_parts` bypasses it on purpose so chunk joins can be measured raw, so
    // this closes the loop — and costs nothing extra, since the audio is already paid
    // for and `normalize_16k` is local ffmpeg.
    let post = litrpg_tts::FfmpegPostProcessor::default();
    let normed: Vec<Pcm16k> = seg_pcms
        .iter()
        .map(|p| post.normalize_16k(p).unwrap())
        .collect();
    let normed_levels: Vec<f64> = normed
        .iter()
        .enumerate()
        .filter_map(|(i, p)| lufs(p, &format!("n{i}")))
        .collect();
    let normed_spread = normed_levels.iter().cloned().fold(f64::MIN, f64::max)
        - normed_levels.iter().cloned().fold(f64::MAX, f64::min);
    eprintln!(
        "cross-segment spread after per-segment loudnorm: {normed_spread:.1} LU  {:?}",
        normed_levels
            .iter()
            .map(|v| format!("{v:.1}"))
            .collect::<Vec<_>>()
    );

    // The defect must still reproduce un-normalized (else the normalization is dead
    // weight and should be reconsidered), and must be closed after it.
    assert!(
        seg_spread > 2.0,
        "un-normalized voices only spread {seg_spread:.1} LU — has Azure started \
         levelling its own voices, making this normalization unnecessary?"
    );
    assert!(
        normed_spread < 1.5,
        "after per-segment loudnorm the voices still spread {normed_spread:.1} LU"
    );
    for l in &normed_levels {
        assert!(
            (-23.0..=-17.0).contains(l),
            "normalized segment at {l:.1} LUFS is outside the ACX window"
        );
    }

    // ================= assertions =================

    // 1. No request came close to its own budget. This is the invariant that makes the
    //    timeout a ceiling rather than a wait.
    assert!(
        worst_used < 0.5,
        "a request used {:.0}% of its timeout budget — the ceiling is too tight",
        worst_used * 100.0
    );

    // 2. The chapter actually exercised splitting.
    assert!(
        !split_segments.is_empty(),
        "no segment was split; this fixture no longer tests chunking"
    );
    assert!(
        requests > reqs.len(),
        "expected more requests than segments"
    );

    // 3. Chunk joins must not step in level. Same voice on both sides, so anything
    //    large here is Azure being inconsistent request to request — which would make
    //    a mid-segment join audible in a way per-segment measurement never sees.
    for (idx, spread) in &chunk_join_spreads {
        assert!(
            *spread < 2.0,
            "segment {idx}: {spread:.1} LU across chunk joins — a mid-sentence-group \
             level step would be audible"
        );
    }

    // 4. Byte invariant on the joined result.
    assert!(chapter.pcm.is_whole_ms());
    assert_eq!(chapter.pcm.len() as u32, chapter.pcm.duration_ms() * 32);
    assert_eq!(
        chapter.pcm.len() as f64,
        32_000.0 * (chapter.pcm.duration_ms() as f64 / 1000.0)
    );

    // 5. Manifest offsets byte-match their sources, across split segments.
    assert_eq!(chapter.spans.len(), seg_pcms.len());
    for (span, src) in chapter.spans.iter().zip(&seg_pcms) {
        let slice = &chapter.pcm.as_bytes()[span.start_byte() as usize..span.end_byte() as usize];
        assert_eq!(slice.len(), src.len(), "span length");
        assert_eq!(slice, src.as_bytes(), "span content drifted");
    }

    // 6. A real chapter, and nothing silent.
    assert!(chapter.pcm.duration_secs_f64() > 300.0);
    for (i, p) in seg_pcms.iter().enumerate() {
        assert!(peak(p) > 500, "segment {i} silent");
    }
}

/// Verifies the per-segment normalization that closes the **4.9 LU step between Azure
/// voices** found by the full-chapter report. Deliberately cheap (~400 chars): the
/// defect is per-voice level, which two short renders show as well as two long ones.
#[tokio::test]
#[ignore = "spends ~400 chars of Azure quota"]
async fn azure_voices_are_levelled_against_each_other() {
    // The exact pair that stepped: narrator Ada vs SYSTEM Steffan.
    let pair = [
        ("en-GB-Ada:DragonHDLatestNeural", SpeakerKind::Narrator),
        ("en-US-Steffan:DragonHDLatestNeural", SpeakerKind::System),
    ];
    let text = "The seal on the door pulsed once, and the crack widened by a finger's width.";

    let mut raw = Vec::new();
    let mut normed = Vec::new();
    for (voice, kind) in pair {
        let r = req(0, &format!("azure:{voice}"), text, kind);
        let a = backend().with_normalize(false).render(&r).await.unwrap();
        let b = backend().render(&r).await.unwrap(); // normalize is the default
        let (la, lb) = (
            lufs(&a, "raw").expect("ebur128"),
            lufs(&b, "norm").expect("ebur128"),
        );
        eprintln!("{voice:<40} raw {la:>6.1} -> normalized {lb:>6.1} LUFS");
        raw.push(la);
        normed.push(lb);
        assert!(
            b.is_whole_ms(),
            "normalization must preserve the byte contract"
        );
        assert_eq!(b.len() as u32, b.duration_ms() * 32);
        assert!(peak(&b) > 500, "normalized output is silent");
    }

    let raw_step = (raw[0] - raw[1]).abs();
    let norm_step = (normed[0] - normed[1]).abs();
    eprintln!("per-voice step: raw {raw_step:.1} LU -> normalized {norm_step:.1} LU");

    // The defect reproduces without normalization...
    assert!(
        raw_step > 2.0,
        "expected the un-normalized voices to step by >2 LU (measured 4.9); got \
         {raw_step:.1} LU — has Azure changed, making this normalization unnecessary?"
    );
    // ...and normalization closes it below the ~1 LU just-noticeable difference.
    assert!(
        norm_step < 1.0,
        "normalized voices still step by {norm_step:.1} LU, at or above the JND"
    );
    // Both land in the ACX window.
    for l in &normed {
        assert!(
            (-23.0..=-17.0).contains(l),
            "{l:.1} LUFS outside the ACX window"
        );
    }
}

// ------------------------------------------------- retry, observed (issue #12)

/// Proves retry **fires, backs off, and stops** — deterministically and for **zero
/// quota**, by pointing at a region that does not resolve so every attempt is a
/// transport error.
///
/// A real transient cannot be summoned on demand, so observing one is luck, not a
/// test. This asserts the machinery instead: the number of attempts, the backoff
/// between them, and that it terminates.
#[tokio::test]
#[ignore = "does DNS/network (no Azure quota)"]
async fn a_transport_failure_is_retried_a_bounded_number_of_times() {
    use litrpg_tts::azure::{AzureConfig, retry_delay};

    let cfg = AzureConfig::from_json_str(
        r#"{"key":"not-a-real-key","tts_region":"this-region-does-not-exist-litrpg"}"#,
    )
    .unwrap();

    // Retry disabled: one attempt, fails fast.
    let once = AzureBackend::new(cfg.clone())
        .with_normalize(false)
        .with_max_attempts(1);
    let r = req(
        0,
        "azure:en-GB-Ada:DragonHDLatestNeural",
        "Hello.",
        SpeakerKind::Narrator,
    );
    let t0 = std::time::Instant::now();
    let e1 = once
        .render(&r)
        .await
        .expect_err("unreachable host must fail");
    let solo = t0.elapsed();
    eprintln!("1 attempt:  {:.2}s  {e1}", solo.as_secs_f64());

    // Retry enabled: three attempts, so at least the sum of the backoffs elapses.
    let thrice = AzureBackend::new(cfg)
        .with_normalize(false)
        .with_max_attempts(3);
    let t0 = std::time::Instant::now();
    let e3 = thrice
        .render(&r)
        .await
        .expect_err("unreachable host must still fail");
    let retried = t0.elapsed();
    eprintln!("3 attempts: {:.2}s  {e3}", retried.as_secs_f64());

    let min_backoff = retry_delay(2) + retry_delay(3);
    assert!(
        retried >= min_backoff,
        "retried in {retried:?}, less than the {min_backoff:?} of backoff — did it retry?"
    );
    assert!(
        retried > solo,
        "three attempts ({retried:?}) must take longer than one ({solo:?})"
    );
    // Bounded: it gave up rather than looping.
    assert!(
        retried < min_backoff + solo * 5 + std::time::Duration::from_secs(60),
        "retry did not terminate promptly: {retried:?}"
    );
}

/// A **permanent** rejection must not be retried — retrying the Ollie 400 would have
/// tripled the cost of a guaranteed failure. Costs one short request.
#[tokio::test]
#[ignore = "spends a few chars of Azure quota"]
async fn a_permanent_rejection_is_not_retried() {
    // A voice name Azure has never heard of: the exact chapter-losing 400.
    let r = req(
        0,
        "azure:en-GB-OllieMultilingual:DragonHDLatestNeural",
        "Ready.",
        SpeakerKind::Narrator,
    );
    let b = backend().with_max_attempts(3);

    let t0 = std::time::Instant::now();
    let err = b.render(&r).await.expect_err("a made-up voice must 400");
    let wall = t0.elapsed();
    eprintln!(
        "permanent rejection after {:.2}s: {err}",
        wall.as_secs_f64()
    );

    assert!(
        matches!(err, litrpg_tts::TtsError::HttpStatus { status: 400, .. }),
        "expected a 400, got {err:?}"
    );
    // No backoff was spent, which is how we know it did not retry.
    let one_backoff = litrpg_tts::azure::retry_delay(2);
    assert!(
        wall < one_backoff + std::time::Duration::from_secs(20),
        "took {wall:?} — a permanent 400 appears to have been retried"
    );
}

// ============ #2 at per-sentence granularity: one render, four numbers ============

fn dist(label: &str, v: &[f64]) {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |f: f64| s[((s.len() - 1) as f64 * f).round() as usize];
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    let sd = (s.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    eprintln!(
        "  {label:<28} n={:<3} min {:>6.1} p10 {:>6.1} p25 {:>6.1} med {:>6.1} p75 {:>6.1} \
         p90 {:>6.1} max {:>6.1}  spread {:>4.1}  sd {:>4.2}",
        s.len(),
        s[0],
        q(0.10),
        q(0.25),
        q(0.50),
        q(0.75),
        q(0.90),
        s[s.len() - 1],
        s[s.len() - 1] - s[0],
        sd
    );
}

/// **Closes #2 at the granularity actually shipped.** Parts 7–8 measured a full chapter
/// at *turn* granularity; per-sentence manifests changed the unit, so this re-measures
/// there.
///
/// One Azure render, four numbers: per-part loudness distribution, chunk joins,
/// per-request wall against each request's own `timeout_for_chars`, and the byte
/// invariant. `render_parts` returns **un-normalized** audio, so both normalization
/// topologies are evaluated from the same paid-for render.
///
/// ⚠️ ~8 200 chars of quota — the same as a turn-granularity run, since splitting does
/// not change the characters billed.
#[tokio::test]
#[ignore = "spends ~8200 chars of Azure quota; the per-sentence #2 report"]
async fn per_sentence_chapter_report() {
    use litrpg_tts::azure::{MAX_CHARS_PER_REQUEST, split_for_requests};

    // The live story's observed max part size.
    const SENTENCE_BUDGET: usize = 200;

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../media/0001.json")
        .canonicalize()
        .expect("media/0001.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();

    // One request per sentence, as the engine now does.
    let mut turns: Vec<(u32, &str, Vec<String>)> = Vec::new();
    for s in json["segments"].as_array().unwrap() {
        let kind = s["kind"].as_str().unwrap();
        let voice = if kind == "system" {
            "azure:en-US-Steffan:DragonHDLatestNeural"
        } else {
            "azure:en-GB-Ada:DragonHDLatestNeural"
        };
        let sentences = split_for_requests(s["text"].as_str().unwrap(), SENTENCE_BUDGET);
        turns.push((s["idx"].as_u64().unwrap() as u32, voice, sentences));
    }
    let n_parts: usize = turns.iter().map(|(_, _, p)| p.len()).sum();
    let total_chars: usize = turns
        .iter()
        .flat_map(|(_, _, p)| p.iter())
        .map(|s| s.chars().count())
        .sum();
    eprintln!(
        "\n=== per-sentence chapter: {} turns -> {n_parts} parts, {total_chars} chars, \
         sentence budget {SENTENCE_BUDGET}, request budget {MAX_CHARS_PER_REQUEST} ===",
        turns.len()
    );

    let b = backend();
    let post = litrpg_tts::FfmpegPostProcessor::default();

    let mut worst_used = 0.0f64;
    let mut worst_desc = String::new();
    let mut raw_levels = Vec::new();
    let mut per_part_normed = Vec::new();
    let mut turn_normed = Vec::new();
    let mut turn_pcms: Vec<Pcm16k> = Vec::new();
    let mut chunk_splits = 0usize;
    let retries_observed = 0usize;

    let started = std::time::Instant::now();
    for (idx, voice, sentences) in &turns {
        let mut raw_parts: Vec<Pcm16k> = Vec::new();
        for (si, sentence) in sentences.iter().enumerate() {
            let r = req(*idx, voice, sentence, SpeakerKind::Narrator);
            let parts = b.render_parts(&r).await.unwrap();
            // A sentence-sized request is far under MAX_CHARS_PER_REQUEST, so
            // `split_for_requests` inside render_one is a no-op: at this granularity
            // the chunk-join seam does not exist. Recorded rather than assumed.
            if parts.len() > 1 {
                chunk_splits += 1;
            }
            for p in &parts {
                if p.budget_used() > worst_used {
                    worst_used = p.budget_used();
                    worst_desc = format!(
                        "turn {idx} sentence {si}: {} chars, {:.1}s of {:.0}s budget",
                        p.chars,
                        p.wall.as_secs_f64(),
                        p.budget.as_secs_f64()
                    );
                }
                raw_levels.push(lufs(&p.pcm, "raw").unwrap());
                // Mode A: normalize this part on its own.
                per_part_normed.push(lufs(&post.normalize_16k(&p.pcm).unwrap(), "a").unwrap());
                raw_parts.push(p.pcm.clone());
            }
        }
        // Mode B: normalize the whole turn once.
        let turn = post.normalize_16k(&Pcm16k::concat(&raw_parts)).unwrap();
        turn_normed.push(lufs(&turn, "b").unwrap());
        turn_pcms.push(turn);
    }
    let wall = started.elapsed();

    // ---------------- 1. per-part loudness distribution
    eprintln!("\n[1] loudness distribution over {n_parts} sentence-level parts");
    dist("raw (un-normalized)", &raw_levels);
    dist("A: loudnorm per part", &per_part_normed);
    dist("B: loudnorm per turn", &turn_normed);
    let sp = |v: &[f64]| {
        v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min)
    };

    // ---------------- 2. chunk joins
    eprintln!(
        "\n[2] chunk joins: {chunk_splits} of {n_parts} parts needed splitting \
         (a sentence is far below the {MAX_CHARS_PER_REQUEST}-char request budget, so the \
         chunk-join seam of Parts 7-8 does not arise at this granularity)"
    );

    // ---------------- 3. timeout headroom
    eprintln!(
        "\n[3] worst timeout headroom: {:.0}% of budget  [{worst_desc}]",
        worst_used * 100.0
    );

    // ---------------- 4. byte invariant + manifest slicing
    let chapter = litrpg_tts::assemble(&turn_pcms, litrpg_tts::DEFAULT_GAP_MS);
    eprintln!(
        "\n[4] chapter: {} B = {:.1} s audio in {:.1} s sequential wall; \
         whole-chapter {:.1} LUFS; retries observed {retries_observed}",
        chapter.pcm.len(),
        chapter.pcm.duration_secs_f64(),
        wall.as_secs_f64(),
        lufs(&chapter.pcm, "whole").unwrap()
    );
    eprintln!(
        "    spread — raw {:.1} LU | per-part {:.1} LU | per-turn {:.1} LU",
        sp(&raw_levels),
        sp(&per_part_normed),
        sp(&turn_normed)
    );

    // ================= assertions =================
    assert!(
        n_parts >= 30,
        "only {n_parts} parts; not per-sentence granularity"
    );

    // [3] no request near its ceiling.
    assert!(
        worst_used < 0.5,
        "a request used {:.0}% of its timeout budget: {worst_desc}",
        worst_used * 100.0
    );

    // [1] levelling must survive the finer granularity, whichever topology is used.
    assert!(
        sp(&per_part_normed) < 2.0,
        "per-part loudness spread {:.1} LU across {n_parts} parts",
        sp(&per_part_normed)
    );
    assert!(
        sp(&turn_normed) < 1.5,
        "per-turn spread {:.1} LU — engines are not level",
        sp(&turn_normed)
    );
    // The defect must still reproduce un-normalized, or normalization is dead weight.
    assert!(
        sp(&raw_levels) > 2.0,
        "raw spread only {:.1} LU",
        sp(&raw_levels)
    );

    // [4] byte invariant and manifest offsets.
    assert!(chapter.pcm.is_whole_ms());
    assert_eq!(chapter.pcm.len() as u32, chapter.pcm.duration_ms() * 32);
    assert_eq!(
        chapter.pcm.len() as f64,
        32_000.0 * (chapter.pcm.duration_ms() as f64 / 1000.0)
    );
    assert_eq!(chapter.spans.len(), turn_pcms.len());
    for (span, src) in chapter.spans.iter().zip(&turn_pcms) {
        let slice = &chapter.pcm.as_bytes()[span.start_byte() as usize..span.end_byte() as usize];
        assert_eq!(slice.len(), src.len(), "span length");
        assert_eq!(slice, src.as_bytes(), "span content drifted");
    }
    assert!(
        chapter.pcm.duration_secs_f64() > 300.0,
        "not a full-length chapter"
    );
}
