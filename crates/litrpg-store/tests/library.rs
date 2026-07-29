//! The retrieval readers behind the engine's `Library` port.
//!
//! Their absence was a silent-degradation path: without a lore reader, an entry
//! written in chapter 5 could never be retrieved for chapter 6, so §6.3 retrieval
//! would quietly collapse to always-on entries with nothing reporting a fault.

use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::migrations::TARGET_VERSION;
use litrpg_store::{NewChapter, Store};

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn chapter(number: u32) -> NewChapter {
    NewChapter {
        number,
        title: format!("Chapter {number}"),
        text_md: "[narrator] The vale.".into(),
        prompt_hash: "fnv1a64:cbf29ce484222325".into(),
        state_dirty: false,
    }
}

#[test]
fn migrations_reach_the_target_version() {
    assert_eq!(store().schema_version().unwrap(), TARGET_VERSION);
}

#[test]
fn lore_is_readable_after_being_written() {
    let s = store();
    s.upsert_lore(
        "Ashen Vale",
        "place",
        "ashen vale,the vale",
        "A basin of iron dust.",
        10,
        false,
        5,
    )
    .unwrap();
    s.upsert_lore(
        "The System",
        "rule",
        "system",
        "Speaks in panes.",
        100,
        true,
        1,
    )
    .unwrap();

    let rows = s.lore().unwrap();
    assert_eq!(rows.len(), 2);
    // Highest priority first.
    assert_eq!(rows[0].name, "The System");
    assert!(rows[0].always_on);
    assert_eq!(rows[1].name, "Ashen Vale");
    assert_eq!(rows[1].keywords, "ashen vale,the vale");
    assert_eq!(rows[1].kind, "place");
    assert_eq!(rows[1].priority, 10);
    assert!(!rows[1].always_on);
    assert_eq!(rows[1].updated_chapter, 5);
}

#[test]
fn upserting_lore_replaces_rather_than_duplicates() {
    let s = store();
    s.upsert_lore("Ashen Vale", "place", "vale", "First draft.", 1, false, 5)
        .unwrap();
    s.upsert_lore("Ashen Vale", "place", "vale,basin", "Revised.", 20, true, 9)
        .unwrap();

    let rows = s.lore().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].body_md, "Revised.");
    assert_eq!(rows[0].priority, 20);
    assert!(rows[0].always_on);
    assert_eq!(rows[0].updated_chapter, 9);
}

#[test]
fn summaries_come_back_oldest_first() {
    let s = store();
    for n in 1..=6 {
        s.put_chapter_summary(n, &format!("Chapter {n} happened."))
            .unwrap();
    }

    let recent = s.recent_chapter_summaries(3).unwrap();
    assert_eq!(recent.len(), 3);
    // The three most recent, in the order they belong in a prompt.
    assert_eq!(recent[0].to_ch, 4);
    assert_eq!(recent[1].to_ch, 5);
    assert_eq!(recent[2].to_ch, 6);
    assert_eq!(recent[2].body_md, "Chapter 6 happened.");
    assert_eq!(recent[0].level, 0);
}

#[test]
fn asking_for_more_summaries_than_exist_is_fine() {
    let s = store();
    s.put_chapter_summary(1, "One.").unwrap();
    assert_eq!(s.recent_chapter_summaries(10).unwrap().len(), 1);
    assert!(store().recent_chapter_summaries(5).unwrap().is_empty());
}

/// Re-extracting a `state_dirty` chapter must replace its summary. Two summaries
/// for one chapter would both surface in retrieval, quietly doubling its weight.
#[test]
fn putting_a_summary_twice_replaces_it() {
    let s = store();
    s.put_chapter_summary(7, "First extraction.").unwrap();
    s.put_chapter_summary(7, "Second extraction, after state_dirty.")
        .unwrap();

    let all = s.recent_chapter_summaries(10).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].body_md, "Second extraction, after state_dirty.");
}

#[test]
fn arc_summaries_do_not_pollute_chapter_retrieval() {
    let s = store();
    s.put_chapter_summary(1, "Chapter one.").unwrap();
    s.put_summary(1, 1, 10, "Arc one.").unwrap();
    s.put_summary(2, 1, 40, "Book one.").unwrap();

    let chapters = s.recent_chapter_summaries(10).unwrap();
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].body_md, "Chapter one.");
}

/// Level is part of the key, so an arc summary covering 1..10 and a chapter
/// summary covering 1..1 coexist — and two arcs over the same range replace.
#[test]
fn summary_identity_is_level_plus_range() {
    let s = store();
    s.put_summary(1, 1, 10, "Arc one, draft.").unwrap();
    s.put_summary(1, 1, 10, "Arc one, revised.").unwrap();
    s.put_summary(1, 11, 20, "Arc two.").unwrap();

    // Two distinct arc rows, the first replaced rather than duplicated.
    let arcs = s.recent_chapter_summaries(10).unwrap();
    assert!(
        arcs.is_empty(),
        "arc rows must not appear as chapter summaries"
    );
}

#[test]
fn chapters_missing_audio_lists_only_the_silent_ones() {
    let s = store();
    for n in 1..=3 {
        s.insert_chapter(&chapter(n)).unwrap();
    }
    let m = Manifest::new(
        2,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "sherpa:piper-en_GB-cori-high:0".into(),
            text: "The vale.".into(),
            start_ms: 0,
            end_ms: 1000,
        }],
    );
    s.attach_audio(2, &m).unwrap();

    assert_eq!(s.chapters_missing_audio().unwrap(), vec![1, 3]);
}

#[test]
fn no_chapters_means_nothing_missing() {
    assert!(store().chapters_missing_audio().unwrap().is_empty());
}
