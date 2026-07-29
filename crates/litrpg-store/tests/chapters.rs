use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::{NewChapter, Store};

fn manifest(chapter: u32) -> Manifest {
    Manifest::new(
        chapter,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "sherpa:piper-en_GB-cori:0".into(),
            text: "The vale smelled of iron and wet ash.".into(),
            start_ms: 0,
            end_ms: 4120,
        }],
    )
}

fn new_chapter(number: u32) -> NewChapter {
    NewChapter {
        number,
        title: format!("Chapter {number}"),
        text_md: "[narrator] The vale smelled of iron and wet ash.".into(),
        prompt_hash: "abc123".into(),
        state_dirty: false,
    }
}

#[test]
fn inserts_and_reads_back_a_chapter() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();

    let ch = store.chapter(1).unwrap();
    assert_eq!(ch.number, 1);
    assert_eq!(ch.title, "Chapter 1");
    assert!(!ch.has_audio);
    assert_eq!(ch.duration_ms, 0);
}

#[test]
fn missing_chapter_is_an_error_not_a_panic() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.chapter(99).is_err());
}

#[test]
fn attaching_audio_persists_manifest_segments_and_duration() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.attach_audio(1, &manifest(1)).unwrap();

    let ch = store.chapter(1).unwrap();
    assert!(ch.has_audio);
    assert_eq!(ch.duration_ms, 4120);

    let segments = store.segments(1).unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].voice_ref, "sherpa:piper-en_GB-cori:0");
    assert_eq!(segments[0].start_byte(), 0);
    assert_eq!(segments[0].end_byte(), 131_840);
}

#[test]
fn attaching_audio_twice_replaces_rather_than_duplicates_segments() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.attach_audio(1, &manifest(1)).unwrap();
    store.attach_audio(1, &manifest(1)).unwrap();
    assert_eq!(store.segments(1).unwrap().len(), 1);
}

/// The inverse of `attach_audio`, and the thing whose absence made a "non-destructive"
/// voice substitution permanent: the cast rows were preserved, but nothing could ask for
/// a re-render, so nothing ever used them.
#[test]
fn clearing_audio_queues_the_chapter_for_the_resume_path() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.attach_audio(1, &manifest(1)).unwrap();
    assert!(store.chapters_missing_audio().unwrap().is_empty());

    assert!(
        store.clear_audio(1).unwrap(),
        "the flag should have changed"
    );
    assert_eq!(store.chapters_missing_audio().unwrap(), vec![1]);

    // Idempotent, and says which it was — a caller re-running `litrpg render 1` should be
    // told it was already queued rather than shown a second identical success.
    assert!(!store.clear_audio(1).unwrap());
}

/// Only the flag. The manifest, duration and segments survive, because the rendered file
/// is still on disk and playable — if the re-render never happens, wiping them would have
/// destroyed the only record of working audio. Undoing a flag is one `attach_audio`;
/// undoing a deleted manifest is nothing.
#[test]
fn clearing_audio_preserves_the_manifest_duration_and_segments() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.attach_audio(1, &manifest(1)).unwrap();

    store.clear_audio(1).unwrap();

    let ch = store.chapter(1).unwrap();
    assert!(!ch.has_audio);
    assert_eq!(
        ch.duration_ms, 4120,
        "duration was wiped; the audio on disk is still that long"
    );
    assert_eq!(store.segments(1).unwrap().len(), 1);
}

/// Absent must not read as "already queued". Collapsing them would make `litrpg render 99`
/// on a one-chapter story print a reassuring no-op.
#[test]
fn clearing_audio_on_a_missing_chapter_is_an_error() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    let err = store.clear_audio(99).unwrap_err();
    assert!(err.to_string().contains("chapter 99 not found"), "{err}");
}

/// A chapter whose text shipped but whose render failed is already queued. Clearing it
/// reports `false` rather than erroring: nothing is wrong, there is just nothing to do.
#[test]
fn clearing_audio_that_was_never_attached_reports_no_change() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    assert!(!store.clear_audio(1).unwrap());
}

#[test]
fn chapters_since_returns_ascending_numbers_only_after_the_cursor() {
    let store = Store::open_in_memory().unwrap();
    for n in 1..=4 {
        store.insert_chapter(&new_chapter(n)).unwrap();
    }
    let numbers: Vec<u32> = store
        .chapters_since(2)
        .unwrap()
        .iter()
        .map(|c| c.number)
        .collect();
    assert_eq!(numbers, vec![3, 4]);
}

#[test]
fn latest_number_tracks_the_highest_chapter() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.latest_number().unwrap(), 0);
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.insert_chapter(&new_chapter(2)).unwrap();
    assert_eq!(store.latest_number().unwrap(), 2);
}

#[test]
fn state_dirty_chapters_can_be_listed_for_re_extraction() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    let mut dirty = new_chapter(2);
    dirty.state_dirty = true;
    store.insert_chapter(&dirty).unwrap();

    assert_eq!(store.dirty_chapters().unwrap(), vec![2]);
}

#[test]
fn on_disk_store_round_trips() {
    let dir = std::env::temp_dir().join(format!("litrpg-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("story.db");

    {
        let store = Store::open(&path).unwrap();
        store.insert_chapter(&new_chapter(7)).unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.chapter(7).unwrap().title, "Chapter 7");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
