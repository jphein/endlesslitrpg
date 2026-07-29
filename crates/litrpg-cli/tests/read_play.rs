//! `litrpg read` and `litrpg play`.
//!
//! No test here executes a real audio player. `play` is exercised through `plan`,
//! `--print-command` rendering, and injected player argv — `spawn` is driven only
//! with `true`/`false`, never with anything that opens an audio device.

use std::path::{Path, PathBuf};

use litrpg_cli::CliError;
use litrpg_cli::play::{self, Player, Source};
use litrpg_cli::read;
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::{NewChapter, Store};
use tempfile::TempDir;

fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-play-")
        .tempdir()
        .unwrap()
}

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn chapter(s: &Store, n: u32, text: &str) {
    s.insert_chapter(&NewChapter {
        number: n,
        title: format!("Chapter {n}"),
        text_md: text.into(),
        prompt_hash: "fnv1a64:0000000000000000".into(),
        state_dirty: false,
    })
    .unwrap();
}

fn seg(idx: u32, speaker: &str, kind: SpeakerKind, voice: &str, a: u32, b: u32) -> Segment {
    Segment {
        idx,
        speaker: speaker.into(),
        kind,
        voice_ref: voice.into(),
        text: format!("Line {idx}."),
        start_ms: a,
        end_ms: b,
    }
}

/// Attach audio, creating real files so the on-disk checks are exercised.
fn attach(s: &Store, dir: &Path, n: u32, segments: Vec<Segment>) -> (PathBuf, PathBuf) {
    let mp3 = dir.join(format!("{n:04}.mp3"));
    let pcm = dir.join(format!("{n:04}.pcm"));
    std::fs::write(&mp3, b"fake mp3").unwrap();
    std::fs::write(&pcm, b"fake pcm").unwrap();
    let m = Manifest::new(n, segments);
    s.attach_audio(n, &m, pcm.to_str().unwrap(), mp3.to_str().unwrap())
        .unwrap();
    (mp3, pcm)
}

/// A fake executable on a fake PATH, so player resolution is tested without
/// depending on what happens to be installed.
fn fake_exe(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

// ==================================================================== read

#[test]
fn read_prints_the_stored_markdown_verbatim() {
    let s = store();
    chapter(
        &s,
        1,
        "[narrator] The vale smelled of iron.\n\n[Kaelen] \"Not again.\"\n",
    );
    let v = read::read(&s, Some(1)).unwrap();
    assert_eq!(
        v.text_md,
        "[narrator] The vale smelled of iron.\n\n[Kaelen] \"Not again.\"\n"
    );
    let out = read::render_prose(&v);
    assert!(out.contains("The vale smelled of iron."), "{out}");
    assert!(out.contains("Chapter 1"), "{out}");
}

#[test]
fn read_with_no_argument_takes_the_latest() {
    let s = store();
    chapter(&s, 1, "first");
    chapter(&s, 2, "second");
    chapter(&s, 3, "third");
    assert_eq!(read::read(&s, None).unwrap().number, 3);
    assert_eq!(read::read(&s, None).unwrap().text_md, "third");
}

#[test]
fn read_counts_words() {
    let s = store();
    chapter(&s, 1, "one two three four five");
    assert_eq!(read::read(&s, Some(1)).unwrap().words, 5);
    assert!(read::render_prose(&read::read(&s, Some(1)).unwrap()).contains("5 words"));
}

#[test]
fn read_on_an_empty_database_says_there_are_no_chapters() {
    let err = read::read(&store(), None).unwrap_err();
    assert!(matches!(err, CliError::NoChapters), "got {err:?}");
    assert!(err.to_string().contains("no chapters"), "{err}");
}

#[test]
fn a_missing_chapter_names_the_highest_that_exists() {
    let s = store();
    chapter(&s, 1, "a");
    chapter(&s, 2, "b");
    let err = read::read(&s, Some(9)).unwrap_err();
    match &err {
        CliError::NoSuchChapter { wanted, latest } => {
            assert_eq!(*wanted, 9);
            assert_eq!(*latest, 2);
        }
        other => panic!("expected NoSuchChapter, got {other:?}"),
    }
    assert!(err.to_string().contains('2'), "{err}");
}

#[test]
fn chapter_zero_is_rejected_rather_than_silently_meaning_latest() {
    let s = store();
    chapter(&s, 1, "a");
    assert!(matches!(
        read::read(&s, Some(0)).unwrap_err(),
        CliError::NoSuchChapter { wanted: 0, .. }
    ));
}

#[test]
fn a_gap_below_the_latest_is_reported_as_missing_not_returned_empty() {
    // A rewind or failed write can leave a hole, so `n <= latest` does not imply
    // existence. Resolution asks the store rather than assuming.
    let s = store();
    chapter(&s, 1, "a");
    chapter(&s, 5, "e");
    let err = read::read(&s, Some(3)).unwrap_err();
    assert!(
        matches!(
            err,
            CliError::NoSuchChapter {
                wanted: 3,
                latest: 5
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn read_reports_a_chapter_whose_deltas_were_never_extracted() {
    let s = store();
    s.insert_chapter(&NewChapter {
        number: 1,
        title: "Chapter 1".into(),
        text_md: "text".into(),
        prompt_hash: String::new(),
        state_dirty: true,
    })
    .unwrap();
    let v = read::read(&s, Some(1)).unwrap();
    assert!(v.state_dirty);
    let out = read::render_prose(&v);
    assert!(out.contains("state_dirty"), "{out}");
    assert!(out.contains("not in the ledger"), "{out}");
}

// ------------------------------------------------------------ --segments

#[test]
fn segments_view_shows_speaker_kind_voice_and_timing() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![
            seg(
                0,
                "narrator",
                SpeakerKind::Narrator,
                "sherpa:piper-en_GB-cori-high:0",
                0,
                4120,
            ),
            seg(
                1,
                "Kaelen",
                SpeakerKind::Character,
                "sherpa:kokoro-multi-lang-v1_0:18",
                4120,
                9000,
            ),
        ],
    );

    let v = read::read(&s, Some(1)).unwrap();
    assert_eq!(v.segments.len(), 2);
    assert_eq!(v.segments[1].speaker, "Kaelen");
    assert_eq!(v.segments[1].kind, "character");
    assert_eq!(v.segments[1].backend.as_deref(), Some("sherpa"));
    assert_eq!(v.segments[1].duration_ms, 4880);

    let out = read::render_segments(&v);
    assert!(out.contains("narrator"), "{out}");
    assert!(out.contains("Kaelen"), "{out}");
    assert!(out.contains("kokoro-multi-lang-v1_0:18"), "{out}");
    assert!(out.contains("4120-9000"), "{out}");
    assert!(out.contains("Line 1."), "{out}");
}

#[test]
fn segment_byte_offsets_are_materialised_at_32_bytes_per_ms() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            4120,
        )],
    );
    let v = read::read(&s, Some(1)).unwrap();
    assert_eq!(v.segments[0].start_byte, 0);
    assert_eq!(v.segments[0].end_byte, 131_840);
}

