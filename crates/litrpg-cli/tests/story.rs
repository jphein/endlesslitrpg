use litrpg_cli::story::{self, StoryEdit};
use litrpg_cli::{CliError, naming};
use litrpg_core::hash::content_hash;
use litrpg_store::{NewStory, Store};
use tempfile::TempDir;

fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-story-")
        .tempdir()
        .unwrap()
}

/// A story whose prompt file exists in `dir` and names `prompt_says`.
fn seeded(dir: &std::path::Path, protagonist: &str, prompt_says: &str) -> Store {
    let body = format!("# The Ashen Vale\n\n{prompt_says} returns to the vale.\n");
    std::fs::write(dir.join("prompt.md"), &body).unwrap();
    let s = Store::open_in_memory().unwrap();
    s.upsert_story(&NewStory {
        title: "The Ashen Vale".into(),
        protagonist: protagonist.into(),
        prompt_path: "prompt.md".into(),
        prompt_hash: content_hash(&body),
        target_words: 2000,
    })
    .unwrap();
    s
}

fn edit_protagonist(name: &str) -> StoryEdit {
    StoryEdit {
        protagonist: Some(name.to_string()),
        ..Default::default()
    }
}

// ------------------------------------------------------------- reading

#[test]
fn with_no_flags_it_prints_the_row_and_changes_nothing() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    let r = story::story(&s, &StoryEdit::default(), dir.path()).unwrap();
    assert!(!r.changed());
    assert_eq!(r.protagonist, "Kaelen");
    assert_eq!(r.title, "The Ashen Vale");
    assert_eq!(r.target_words, 2000);

    let out = story::render_text(&r);
    assert!(out.contains("Kaelen"), "{out}");
    assert!(out.contains("The Ashen Vale"), "{out}");
    assert!(!out.contains("->"), "nothing changed:\n{out}");
}

#[test]
fn it_shows_the_playback_cursor_and_prompt_provenance() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    s.set_consumed_through(0).unwrap();
    let out = story::render_text(&story::story(&s, &StoryEdit::default(), dir.path()).unwrap());
    assert!(out.contains("nothing yet"), "{out}");
    assert!(out.contains("prompt.md"), "{out}");
    assert!(out.contains("fnv1a64:"), "{out}");
}

#[test]
fn a_story_row_is_required() {
    let dir = tmp();
    let s = Store::open_in_memory().unwrap();
    let err = story::story(&s, &StoryEdit::default(), dir.path()).unwrap_err();
    assert!(
        matches!(err, CliError::Store(litrpg_store::StoreError::NoStoryRow)),
        "got {err:?}"
    );
}

// ------------------------------------------------------------- writing

#[test]
fn setting_the_protagonist_records_the_change_without_touching_the_prompt() {
    // The whole point of #13: fixing a name must not go through --force, which rewrites
    // prompt.md to the starter template and loses the premise.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");
    let before = std::fs::read_to_string(dir.path().join("prompt.md")).unwrap();

    let r = story::story(&s, &edit_protagonist("Kaelen Vord"), dir.path()).unwrap();
    assert!(r.changed());
    assert_eq!(r.protagonist, "Kaelen Vord");
    assert_eq!(s.story().unwrap().unwrap().protagonist, "Kaelen Vord");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("prompt.md")).unwrap(),
        before,
        "the premise must be untouched"
    );

    let out = story::render_text(&r);
    assert!(out.contains("\"Kaelen\" -> \"Kaelen Vord\""), "{out}");
}

#[test]
fn setting_the_title_works_and_leaves_the_protagonist_alone() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    let r = story::story(
        &s,
        &StoryEdit {
            title: Some("A New Name".into()),
            ..Default::default()
        },
        dir.path(),
    )
    .unwrap();
    assert_eq!(r.title, "A New Name");
    assert_eq!(r.protagonist, "Kaelen");
    assert_eq!(r.changes.len(), 1);
}

#[test]
fn both_fields_can_change_at_once() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");
    let r = story::story(
        &s,
        &StoryEdit {
            protagonist: Some("Kaelen Vord".into()),
            title: Some("The Ashen Ledger".into()),
        },
        dir.path(),
    )
    .unwrap();
    assert_eq!(r.changes.len(), 2);
    let row = s.story().unwrap().unwrap();
    assert_eq!(row.protagonist, "Kaelen Vord");
    assert_eq!(row.title, "The Ashen Ledger");
}

