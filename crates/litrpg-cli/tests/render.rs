//! `litrpg render` — queueing chapters for re-render.
//!
//! Nothing here renders or plays audio; `render` clears a flag and the engine does the
//! work, so these tests assert the flag and the reporting.

use litrpg_cli::engine::EngineStatus;
use litrpg_cli::render::{self, Outcome, Selection};
use litrpg_cli::{CliError, play};
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::{NewChapter, Store};
use tempfile::TempDir;

fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-render-")
        .tempdir()
        .unwrap()
}

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

/// No heartbeat: the degraded path, which must still be useful.
fn no_engine() -> EngineStatus {
    EngineStatus::NeverSeen
}

fn live_engine(backends: &[&str]) -> EngineStatus {
    EngineStatus::Reported {
        age_secs: 3,
        stale: false,
        pid: 4242,
        version: "0.1.0".into(),
        backends: backends.iter().map(|b| b.to_string()).collect(),
    }
}

fn chapter(s: &Store, n: u32) {
    s.insert_chapter(&NewChapter {
        number: n,
        title: format!("Chapter {n}"),
        text_md: "text".into(),
        prompt_hash: String::new(),
        state_dirty: false,
    })
    .unwrap();
}

/// Attach audio and write the media files where `render`/`play` will derive them.
fn with_audio(s: &Store, dir: &std::path::Path, n: u32, end_ms: u32) {
    chapter(s, n);
    let m = Manifest::new(
        n,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "sherpa:piper-en_GB-cori-high:0".into(),
            text: "Text.".into(),
            start_ms: 0,
            end_ms,
        }],
    );
    s.attach_audio(n, &m).unwrap();
    std::fs::write(play::media_path(dir, n, "mp3"), b"fake mp3").unwrap();
    std::fs::write(play::media_path(dir, n, "pcm"), b"fake pcm").unwrap();
}

// ------------------------------------------------------ selection parsing

#[test]
fn a_single_chapter_parses() {
    assert_eq!(
        Selection::parse(&["3".to_string()]).unwrap(),
        Selection::These(vec![3])
    );
}

#[test]
fn several_chapters_parse_in_the_order_given() {
    assert_eq!(
        Selection::parse(&["3".into(), "5".into(), "7".into()]).unwrap(),
        Selection::These(vec![3, 5, 7])
    );
}

#[test]
fn an_inclusive_range_parses() {
    assert_eq!(
        Selection::parse(&["3..7".to_string()]).unwrap(),
        Selection::Range { from: 3, to: 7 }
    );
}

#[test]
fn a_single_chapter_range_is_allowed() {
    assert_eq!(
        Selection::parse(&["4..4".to_string()]).unwrap(),
        Selection::Range { from: 4, to: 4 }
    );
}

#[test]
fn a_backwards_range_is_refused_rather_than_silently_empty() {
    let err = Selection::parse(&["7..3".to_string()]).unwrap_err();
    assert!(matches!(err, CliError::BadRange { .. }), "got {err:?}");
    assert!(err.to_string().contains("starts after it ends"), "{err}");
}

#[test]
fn a_non_numeric_selection_is_refused_with_the_accepted_forms() {
    for bad in ["latest", "3-7", "", "3..x", "x..7", "-1"] {
        let err = Selection::parse(&[bad.to_string()]).unwrap_err();
        assert!(
            matches!(err, CliError::BadRange { .. }),
            "{bad:?} should be refused, got {err:?}"
        );
        assert!(err.to_string().contains("3..7"), "{err}");
    }
}

// --------------------------------------------------------- queueing

#[test]
fn queueing_clears_has_audio_so_the_engine_picks_it_up() {
    // The whole mechanism: the engine's resume path only sees chapters with
    // has_audio = false, and before this command nothing could clear it — which is
    // what made a substituted voice permanent.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 137_000);
    assert!(s.chapter(1).unwrap().has_audio);

    let r = render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert_eq!(r.chapters[0].outcome, Outcome::Queued);
    assert_eq!(r.queued(), 1);
    assert!(!s.chapter(1).unwrap().has_audio, "the flag must be cleared");
}

#[test]
fn the_report_says_what_will_happen_not_that_it_rendered() {
    // The wording changed when the heartbeat landed — the engine sentence is now driven
    // by `engine::describe` rather than being unconditional — but the property this test
    // exists for did not: the output must never imply it rendered anything.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 137_000);
    let out = render::render_text(
        &render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap(),
    );
    assert!(out.contains("queued"), "{out}");
    assert!(
        out.contains("renders nothing itself"),
        "must not imply it rendered:\n{out}"
    );
    assert!(
        out.contains("45 seconds"),
        "must name the poll interval:\n{out}"
    );
    assert!(
        !out.to_lowercase().contains("rendered chapter"),
        "must not claim completion:\n{out}"
    );
}

