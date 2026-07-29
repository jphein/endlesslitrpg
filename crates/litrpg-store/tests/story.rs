//! The singleton story row, and the one write path that must not destroy state
//! belonging to another subsystem.

use litrpg_store::{NewStory, Store};

fn new_story(title: &str, protagonist: &str) -> NewStory {
    NewStory {
        title: title.into(),
        protagonist: protagonist.into(),
        prompt_path: "/home/jp/.local/share/endlesslitrpg/story/prompt.md".into(),
        prompt_hash: "fnv1a64:cbf29ce484222325".into(),
        target_words: 2000,
    }
}

#[test]
fn absent_until_created() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.story().unwrap().is_none());
}

#[test]
fn insert_if_absent_reports_whether_it_inserted() {
    let store = Store::open_in_memory().unwrap();
    assert!(
        store
            .insert_story_if_absent(&new_story("Endless", "Kaelen"))
            .unwrap()
    );

    // Second call is a no-op and says so, rather than clobbering — this is what
    // makes `litrpg init` safe to re-run.
    assert!(
        !store
            .insert_story_if_absent(&new_story("Overwritten", "Vessa"))
            .unwrap()
    );

    let s = store.story().unwrap().unwrap();
    assert_eq!(s.title, "Endless");
    assert_eq!(s.protagonist, "Kaelen");
    assert!(s.updated_at > 0);
}

#[test]
fn upsert_inserts_when_absent() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_story(&new_story("Endless", "Kaelen")).unwrap();
    assert_eq!(store.story().unwrap().unwrap().title, "Endless");
}

#[test]
fn upsert_overwrites_caller_owned_fields() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_story(&new_story("Endless", "Kaelen")).unwrap();

    let mut changed = new_story("The Ashen Ledger", "Kaelen Vord");
    changed.target_words = 2500;
    store.upsert_story(&changed).unwrap();

    let s = store.story().unwrap().unwrap();
    assert_eq!(s.title, "The Ashen Ledger");
    assert_eq!(s.protagonist, "Kaelen Vord");
    assert_eq!(s.target_words, 2500);
}

/// The regression this whole API shape exists to prevent.
///
/// `arc_outline_md` is engine-owned narrative state. `litrpg init --force` run at
/// chapter 60 to fix a title must not erase the story's arc — a write path silently
/// destroying another subsystem's state is the same bug shape as the self-approving
/// rejected subject and the rewind-inflated rejection count.
#[test]
fn upsert_preserves_the_engine_owned_arc_outline() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_story(&new_story("Endless", "Kaelen")).unwrap();

    let outline = "## Arc 3\nKaelen breaks the second seal and loses the Vord name.";
    store.set_arc_outline(outline).unwrap();
    assert_eq!(store.story().unwrap().unwrap().arc_outline_md, outline);

    // Simulate `litrpg init --force` at chapter 60, repointing the path and title.
    let mut refreshed = new_story("The Ashen Ledger", "Kaelen");
    refreshed.prompt_path = "/srv/story/prompt.md".into();
    store.upsert_story(&refreshed).unwrap();

    let s = store.story().unwrap().unwrap();
    assert_eq!(s.title, "The Ashen Ledger");
    assert_eq!(s.prompt_path, "/srv/story/prompt.md");
    assert_eq!(
        s.arc_outline_md, outline,
        "upsert_story erased the arc outline — that is the bug this test exists for"
    );
}

#[test]
fn insert_if_absent_also_leaves_the_outline_alone() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_story(&new_story("Endless", "Kaelen")).unwrap();
    let outline = "## Arc 1\nThe vale.";
    store.set_arc_outline(outline).unwrap();

    assert!(
        !store
            .insert_story_if_absent(&new_story("Nope", "Nobody"))
            .unwrap()
    );
    assert_eq!(store.story().unwrap().unwrap().arc_outline_md, outline);
}

/// A zero-row write is how narrative state goes missing without a trace, so this
/// errors rather than quietly succeeding.
#[test]
fn setting_the_outline_without_a_story_row_is_an_error() {
    let store = Store::open_in_memory().unwrap();
    let err = store.set_arc_outline("## Arc 1").unwrap_err();
    assert!(err.to_string().contains("no story row"), "{err}");
}

#[test]
fn the_story_table_stays_a_singleton() {
    let store = Store::open_in_memory().unwrap();
    store.insert_story_if_absent(&new_story("A", "X")).unwrap();
    store.insert_story_if_absent(&new_story("B", "Y")).unwrap();
    store.upsert_story(&new_story("C", "Z")).unwrap();

    // `story()` takes the first row; assert there is only ever one to take.
    let s = store.story().unwrap().unwrap();
    assert_eq!(s.title, "C");
    assert_eq!(s.protagonist, "Z");
}

#[test]
fn the_playback_cursor_starts_at_zero_and_records_progress() {
    let store = Store::open_in_memory().unwrap();
    // No story row yet: "nothing listened to" is the honest answer, not an error,
    // because the engine reads this every cycle.
    assert_eq!(store.consumed_through().unwrap(), 0);

    store.upsert_story(&new_story("Endless", "Kaelen")).unwrap();
    assert_eq!(store.consumed_through().unwrap(), 0);

    store.set_consumed_through(7).unwrap();
    assert_eq!(store.consumed_through().unwrap(), 7);
    assert_eq!(store.story().unwrap().unwrap().consumed_through, 7);

    // Going backwards is allowed — re-listening is legitimate, and a cursor that
    // only ratchets forward is a high-water mark, not a position.
    store.set_consumed_through(3).unwrap();
    assert_eq!(store.consumed_through().unwrap(), 3);
}

#[test]
fn recording_progress_without_a_story_is_an_error() {
    let store = Store::open_in_memory().unwrap();
    let err = store.set_consumed_through(1).unwrap_err();
    assert!(err.to_string().contains("no story row"), "{err}");
}

/// The explicit column list in `upsert_story` protects this for free — the same
/// decision that saved `arc_outline_md`. `litrpg init --force` to fix a title must
/// not silently reset how far someone has listened.
#[test]
fn upsert_preserves_the_playback_cursor() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_story(&new_story("Endless", "Kaelen")).unwrap();
    store.set_consumed_through(12).unwrap();

    store.upsert_story(&new_story("The Ashen Ledger", "Kaelen Vord")).unwrap();

    let s = store.story().unwrap().unwrap();
    assert_eq!(s.title, "The Ashen Ledger");
    assert_eq!(
        s.consumed_through, 12,
        "upsert_story reset the playback cursor — init --force must never do that"
    );
}
