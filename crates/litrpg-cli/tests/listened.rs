use litrpg_cli::listened::{self, BufferState};
use litrpg_cli::{CliError, status};
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::{NewChapter, NewStory, Store};
use std::path::Path;

/// A store with a story row, since the cursor lives on it.
fn store() -> Store {
    let s = Store::open_in_memory().unwrap();
    s.upsert_story(&NewStory {
        title: "The Ashen Vale".into(),
        protagonist: "Kaelen".into(),
        prompt_path: "/tmp/nonexistent/prompt.md".into(),
        prompt_hash: "fnv1a64:0000000000000000".into(),
        target_words: 2000,
    })
    .unwrap();
    s
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

fn audio(s: &Store, n: u32) {
    let m = Manifest::new(
        n,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "sherpa:x:0".into(),
            text: "Text.".into(),
            start_ms: 0,
            end_ms: 1000,
        }],
    );
    s.attach_audio(n, &m).unwrap();
}

/// `n` chapters, all rendered.
fn rendered(n: u32) -> Store {
    let s = store();
    for i in 1..=n {
        chapter(&s, i);
        audio(&s, i);
    }
    s
}

// ------------------------------------------------------------------ show

#[test]
fn showing_the_cursor_on_a_fresh_story_reports_nothing_listened() {
    let r = listened::show(&store(), 3).unwrap();
    assert_eq!(r.position, 0);
    assert_eq!(r.previous, None);
    assert!(!r.changed());
    assert!(!r.moved_backwards);
    let out = listened::render_text(&r);
    assert!(out.contains("nothing yet"), "{out}");
}

#[test]
fn showing_does_not_change_the_cursor() {
    let s = rendered(3);
    listened::set(&s, 2, 3).unwrap();
    listened::show(&s, 3).unwrap();
    assert_eq!(s.consumed_through().unwrap(), 2);
}

// ------------------------------------------------------------------- set

#[test]
fn setting_the_cursor_records_it_and_reports_what_is_ahead() {
    let s = rendered(5);
    let r = listened::set(&s, 2, 3).unwrap();
    assert_eq!(r.position, 2);
    assert_eq!(r.previous, Some(0));
    assert!(r.changed());
    assert_eq!(s.consumed_through().unwrap(), 2);
    // Chapters 3, 4, 5 are rendered and consecutive from the cursor.
    assert_eq!(r.buffer.playable_ahead, 3);
    assert_eq!(r.buffer.chapters_ahead, 3);
    assert_eq!(r.buffer_state, BufferState::At);
    assert!(r.buffer.buffer_ok);
}

#[test]
fn the_report_names_the_buffer_state_against_the_target() {
    let s = rendered(5);
    assert_eq!(
        listened::set(&s, 4, 3).unwrap().buffer_state,
        BufferState::Below
    );
    assert_eq!(
        listened::set(&s, 2, 3).unwrap().buffer_state,
        BufferState::At
    );
    assert_eq!(
        listened::set(&s, 1, 3).unwrap().buffer_state,
        BufferState::Above
    );
}

#[test]
fn the_shortfall_says_how_many_more_chapters_are_needed() {
    let s = rendered(5);
    let r = listened::set(&s, 4, 3).unwrap();
    assert_eq!(r.buffer.playable_ahead, 1);
    assert_eq!(r.buffer.shortfall(), 2);
    let out = listened::render_text(&r);
    assert!(out.contains("2 more chapter"), "{out}");
    assert!(out.contains("below target"), "{out}");
}

#[test]
fn zero_is_a_valid_position_meaning_not_started() {
    // Not a missing chapter — a legitimate place to be.
    let s = rendered(3);
    listened::set(&s, 3, 3).unwrap();
    let r = listened::set(&s, 0, 3).unwrap();
    assert_eq!(r.position, 0);
    assert_eq!(s.consumed_through().unwrap(), 0);
    assert_eq!(r.buffer.playable_ahead, 3);
}

#[test]
fn zero_is_accepted_even_with_no_chapters_at_all() {
    let r = listened::set(&store(), 0, 3).unwrap();
    assert_eq!(r.position, 0);
    assert_eq!(r.latest_chapter, 0);
}