#[test]
fn the_report_warns_that_re_rendering_replaces_the_audio_and_manifest() {
    // Chapters rendered before per-sentence manifests held turn-level ones, including a
    // single 570-second span, and nothing migrated them. Re-rendering changes timings.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 570_000);
    let out = render::render_text(
        &render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap(),
    );
    assert!(out.contains("replaces"), "{out}");
    assert!(out.contains("manifest"), "{out}");
    assert!(out.contains("granularity"), "{out}");
    assert!(
        out.contains("9:30"),
        "should name the duration being replaced:\n{out}"
    );
}

#[test]
fn queueing_an_already_queued_chapter_is_a_noop_worth_reporting() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();

    let again =
        render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert_eq!(again.chapters[0].outcome, Outcome::AlreadyQueued);
    assert_eq!(again.queued(), 0);
    let out = render::render_text(&again);
    assert!(out.contains("already awaiting"), "{out}");
    // Nothing to promise about the engine when nothing changed.
    assert!(!out.contains("45 seconds"), "{out}");
}

#[test]
fn a_chapter_that_never_rendered_is_reported_as_already_queued_not_an_error() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1); // text only
    let r = render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert_eq!(r.chapters[0].outcome, Outcome::AlreadyQueued);
}

#[test]
fn a_missing_chapter_is_refused_naming_the_latest() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let err =
        render::render(&s, &Selection::These(vec![9]), dir.path(), 45, no_engine()).unwrap_err();
    assert!(
        matches!(
            err,
            CliError::NoSuchChapter {
                wanted: 9,
                latest: 1
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_range_containing_a_missing_chapter_queues_nothing() {
    // Refused before any write, so a typo'd range cannot half-apply.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    with_audio(&s, dir.path(), 2, 1000);
    assert!(
        render::render(
            &s,
            &Selection::Range { from: 1, to: 4 },
            dir.path(),
            45,
            no_engine()
        )
        .is_err()
    );
    assert!(
        s.chapter(1).unwrap().has_audio,
        "must not have been cleared"
    );
    assert!(s.chapter(2).unwrap().has_audio);
}

#[test]
fn a_range_queues_every_chapter_in_it() {
    let dir = tmp();
    let s = store();
    for n in 1..=4 {
        with_audio(&s, dir.path(), n, 1000);
    }
    let r = render::render(
        &s,
        &Selection::Range { from: 2, to: 4 },
        dir.path(),
        45,
        no_engine(),
    )
    .unwrap();
    assert_eq!(r.queued(), 3);
    assert!(s.chapter(1).unwrap().has_audio, "1 was outside the range");
    for n in 2..=4 {
        assert!(!s.chapter(n).unwrap().has_audio, "chapter {n}");
    }
}

#[test]
fn all_queues_only_chapters_that_currently_have_audio() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    chapter(&s, 2); // no audio
    with_audio(&s, dir.path(), 3, 1000);

    let r = render::render(&s, &Selection::All, dir.path(), 45, no_engine()).unwrap();
    assert_eq!(r.queued(), 2);
    let queued: Vec<u32> = r.chapters.iter().map(|c| c.chapter).collect();
    assert_eq!(queued, vec![1, 3], "chapter 2 had nothing to replace");
}

#[test]
fn all_on_a_story_with_no_audio_says_so_rather_than_claiming_success() {
    let dir = tmp();
    let s = store();
    chapter(&s, 1);
    let r = render::render(&s, &Selection::All, dir.path(), 45, no_engine()).unwrap();
    assert!(r.chapters.is_empty());
    assert!(render::render_text(&r).contains("nothing to re-render"));
}

#[test]
fn the_report_notes_that_existing_audio_stays_playable() {
    // clear_audio deliberately leaves the media alone, so a queued chapter is not a
    // silent one until the engine overwrites it.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let r = render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert!(r.chapters[0].media_on_disk);
    assert!(render::render_text(&r).contains("stays playable"));
}

#[test]
fn queueing_leaves_the_media_and_the_text_alone() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let mp3 = play::media_path(dir.path(), 1, "mp3");

    render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert!(mp3.is_file(), "the audio must not be deleted");
    assert_eq!(s.chapter(1).unwrap().text_md, "text");
    // Segments survive too: destroying them would lose the only record of audio that
    // is still on disk if the re-render never happens.
    assert_eq!(s.segments(1).unwrap().len(), 1);
}

#[test]
fn the_report_serialises() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let json = serde_json::to_string(
        &render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap(),
    )
    .unwrap();
    assert!(json.contains("\"outcome\":\"queued\""), "{json}");
    assert!(json.contains("poll_interval_secs"), "{json}");
    assert!(json.contains("media_on_disk"), "{json}");
}