#[test]
fn a_chapter_without_audio_has_no_segments_and_says_why() {
    let s = store();
    chapter(&s, 1, "text");
    let v = read::read(&s, Some(1)).unwrap();
    assert!(v.segments.is_empty());
    let out = read::render_segments(&v);
    assert!(out.contains("No segments"), "{out}");
    assert!(
        out.contains("§10"),
        "must explain text-without-audio:\n{out}"
    );
}

fn view(idx: u32, start_ms: u32, end_ms: u32) -> read::SegmentView {
    read::SegmentView {
        idx,
        speaker: "narrator".into(),
        kind: "narrator",
        voice_ref: "sherpa:x:0".into(),
        backend: Some("sherpa".into()),
        text: "Line.".into(),
        start_ms,
        end_ms,
        duration_ms: end_ms - start_ms,
        start_byte: start_ms as u64 * 32,
        end_byte: end_ms as u64 * 32,
    }
}

#[test]
fn a_gap_between_segments_is_detected() {
    // `Store::attach_audio` now rejects a non-contiguous manifest, so this state is
    // unreachable through the public API and the predicate is tested directly. The
    // render-side check stays as defence for a hand-edited database: the symptom is
    // seeking that lands in the wrong segment, which is invisible except to a listener.
    assert_eq!(
        read::first_discontinuity(&[view(0, 0, 1000), view(1, 1500, 2000)]),
        Some((0, 1000, 1, 1500))
    );
}

#[test]
fn an_overlap_between_segments_is_detected_too() {
    assert_eq!(
        read::first_discontinuity(&[view(0, 0, 1000), view(1, 800, 2000)]),
        Some((0, 1000, 1, 800))
    );
}

#[test]
fn segments_not_starting_at_zero_are_detected() {
    // Byte offsets are absolute from the start of the stream, so a first segment
    // that does not begin at 0 shifts everything after it.
    assert_eq!(
        read::first_discontinuity(&[view(0, 120, 1000)]),
        Some((0, 0, 0, 120))
    );
}

