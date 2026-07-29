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