// ------------------------------------------- interaction with `play`

#[test]
fn play_calls_a_queued_chapter_queued_rather_than_unrendered() {
    // Before `render` existed, `has_audio = false` meant "never rendered". Now it can
    // also mean "awaiting replacement", and the audio is still on disk and playable —
    // so refusing with "no audio yet" would be a lie.
    let dir = tmp();
    let bin = tmp();
    let exe = bin.path().join("mpv");
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();

    let plan = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some(bin.path().to_str().unwrap()),
        dir.path(),
    )
    .expect("a queued chapter is still playable");
    assert!(plan.queued_for_rerender);
    assert!(play::render_plan(&plan).contains("queued for re-render"));
}

#[test]
fn play_still_refuses_a_chapter_with_no_media_at_all() {
    // The other branch must keep working: never rendered and nothing on disk.
    let dir = tmp();
    let s = store();
    chapter(&s, 1);
    let err = play::plan(
        &s,
        Some(1),
        &play::players(),
        Some("/nonexistent"),
        dir.path(),
    )
    .unwrap_err();
    assert!(
        matches!(err, CliError::ChapterHasNoAudio { chapter: 1 }),
        "got {err:?}"
    );
}

// ------------------------------------------------------- engine heartbeat

#[test]
fn with_no_heartbeat_the_report_degrades_to_stating_the_dependency() {
    // Must stay useful when the engine has never run: say what is required rather than
    // guessing at whether it is satisfied.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let out = render::render_text(
        &render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap(),
    );
    assert!(out.contains("No engine has ever run"), "{out}");
    assert!(out.contains("45 seconds"), "{out}");
    assert!(
        out.contains("renders nothing itself"),
        "must not imply it rendered:\n{out}"
    );
}

#[test]
fn a_live_engine_is_named_with_its_pid_and_backends() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let out = render::render_text(
        &render::render(
            &s,
            &Selection::These(vec![1]),
            dir.path(),
            45,
            live_engine(&["azure", "sherpa"]),
        )
        .unwrap(),
    );
    assert!(out.contains("is running"), "{out}");
    assert!(out.contains("4242"), "{out}");
    assert!(out.contains("azure, sherpa"), "{out}");
    assert!(!out.contains("!!"), "nothing is wrong:\n{out}");
}

#[test]
fn a_stale_engine_is_reported_as_probably_stopped() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let stale = EngineStatus::Reported {
        age_secs: 7200,
        stale: true,
        pid: 99,
        version: "0.1.0".into(),
        backends: vec!["sherpa".into()],
    };
    let out = render::render_text(
        &render::render(&s, &Selection::These(vec![1]), dir.path(), 45, stale).unwrap(),
    );
    assert!(out.contains("probably stopped"), "{out}");
    assert!(out.contains("will not render"), "{out}");
    assert!(out.contains("!!"), "{out}");
}

#[test]
fn a_backend_the_engine_lacks_is_reported_without_promising_restoration() {
    // Since #15 a re-render *does* restore the cast voice — but only if the engine can
    // render it. An azure-only engine substitutes again, so this warning is about the
    // engine's registry, not about the resume path's fidelity.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    s.upsert_cast("narrator", "sherpa:piper-en_GB-cori-high:0", "narrator", 1)
        .unwrap();

    let r = render::render(
        &s,
        &Selection::These(vec![1]),
        dir.path(),
        45,
        live_engine(&["azure"]),
    )
    .unwrap();
    assert_eq!(r.missing_backends, vec!["sherpa".to_string()]);

    let out = render::render_text(&r);
    assert!(out.contains("does not provide"), "{out}");
    assert!(out.contains("substituted again"), "{out}");
    assert!(
        !out.contains("restoring the original"),
        "must not promise a restoration this path cannot perform:\n{out}"
    );
}