#[test]
fn a_contiguous_run_has_no_discontinuity() {
    assert_eq!(
        read::first_discontinuity(&[view(0, 0, 1000), view(1, 1000, 2000), view(2, 2000, 3500)]),
        None
    );
    assert_eq!(read::first_discontinuity(&[]), None);
}

#[test]
fn the_store_refuses_to_create_a_non_contiguous_chapter_at_all() {
    // Confirms where the invariant actually lives: enforced on write, so the
    // read-side check is depth rather than the primary guard.
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    let mp3 = dir.path().join("1.mp3");
    let pcm = dir.path().join("1.pcm");
    std::fs::write(&mp3, b"x").unwrap();
    std::fs::write(&pcm, b"x").unwrap();
    let m = Manifest::new(
        1,
        vec![
            seg(0, "narrator", SpeakerKind::Narrator, "sherpa:x:0", 0, 1000),
            seg(
                1,
                "Kaelen",
                SpeakerKind::Character,
                "sherpa:y:0",
                1500,
                2000,
            ),
        ],
    );
    let err = s
        .attach_audio(1, &m, pcm.to_str().unwrap(), mp3.to_str().unwrap())
        .unwrap_err();
    assert!(
        matches!(err, litrpg_store::StoreError::InvalidManifest { .. }),
        "got {err:?}"
    );
}

#[test]
fn contiguous_segments_are_not_flagged() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![
            seg(0, "narrator", SpeakerKind::Narrator, "sherpa:x:0", 0, 1000),
            seg(
                1,
                "Kaelen",
                SpeakerKind::Character,
                "sherpa:y:0",
                1000,
                2000,
            ),
        ],
    );
    let out = read::render_segments(&read::read(&s, Some(1)).unwrap());
    assert!(!out.contains("not contiguous"), "{out}");
}

#[test]
fn read_serialises_the_chapter_and_its_segments() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            4120,
        )],
    );
    let json = serde_json::to_string(&read::read(&s, Some(1)).unwrap()).unwrap();
    assert!(json.contains("\"has_audio\":true"), "{json}");
    assert!(json.contains("\"start_byte\":0"), "{json}");
    assert!(json.contains("\"end_byte\":131840"), "{json}");
    assert!(json.contains("\"words\":1"), "{json}");
}

// ==================================================================== play

#[test]
fn play_prefers_mpv_with_the_mp3() {
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "mpv");
    let s = store();
    chapter(&s, 1, "text");
    let (mp3, _) = attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            4120,
        )],
    );

    let plan = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap();
    assert_eq!(plan.source, Source::Mp3);
    assert_eq!(plan.path, mp3);
    assert_eq!(plan.argv[0], "mpv");
    assert_eq!(plan.argv.last().unwrap(), &mp3.display().to_string());
    assert_eq!(plan.chapter, 1);
    assert_eq!(plan.duration_ms, 4120);
}

#[test]
fn play_falls_through_to_the_next_installed_player() {
    let dir = tmp();
    let bin = tmp();
    // No mpv; ffplay only.
    fake_exe(bin.path(), "ffplay");
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );

    let plan = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap();
    assert_eq!(plan.argv[0], "ffplay");
    assert_eq!(plan.source, Source::Mp3);
}

#[test]
fn a_pcm_only_player_gets_the_pcm_file_and_explicit_format_flags() {
    // paplay and aplay cannot decode mp3. Handing either an .mp3 would fail, or
    // play compressed bytes as samples. They must get the raw artifact with the
    // §7.1 format spelled out.
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "aplay");
    let s = store();
    chapter(&s, 1, "text");
    let (_, pcm) = attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );

    let plan = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap();
    assert_eq!(plan.source, Source::RawPcm);
    assert_eq!(plan.path, pcm, "must be the .pcm, never the .mp3");
    let line = plan.command_line();
    assert!(line.contains("S16_LE"), "{line}");
    assert!(line.contains("16000"), "rate must be spelled out: {line}");
    assert!(line.contains("-c 1"), "mono must be spelled out: {line}");
    assert!(!line.contains(".mp3"), "{line}");
}

#[test]
fn paplay_gets_pulseaudio_flavoured_raw_flags() {
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "paplay");
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    let line = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap()
    .command_line();
    assert!(line.contains("--raw"), "{line}");
    assert!(line.contains("--format=s16le"), "{line}");
    assert!(line.contains("--rate=16000"), "{line}");
    assert!(line.contains("--channels=1"), "{line}");
}