#[test]
fn a_chapter_that_does_not_exist_is_refused_naming_the_latest() {
    // A typo must not park the cursor past the story and make the buffer look full.
    let s = rendered(3);
    let err = listened::set(&s, 30, 3).unwrap_err();
    match &err {
        CliError::NoSuchChapter { wanted, latest } => {
            assert_eq!(*wanted, 30);
            assert_eq!(*latest, 3);
        }
        other => panic!("expected NoSuchChapter, got {other:?}"),
    }
    assert_eq!(
        s.consumed_through().unwrap(),
        0,
        "must not have been written"
    );
}

#[test]
fn a_nonzero_chapter_with_no_chapters_at_all_says_so() {
    let err = listened::set(&store(), 4, 3).unwrap_err();
    assert!(matches!(err, CliError::NoChapters), "got {err:?}");
}

#[test]
fn a_gap_in_the_sequence_is_refused_like_read_does() {
    let s = store();
    chapter(&s, 1);
    chapter(&s, 5);
    assert!(matches!(
        listened::set(&s, 3, 3).unwrap_err(),
        CliError::NoSuchChapter {
            wanted: 3,
            latest: 5
        }
    ));
}

#[test]
fn setting_the_cursor_needs_a_story_row() {
    // Recording progress against a story that does not exist would make the cursor
    // mean nothing.
    let s = Store::open_in_memory().unwrap();
    chapter(&s, 1);
    let err = listened::set(&s, 1, 3).unwrap_err();
    assert!(
        matches!(err, CliError::Store(litrpg_store::StoreError::NoStoryRow)),
        "got {err:?}"
    );
}

// ------------------------------------------------------------- backwards

#[test]
fn moving_backwards_is_allowed_and_reported() {
    let s = rendered(5);
    listened::set(&s, 4, 3).unwrap();
    let r = listened::set(&s, 1, 3).unwrap();
    assert_eq!(r.position, 1);
    assert_eq!(r.previous, Some(4));
    assert!(r.moved_backwards);
    assert_eq!(
        s.consumed_through().unwrap(),
        1,
        "the move must have happened"
    );

    let out = listened::render_text(&r);
    assert!(out.contains("moved backwards"), "{out}");
    assert!(out.contains("allowed"), "must not read as an error:\n{out}");
    assert!(
        out.contains("generate less"),
        "must say the consequence:\n{out}"
    );
}

#[test]
fn moving_forwards_is_not_flagged_as_backwards() {
    let s = rendered(5);
    listened::set(&s, 1, 3).unwrap();
    let r = listened::set(&s, 3, 3).unwrap();
    assert!(!r.moved_backwards);
    assert!(!listened::render_text(&r).contains("moved backwards"));
}

#[test]
fn re_setting_the_same_position_is_neither_a_change_nor_backwards() {
    let s = rendered(5);
    listened::set(&s, 2, 3).unwrap();
    let r = listened::set(&s, 2, 3).unwrap();
    assert!(!r.changed());
    assert!(!r.moved_backwards);
    assert!(listened::render_text(&r).contains("unchanged"));
}

// ------------------------------------------------- gaps and playability

#[test]
fn playable_ahead_stops_at_an_unrendered_gap() {
    // Chapters 2 and 4 rendered, 3 not: playing straight through stalls at 3, so
    // only one chapter is really available even though two are rendered.
    let s = store();
    for n in 1..=4 {
        chapter(&s, n);
    }
    audio(&s, 1);
    audio(&s, 2);
    audio(&s, 4);

    let r = listened::set(&s, 1, 3).unwrap();
    assert_eq!(r.buffer.playable_ahead, 1);
    assert_eq!(r.buffer.chapters_ahead, 2);
    assert!(r.buffer.has_gap());

    let out = listened::render_text(&r);
    assert!(out.contains("past an unrendered gap"), "{out}");
    assert!(out.contains("stall"), "{out}");
}

#[test]
fn no_gap_warning_when_the_run_is_continuous() {
    let s = rendered(4);
    let r = listened::set(&s, 1, 3).unwrap();
    assert!(!r.buffer.has_gap());
    assert!(!listened::render_text(&r).contains("gap"));
}