#[test]
fn a_cast_the_engine_can_render_produces_no_backend_warning() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    s.upsert_cast("narrator", "sherpa:piper-en_GB-cori-high:0", "narrator", 1)
        .unwrap();
    let r = render::render(
        &s,
        &Selection::These(vec![1]),
        dir.path(),
        45,
        live_engine(&["sherpa", "azure"]),
    )
    .unwrap();
    assert!(r.missing_backends.is_empty(), "{:?}", r.missing_backends);
    assert!(!render::render_text(&r).contains("does not provide"));
}

#[test]
fn an_unknown_registry_makes_no_claim_about_missing_backends() {
    // Silence is not evidence: with no heartbeat we do not know what the engine has, so
    // asserting a backend is missing would be a guess dressed as a finding.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    s.upsert_cast("narrator", "sherpa:piper-en_GB-cori-high:0", "narrator", 1)
        .unwrap();
    let r = render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert!(r.missing_backends.is_empty(), "{:?}", r.missing_backends);
    assert!(!render::render_text(&r).contains("does not provide"));
}

#[test]
fn an_unparseable_cast_voice_ref_is_skipped_rather_than_reported_as_a_backend() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    // upsert_cast does not validate, and older rows may predate validation.
    s.upsert_cast("narrator", "novoice", "narrator", 1).unwrap();
    let r = render::render(
        &s,
        &Selection::These(vec![1]),
        dir.path(),
        45,
        live_engine(&["azure"]),
    )
    .unwrap();
    assert!(r.missing_backends.is_empty(), "{:?}", r.missing_backends);
}

#[test]
fn the_report_serialises_the_engine_state() {
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000);
    let json = serde_json::to_string(
        &render::render(
            &s,
            &Selection::These(vec![1]),
            dir.path(),
            45,
            live_engine(&["azure"]),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(json.contains("\"state\":\"reported\""), "{json}");
    assert!(json.contains("\"pid\":4242"), "{json}");
    assert!(json.contains("missing_backends"), "{json}");
}

// ------------------------------- what a re-render will CHANGE (post-#15)

/// Attach audio whose segment records `recorded_voice`, while the cast asks for
/// `cast_voice` — the state a past substitution leaves behind.
fn with_substituted_audio(
    s: &Store,
    dir: &std::path::Path,
    n: u32,
    cast_voice: &str,
    recorded_voice: &str,
) {
    chapter(s, n);
    s.upsert_cast("narrator", cast_voice, "narrator", 1)
        .unwrap();
    let m = Manifest::new(
        n,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: recorded_voice.into(),
            text: "Text.".into(),
            start_ms: 0,
            end_ms: 1000,
        }],
    );
    s.attach_audio(n, &m).unwrap();
    std::fs::write(play::media_path(dir, n, "mp3"), b"fake mp3").unwrap();
}

#[test]
fn a_chapter_whose_recorded_voice_was_substituted_is_reported_as_changing() {
    // Chapters 3 and 4 of the live story. Before #15 the resume render copied the recorded
    // voice verbatim and these were unfixable; now the voice is re-derived from the cast,
    // so the same detection reports what *will change* rather than what cannot be fixed.
    // The detail stays valuable either way — four segments of narration changing voice is
    // worth knowing before it happens.
    let dir = tmp();
    let s = store();
    with_substituted_audio(
        &s,
        dir.path(),
        3,
        "sherpa:piper-en_GB-cori-high:0",
        "azure:en-GB-AdaMultilingualNeural",
    );

    let r = render::render(
        &s,
        &Selection::These(vec![3]),
        dir.path(),
        45,
        live_engine(&["sherpa", "azure"]),
    )
    .unwrap();
    let d = &r.chapters[0].voice_divergence;
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].speaker, "narrator");
    assert_eq!(d[0].recorded, "azure:en-GB-AdaMultilingualNeural");
    assert_eq!(d[0].cast_says, "sherpa:piper-en_GB-cori-high:0");

    let out = render::render_text(&r);
    assert!(
        out.contains("will\n     change voice") || out.contains("change voice"),
        "{out}"
    );
    assert!(out.contains("chapter 3 · narrator"), "{out}");
    assert!(
        out.contains("azure:en-GB-AdaMultilingualNeural -> sherpa:piper-en_GB-cori-high:0"),
        "must name both voices in the direction of the change:\n{out}"
    );
    // The inverted claim must not survive anywhere.
    assert!(!out.contains("NOT restore"), "{out}");
    assert!(!out.contains("verbatim"), "{out}");
    // Present even though the engine has both backends — this is not a backend problem.
    assert!(r.missing_backends.is_empty());
}