#[test]
fn the_raw_flags_track_the_sample_rate_constant() {
    // Guards against the flags drifting from §7.1 if the constant ever changes.
    let rate = litrpg_core::manifest::SAMPLE_RATE_HZ.to_string();
    for p in play::players() {
        if p.source == Source::RawPcm {
            assert!(
                p.argv.iter().any(|a| a.contains(&rate)),
                "{:?} does not mention {rate}",
                p.argv
            );
        }
    }
}

#[test]
fn a_pcm_only_player_is_skipped_when_the_pcm_has_been_pruned() {
    // §8 prunes .pcm outside the buffer window. aplay then has no source, and the
    // correct outcome is "no usable player", not handing it the mp3.
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "aplay");
    let s = store();
    chapter(&s, 1, "text");
    let (_, pcm) = attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    std::fs::remove_file(&pcm).unwrap();

    let err = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap_err();
    assert!(matches!(err, CliError::NoPlayer { .. }), "got {err:?}");
    // aplay was never applicable, so it must not be listed as "tried".
    assert!(!err.to_string().contains("aplay"), "misleading: {err}");
}

#[test]
fn no_audio_yet_is_its_own_message_not_chapter_not_found() {
    let s = store();
    chapter(&s, 1, "text shipped, render did not");
    let err = play::plan(&s, Some(1), &play::players(), Some("/nonexistent")).unwrap_err();
    match &err {
        CliError::ChapterHasNoAudio { chapter } => assert_eq!(*chapter, 1),
        other => panic!("expected ChapterHasNoAudio, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("no audio yet"), "{msg}");
    assert!(msg.contains("§10"), "{msg}");
    assert!(
        msg.contains("litrpg status"),
        "must point somewhere:\n{msg}"
    );
}

#[test]
fn recorded_audio_whose_file_is_gone_is_distinct_from_never_rendered() {
    // has_audio is a database flag; the files live on a filesystem that can be
    // pruned or unmounted. "Restore the media" and "wait for a render" are
    // different actions, so they get different errors.
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    let (mp3, pcm) = attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    std::fs::remove_file(&mp3).unwrap();
    std::fs::remove_file(&pcm).unwrap();

    let err = play::plan(&s, Some(1), &play::players(), Some("/nonexistent")).unwrap_err();
    match &err {
        CliError::AudioFileMissing { chapter, looked } => {
            assert_eq!(*chapter, 1);
            assert!(looked.contains(".mp3"), "{looked}");
            assert!(looked.contains(".pcm"), "{looked}");
            assert!(looked.starts_with("no file at"), "{looked}");
        }
        other => panic!("expected AudioFileMissing, got {other:?}"),
    }
}

#[test]
fn audio_recorded_with_no_paths_at_all_says_so_rather_than_trailing_off() {
    // `has_audio = 1` with NULL paths is a different failure from "paths recorded
    // but pruned": the attach never completed. Found by smoke-testing, where the
    // message read "looked for: " with nothing after it.
    let s = store();
    chapter(&s, 1, "text");
    // Cannot reach this through attach_audio, which always writes both paths — so
    // the message is verified via the error type it would produce.
    let err = CliError::AudioFileMissing {
        chapter: 1,
        looked: "no media paths are recorded on the chapter row".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("no media paths are recorded"), "{msg}");
    assert!(!msg.ends_with("looked for: )"), "{msg}");
}

#[test]
fn no_player_installed_names_what_was_tried() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    let empty = tmp();
    let err = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(empty.path().to_str().unwrap()),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, CliError::NoPlayer { .. }), "{msg}");
    for expected in ["mpv", "ffplay", "paplay", "aplay"] {
        assert!(msg.contains(expected), "{expected} missing from: {msg}");
    }
}

#[test]
fn play_with_no_argument_takes_the_latest() {
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "mpv");
    let s = store();
    chapter(&s, 1, "a");
    chapter(&s, 2, "b");
    attach(
        &s,
        dir.path(),
        2,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    let plan = play::plan(
        &s,
        None,
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap();
    assert_eq!(plan.chapter, 2);
}

#[test]
fn play_reports_a_missing_chapter_like_read_does() {
    let s = store();
    chapter(&s, 1, "a");
    assert!(matches!(
        play::plan(&s, Some(7), &play::players(), Some("/nonexistent")).unwrap_err(),
        CliError::NoSuchChapter {
            wanted: 7,
            latest: 1
        }
    ));
}

#[test]
fn the_printed_command_is_the_command_that_would_run() {
    // --print-command must not be able to disagree with the real invocation, so
    // both render the same plan.
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "mpv");
    let s = store();
    chapter(&s, 1, "text");
    let (mp3, _) = attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    let plan = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap();
    let line = plan.command_line();
    assert!(line.starts_with("mpv "), "{line}");
    assert!(line.ends_with(&mp3.display().to_string()), "{line}");
    assert_eq!(line.split(' ').count(), plan.argv.len());
}