#[test]
fn setting_a_value_that_already_matches_is_not_reported_as_a_change() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    let r = story::story(&s, &edit_protagonist("Kaelen"), dir.path()).unwrap();
    assert!(!r.changed(), "{:?}", r.changes);
}

#[test]
fn surrounding_whitespace_is_trimmed() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");
    story::story(&s, &edit_protagonist("  Kaelen Vord  "), dir.path()).unwrap();
    assert_eq!(s.story().unwrap().unwrap().protagonist, "Kaelen Vord");
}

#[test]
fn a_blank_protagonist_is_refused_because_it_seeds_the_known_subjects() {
    // Clearing it would make every protagonist stat change fail as UnknownSubject — a
    // large silent consequence for an argument that looks like a no-op.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    for blank in ["", "   "] {
        let err = story::story(&s, &edit_protagonist(blank), dir.path()).unwrap_err();
        assert!(
            matches!(
                err,
                CliError::BlankStoryField {
                    field: "protagonist",
                    ..
                }
            ),
            "got {err:?}"
        );
        assert!(err.to_string().contains("UnknownSubject"), "{err}");
    }
    assert_eq!(
        s.story().unwrap().unwrap().protagonist,
        "Kaelen",
        "must not have been written"
    );
}

#[test]
fn a_blank_title_is_refused_too() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    let err = story::story(
        &s,
        &StoryEdit {
            title: Some("  ".into()),
            ..Default::default()
        },
        dir.path(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        CliError::BlankStoryField { field: "title", .. }
    ));
}

// --------------------------------------- the check it can create

#[test]
fn changing_the_protagonist_rechecks_it_against_the_prompt() {
    // A command that can *create* the mismatch should say so immediately rather than
    // leaving it for the next `status`.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");

    // Rename to something the prompt does not mention.
    let r = story::story(&s, &edit_protagonist("Aster"), dir.path()).unwrap();
    assert!(
        r.protagonist_check.is_warning(),
        "{:?}",
        r.protagonist_check
    );
    let out = story::render_text(&r);
    assert!(out.contains("does not name the protagonist"), "{out}");
}

#[test]
fn fixing_the_mismatch_clears_the_warning() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");
    // Before: recorded "Kaelen" while the prompt says "Kaelen Vord".
    let before = story::story(&s, &StoryEdit::default(), dir.path()).unwrap();
    assert!(before.protagonist_check.is_warning());

    let after = story::story(&s, &edit_protagonist("Kaelen Vord"), dir.path()).unwrap();
    assert!(!after.protagonist_check.is_warning());
    assert_eq!(after.protagonist_check, naming::ProtagonistCheck::Named);
}

#[test]
fn a_protagonist_change_says_the_ledger_keeps_the_old_name() {
    // Append-only: this changes what future deltas are accepted under, not history.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");
    let out = story::render_text(
        &story::story(&s, &edit_protagonist("Kaelen Vord"), dir.path()).unwrap(),
    );
    assert!(out.contains("append-only"), "{out}");
    assert!(out.contains("future"), "{out}");
    assert!(
        out.contains("litrpg state"),
        "must point at the split view:\n{out}"
    );
}

#[test]
fn a_title_only_change_does_not_mention_the_ledger() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    let out = story::render_text(
        &story::story(
            &s,
            &StoryEdit {
                title: Some("Another".into()),
                ..Default::default()
            },
            dir.path(),
        )
        .unwrap(),
    );
    assert!(!out.contains("append-only"), "{out}");
}

// ------------------------------------------------ concurrent-write safety

#[test]
fn a_concurrent_engine_write_is_not_reverted() {
    // The reason for single-field setters over upsert_story: read-modify-write would
    // reinstate a stale prompt_hash over one the engine wrote at a chapter boundary.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");

    // Engine advances the in-effect prompt hash and the cursor.
    s.set_prompt_hash("fnv1a64:aaaaaaaaaaaaaaaa").unwrap();
    s.set_consumed_through(2).unwrap();
    s.set_arc_outline("## Act II").unwrap();

    story::story(&s, &edit_protagonist("Kaelen Vord"), dir.path()).unwrap();

    let row = s.story().unwrap().unwrap();
    assert_eq!(row.protagonist, "Kaelen Vord");
    assert_eq!(
        row.prompt_hash, "fnv1a64:aaaaaaaaaaaaaaaa",
        "engine write reverted"
    );
    assert_eq!(row.consumed_through, 2, "cursor reverted");
    assert_eq!(row.arc_outline_md, "## Act II", "arc outline reverted");
}