#[test]
fn a_chapter_recorded_with_the_cast_voice_is_not_flagged() {
    // Chapters 1, 2 and 5: already correct, so a re-render genuinely helps them.
    let dir = tmp();
    let s = store();
    with_substituted_audio(
        &s,
        dir.path(),
        1,
        "sherpa:piper-en_GB-cori-high:0",
        "sherpa:piper-en_GB-cori-high:0",
    );
    let r = render::render(
        &s,
        &Selection::These(vec![1]),
        dir.path(),
        45,
        live_engine(&["sherpa"]),
    )
    .unwrap();
    assert!(r.chapters[0].voice_divergence.is_empty());
    assert!(!render::render_text(&r).contains("change voice"));
}

#[test]
fn divergence_is_reported_once_per_speaker_not_once_per_segment() {
    // A re-split chapter has 83 segments; one mis-voiced narrator should report once.
    let dir = tmp();
    let s = store();
    chapter(&s, 1);
    s.upsert_cast("narrator", "sherpa:a:0", "narrator", 1)
        .unwrap();
    let segs: Vec<Segment> = (0..40)
        .map(|i| Segment {
            idx: i,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "azure:b:0".into(),
            text: "Line.".into(),
            start_ms: i * 100,
            end_ms: (i + 1) * 100,
        })
        .collect();
    s.attach_audio(1, &Manifest::new(1, segs)).unwrap();

    let r = render::render(
        &s,
        &Selection::These(vec![1]),
        dir.path(),
        45,
        live_engine(&["sherpa", "azure"]),
    )
    .unwrap();
    assert_eq!(r.chapters[0].voice_divergence.len(), 1);
}

#[test]
fn a_speaker_absent_from_the_cast_is_not_a_divergence() {
    // Nothing to disagree with; claiming one would invent an intent nobody recorded.
    let dir = tmp();
    let s = store();
    with_audio(&s, dir.path(), 1, 1000); // narrator segment, no cast row
    let r = render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    assert!(r.chapters[0].voice_divergence.is_empty());
}

#[test]
fn divergence_is_in_the_json() {
    let dir = tmp();
    let s = store();
    with_substituted_audio(&s, dir.path(), 1, "sherpa:a:0", "azure:b:0");
    let json = serde_json::to_string(
        &render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap(),
    )
    .unwrap();
    assert!(json.contains("voice_divergence"), "{json}");
    assert!(json.contains("\"recorded\":\"azure:b:0\""), "{json}");
    assert!(json.contains("\"cast_says\":\"sherpa:a:0\""), "{json}");
}

#[test]
fn read_segments_shows_the_same_divergence() {
    // The view someone opens to diagnose a suspected mis-cast makes the comparison rather
    // than leaving it to the eye.
    let dir = tmp();
    let s = store();
    with_substituted_audio(&s, dir.path(), 1, "sherpa:a:0", "azure:b:0");
    let v = litrpg_cli::read::read(&s, Some(1)).unwrap();
    assert_eq!(v.voice_divergence.len(), 1);
    let out = litrpg_cli::read::render_segments(&v);
    assert!(out.contains("disagree with the cast"), "{out}");
    assert!(out.contains("change it to"), "{out}");
    assert!(
        !out.contains("will not change it"),
        "the inverted claim must be gone:\n{out}"
    );
}

#[test]
fn speaker_matching_is_case_insensitive_like_the_engine() {
    // `replan_from_store` matches speakers with `eq_ignore_ascii_case`. An exact-match
    // lookup here would report "nothing will change" about a segment spelled `SYSTEM`
    // against a cast row spelled `system` — and then the voice would change anyway. Two
    // places comparing the same names by different rules is the duplication that keeps
    // biting.
    let dir = tmp();
    let s = store();
    chapter(&s, 1);
    s.upsert_cast("system", "sherpa:kokoro-multi-lang-v1_0:24", "system", 1)
        .unwrap();
    s.attach_audio(
        1,
        &Manifest::new(
            1,
            vec![Segment {
                idx: 0,
                speaker: "SYSTEM".into(),
                kind: SpeakerKind::System,
                voice_ref: "azure:en-GB-SoniaNeural".into(),
                text: "Quest updated.".into(),
                start_ms: 0,
                end_ms: 1000,
            }],
        ),
    )
    .unwrap();

    let r = render::render(&s, &Selection::These(vec![1]), dir.path(), 45, no_engine()).unwrap();
    let d = &r.chapters[0].voice_divergence;
    assert_eq!(d.len(), 1, "casing must not hide a real change: {d:?}");
    assert_eq!(d[0].cast_says, "sherpa:kokoro-multi-lang-v1_0:24");
}