#[test]
fn a_path_with_spaces_is_quoted_in_the_printed_command() {
    let dir = tmp();
    let sub = dir.path().join("my media");
    std::fs::create_dir_all(&sub).unwrap();
    let bin = tmp();
    fake_exe(bin.path(), "mpv");
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        &sub,
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    let line = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap()
    .command_line();
    assert!(
        line.contains('"'),
        "unquoted path would break a shell: {line}"
    );
}

#[test]
fn play_serialises_its_plan() {
    let dir = tmp();
    let bin = tmp();
    fake_exe(bin.path(), "mpv");
    let s = store();
    chapter(&s, 1, "text");
    attach(
        &s,
        dir.path(),
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            4120,
        )],
    );
    let plan = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
    )
    .unwrap();
    let json = serde_json::to_string(&plan).unwrap();
    assert!(json.contains("\"source\":\"mp3\""), "{json}");
    assert!(json.contains("\"chapter\":1"), "{json}");
    assert!(json.contains("argv"), "{json}");
}

// ------------------------------------------------------- player resolution

#[test]
fn on_path_finds_an_executable_and_ignores_a_non_executable() {
    let dir = tmp();
    fake_exe(dir.path(), "yes-exec");
    std::fs::write(dir.path().join("not-exec"), "data").unwrap();

    let p = dir.path().to_str().unwrap();
    assert!(play::on_path("yes-exec", Some(p)).is_some());
    assert!(play::on_path("not-exec", Some(p)).is_none());
    assert!(play::on_path("absent", Some(p)).is_none());
}

#[test]
fn on_path_searches_entries_in_order_and_skips_empty_ones() {
    let first = tmp();
    let second = tmp();
    fake_exe(second.path(), "mpv");
    let joined = format!(
        "{}::{}",
        first.path().to_str().unwrap(),
        second.path().to_str().unwrap()
    );
    assert_eq!(
        play::on_path("mpv", Some(&joined)).unwrap(),
        second.path().join("mpv")
    );
}

#[test]
fn on_path_accepts_a_literal_path_without_searching() {
    let dir = tmp();
    let exe = fake_exe(dir.path(), "custom");
    assert_eq!(
        play::on_path(exe.to_str().unwrap(), None).unwrap(),
        exe,
        "an absolute path must not need a PATH"
    );
    assert!(play::on_path("/nope/nothing", None).is_none());
}

#[test]
fn on_path_with_no_path_env_finds_nothing_by_bare_name() {
    assert!(play::on_path("mpv", None).is_none());
}

#[test]
fn every_default_player_declares_a_source_and_a_command() {
    for p in play::players() {
        assert!(!p.argv.is_empty());
        assert!(!p.command().is_empty());
        assert!(matches!(p.source, Source::Mp3 | Source::RawPcm));
    }
}

// --------------------------------------------------------------- spawning
//
// `spawn` is driven only with `true` and `false`. Nothing here opens an audio
// device: running a real player from a background agent would clobber whatever
// else is speaking on the desktop.

fn plan_with(argv: &[&str], dir: &Path, bin: &Path, s: &Store) -> play::PlayPlan {
    chapter(s, 1, "text");
    attach(
        s,
        dir,
        1,
        vec![seg(
            0,
            "narrator",
            SpeakerKind::Narrator,
            "sherpa:x:0",
            0,
            1000,
        )],
    );
    let candidates = vec![Player {
        argv: argv.iter().map(|a| a.to_string()).collect(),
        source: Source::Mp3,
    }];
    play::plan(s, Some(1), &candidates, Some(bin.to_str().unwrap())).unwrap()
}

#[test]
fn a_player_that_succeeds_returns_ok() {
    let dir = tmp();
    let bin = PathBuf::from("/usr/bin:/bin");
    let s = store();
    let plan = plan_with(&["true"], dir.path(), &bin, &s);
    play::spawn(&plan).unwrap();
}

#[test]
fn a_player_that_exits_nonzero_is_reported_with_its_status() {
    let dir = tmp();
    let bin = PathBuf::from("/usr/bin:/bin");
    let s = store();
    let plan = plan_with(&["false"], dir.path(), &bin, &s);
    let err = play::spawn(&plan).unwrap_err();
    match &err {
        CliError::PlayerFailed { status, .. } => assert!(status.contains("exit code"), "{status}"),
        other => panic!("expected PlayerFailed, got {other:?}"),
    }
}
