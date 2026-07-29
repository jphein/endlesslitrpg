//! `attach_audio` guards the manifest invariant at the write boundary.
//!
//! Clients derive Range offsets from these segments, so a bad manifest doesn't
//! fail loudly — it plays audio from the wrong place, only for listeners, only
//! after the fact. Rejecting it on write is the last cheap place to catch it.

use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::{NewChapter, Store};

fn seg(idx: u32, start_ms: u32, end_ms: u32) -> Segment {
    Segment {
        idx,
        speaker: "narrator".into(),
        kind: SpeakerKind::Narrator,
        voice_ref: "sherpa:piper-en_GB-cori-high:0".into(),
        text: "The vale smelled of iron and wet ash.".into(),
        start_ms,
        end_ms,
    }
}

fn store_with_chapter() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .insert_chapter(&NewChapter {
            number: 1,
            title: "Chapter 1".into(),
            text_md: "[narrator] The vale smelled of iron and wet ash.".into(),
            prompt_hash: "abc123".into(),
            state_dirty: false,
        })
        .unwrap();
    store
}

#[test]
fn accepts_a_contiguous_manifest() {
    let store = store_with_chapter();
    let m = Manifest::new(1, vec![seg(0, 0, 4120), seg(1, 4120, 9000)]);
    assert!(store.attach_audio(1, &m, "0001.pcm", "0001.mp3").is_ok());
    assert_eq!(store.chapter(1).unwrap().duration_ms, 9000);
}

#[test]
fn rejects_a_gap_between_segments() {
    let store = store_with_chapter();
    // 100..150 is a hole: every later byte offset would address the wrong audio.
    let m = Manifest::new(1, vec![seg(0, 0, 100), seg(1, 150, 200)]);
    let err = store.attach_audio(1, &m, "0001.pcm", "0001.mp3").unwrap_err();
    assert!(err.to_string().contains("not contiguous"), "{err}");

    // And nothing was written: the chapter still has no audio.
    let ch = store.chapter(1).unwrap();
    assert!(!ch.has_audio);
    assert_eq!(store.segments(1).unwrap().len(), 0);
}

#[test]
fn rejects_segments_that_do_not_start_at_zero() {
    let store = store_with_chapter();
    let m = Manifest::new(1, vec![seg(0, 40, 100)]);
    let err = store.attach_audio(1, &m, "0001.pcm", "0001.mp3").unwrap_err();
    assert!(err.to_string().contains("not contiguous"), "{err}");
}

#[test]
fn rejects_duration_disagreeing_with_the_last_segment() {
    let store = store_with_chapter();
    let mut m = Manifest::new(1, vec![seg(0, 0, 4120)]);
    // Simulate a caller that computed duration from predicted rather than final PCM.
    m.duration_ms = 4096;
    let err = store.attach_audio(1, &m, "0001.pcm", "0001.mp3").unwrap_err();
    assert!(err.to_string().contains("disagrees"), "{err}");
}

#[test]
fn an_empty_manifest_is_acceptable() {
    // A chapter whose render produced nothing is a real state (§10: text ships
    // even when TTS fails); it must not be conflated with a corrupt manifest.
    let store = store_with_chapter();
    let m = Manifest::new(1, vec![]);
    assert!(store.attach_audio(1, &m, "0001.pcm", "0001.mp3").is_ok());
    assert_eq!(store.chapter(1).unwrap().duration_ms, 0);
}

#[test]
fn created_at_is_exposed_for_rss_pubdate() {
    let store = store_with_chapter();
    assert!(store.chapter(1).unwrap().created_at > 0);
}