#[test]
fn the_report_serialises() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen Vord");
    let json = serde_json::to_string(
        &story::story(&s, &edit_protagonist("Kaelen Vord"), dir.path()).unwrap(),
    )
    .unwrap();
    assert!(json.contains("\"protagonist\":\"Kaelen Vord\""), "{json}");
    assert!(json.contains("changes"), "{json}");
    assert!(json.contains("protagonist_check"), "{json}");
    assert!(json.contains("consumed_through"), "{json}");
}

// ------------------- the chapter-1 signal, and its own fix silencing it

#[test]
fn a_protagonist_who_is_not_in_the_cast_is_flagged() {
    // The live shape, at chapter 1: nothing has split yet, and this is already true.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen Vord", "Kaelen Vord");
    s.upsert_cast("Kaelen", "sherpa:a:0", "character", 1)
        .unwrap();

    let r = story::story(&s, &StoryEdit::default(), dir.path()).unwrap();
    assert!(r.protagonist_cast.is_warning(), "{:?}", r.protagonist_cast);
    let out = story::render_text(&r);
    assert!(out.contains("not in the cast"), "{out}");
    assert!(out.contains("splits in two"), "{out}");
}

#[test]
fn the_alias_that_fixes_it_also_silences_it() {
    // The refinement that matters: JP chose the alias mapping as the fix, so a warning that
    // survived it would be one people learn to ignore.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen Vord", "Kaelen Vord");
    s.upsert_cast("Kaelen", "sherpa:a:0", "character", 1)
        .unwrap();
    assert!(
        story::story(&s, &StoryEdit::default(), dir.path())
            .unwrap()
            .protagonist_cast
            .is_warning()
    );

    s.add_alias("Kaelen", "Kaelen Vord").unwrap();

    let r = story::story(&s, &StoryEdit::default(), dir.path()).unwrap();
    assert!(
        !r.protagonist_cast.is_warning(),
        "the alias is the fix, so this must go quiet: {:?}",
        r.protagonist_cast
    );
    assert!(!story::render_text(&r).contains("not in the cast"));
}

#[test]
fn renaming_the_protagonist_to_the_cast_name_also_silences_it() {
    // The other legitimate fix: make the two the same name.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen Vord", "Kaelen");
    s.upsert_cast("Kaelen", "sherpa:a:0", "character", 1)
        .unwrap();
    assert!(
        story::story(&s, &StoryEdit::default(), dir.path())
            .unwrap()
            .protagonist_cast
            .is_warning()
    );

    let r = story::story(&s, &edit_protagonist("Kaelen"), dir.path()).unwrap();
    assert!(!r.protagonist_cast.is_warning(), "{:?}", r.protagonist_cast);
}

#[test]
fn changing_the_protagonist_away_from_the_cast_reports_it_at_once() {
    // A command that can *create* the mismatch should say so rather than leaving it for the
    // next `status`.
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    s.upsert_cast("Kaelen", "sherpa:a:0", "character", 1)
        .unwrap();
    assert!(
        !story::story(&s, &StoryEdit::default(), dir.path())
            .unwrap()
            .protagonist_cast
            .is_warning()
    );

    let r = story::story(&s, &edit_protagonist("Aster"), dir.path()).unwrap();
    assert!(r.protagonist_cast.is_warning(), "{:?}", r.protagonist_cast);
    assert!(story::render_text(&r).contains("not in the cast"));
}

#[test]
fn a_fresh_story_with_no_cast_is_not_nagged() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen", "Kaelen");
    let r = story::story(&s, &StoryEdit::default(), dir.path()).unwrap();
    assert!(!r.protagonist_cast.is_warning(), "{:?}", r.protagonist_cast);
}

#[test]
fn the_cast_check_is_in_the_json() {
    let dir = tmp();
    let s = seeded(dir.path(), "Kaelen Vord", "Kaelen Vord");
    s.upsert_cast("Kaelen", "sherpa:a:0", "character", 1)
        .unwrap();
    let json = serde_json::to_string(&story::story(&s, &StoryEdit::default(), dir.path()).unwrap())
        .unwrap();
    assert!(json.contains("protagonist_cast"), "{json}");
    assert!(json.contains("\"result\":\"missing\""), "{json}");
}