#[test]
fn a_chapter_with_text_but_no_audio_breaks_the_playable_run() {
    // §10: text ships even when rendering fails, so an unrendered chapter is a real
    // and expected state — not a hole in the numbering.
    let s = store();
    for n in 1..=3 {
        chapter(&s, n);
    }
    audio(&s, 1);
    audio(&s, 3);
    let r = listened::show(&s, 3).unwrap();
    assert_eq!(r.buffer.playable_ahead, 1);
    assert_eq!(r.buffer.chapters_ahead, 2);
}

#[test]
fn the_cursor_at_the_latest_chapter_leaves_nothing_ahead() {
    let s = rendered(3);
    let r = listened::set(&s, 3, 3).unwrap();
    assert_eq!(r.buffer.playable_ahead, 0);
    assert_eq!(r.buffer.chapters_ahead, 0);
    assert!(!r.buffer.buffer_ok);
    assert_eq!(r.buffer.shortfall(), 3);
}

// ------------------------------------------------------- status wiring

#[test]
fn status_reports_the_cursor_as_the_explicit_baseline() {
    let s = rendered(5);
    listened::set(&s, 2, 3).unwrap();
    let r = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    assert_eq!(r.consumed_through, 2);
    assert_eq!(r.playable_ahead, 3);
    assert_eq!(r.chapters_ahead, 3);
    assert!(r.buffer_ok);

    let out = status::render_text(&r);
    assert!(out.contains("listened through  2"), "{out}");
    assert!(
        out.contains("from chapter 3"),
        "the baseline must be explicit, not implied:\n{out}"
    );
}

#[test]
fn status_says_nothing_yet_when_the_cursor_is_zero() {
    let s = rendered(2);
    let out = status::render_text(&status::status(&s, 3, Path::new("/nonexistent")).unwrap());
    assert!(out.contains("nothing yet"), "{out}");
    assert!(out.contains("from chapter 1"), "{out}");
}

#[test]
fn status_and_listened_never_disagree_about_the_buffer() {
    // Both read one implementation, so this pins that they keep doing so.
    let s = store();
    for n in 1..=6 {
        chapter(&s, n);
    }
    for n in [1, 2, 3, 5] {
        audio(&s, n);
    }
    listened::set(&s, 1, 3).unwrap();

    let st = status::status(&s, 3, Path::new("/nonexistent")).unwrap();
    let li = listened::show(&s, 3).unwrap();
    assert_eq!(st.consumed_through, li.buffer.consumed_through);
    assert_eq!(st.playable_ahead, li.buffer.playable_ahead);
    assert_eq!(st.chapters_ahead, li.buffer.chapters_ahead);
    assert_eq!(st.buffer_ok, li.buffer.buffer_ok);
}

#[test]
fn status_flags_rendered_chapters_stranded_past_a_gap() {
    let s = store();
    for n in 1..=4 {
        chapter(&s, n);
    }
    audio(&s, 1);
    audio(&s, 4);
    let out = status::render_text(&status::status(&s, 3, Path::new("/nonexistent")).unwrap());
    assert!(out.contains("past an unrendered gap"), "{out}");
}

// -------------------------------------------------------------- json

#[test]
fn listened_serialises() {
    let s = rendered(4);
    let json = serde_json::to_string(&listened::set(&s, 2, 3).unwrap()).unwrap();
    assert!(json.contains("\"position\":2"), "{json}");
    assert!(json.contains("\"previous\":0"), "{json}");
    assert!(json.contains("\"moved_backwards\":false"), "{json}");
    assert!(
        json.contains("\"buffer_state\":\"below\"") || json.contains("buffer_state"),
        "{json}"
    );
    assert!(json.contains("\"playable_ahead\":2"), "{json}");
    assert!(json.contains("\"consumed_through\":2"), "{json}");
}

#[test]
fn status_json_carries_the_cursor_and_both_ahead_counts() {
    let s = rendered(4);
    listened::set(&s, 1, 3).unwrap();
    let json =
        serde_json::to_string(&status::status(&s, 3, Path::new("/nonexistent")).unwrap()).unwrap();
    assert!(json.contains("\"consumed_through\":1"), "{json}");
    assert!(json.contains("\"playable_ahead\":3"), "{json}");
    assert!(json.contains("\"chapters_ahead\":3"), "{json}");
    assert!(json.contains("\"buffer_target\":3"), "{json}");
    assert!(
        !json.contains("rendered_tail"),
        "the old proxy should be gone: {json}"
    );
}
