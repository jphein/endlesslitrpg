use std::path::{Path, PathBuf};

use litrpg_cli::init::{self, Action, InitOptions};
use litrpg_cli::prompt;
use litrpg_config::Config;
use litrpg_store::Store;
use tempfile::TempDir;

/// Randomly-named temp dir, removed on drop. `init` is filesystem-mutating and
/// idempotency is the property under test, so a leftover predictable directory
/// would make "already exists" tests pass for the wrong reason.
fn tmp() -> TempDir {
    tempfile::Builder::new()
        .prefix("litrpg-init-")
        .tempdir()
        .unwrap()
}

/// A config file pointing everything inside `root`, so nothing touches the real
/// `~/.local/share`.
fn write_config(root: &Path) -> PathBuf {
    let path = root.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "db_path = \"{}/db/story.db\"\n\
             media_dir = \"{}/media\"\n\
             story_dir = \"{}/story\"\n\
             target_words = 1500\n",
            root.display(),
            root.display(),
            root.display()
        ),
    )
    .unwrap();
    path
}

fn opts() -> InitOptions {
    InitOptions::default()
}

fn forced() -> InitOptions {
    InitOptions {
        force: true,
        ..Default::default()
    }
}

// ------------------------------------------------------------- first run

#[test]
fn a_first_run_creates_everything() {
    let dir = tmp();
    let cfg = write_config(dir.path());

    let (config, r) = init::init(Some(&cfg), &opts()).unwrap();

    // Config already existed (we wrote it), so it is reported as such.
    assert_eq!(r.config, Action::Existed);
    assert_eq!(r.prompt, Action::Created);
    assert_eq!(r.story, Action::Created);

    assert!(config.db_path.exists(), "database file should exist");
    assert!(config.media_dir.is_dir());
    assert!(config.story_dir.is_dir());
    assert!(r.prompt_path.exists());
    // Against TARGET_VERSION, not a literal: a future migration should not break
    // an init test that is really asserting "migrations ran to completion".
    assert_eq!(r.schema_version, litrpg_store::migrations::TARGET_VERSION);
    assert_eq!(
        r.target_words, 1500,
        "must come from the config, not a default"
    );
    assert_eq!(r.title, init::DEFAULT_TITLE);
    assert_eq!(r.protagonist, "");
    assert!(r.prompt_is_placeholder);
}

#[test]
fn a_starter_config_is_written_when_absent_and_is_loadable() {
    // Exercised through `ensure_config` rather than the whole of `init`: the starter
    // config carries DEFAULT paths, so running full init here would create a real
    // database under ~/.local/share. Ask me how I know.
    let dir = tmp();
    let cfg = dir.path().join("nested/config.toml");

    assert_eq!(
        init::ensure_config(Some(&cfg), false).unwrap(),
        Action::Created
    );
    assert!(cfg.exists(), "parent directories must be created too");
    // The file it writes must be one it can read back.
    Config::load_from(&cfg).unwrap();
}

#[test]
fn the_database_is_migrated_and_usable() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (config, _) = init::init(Some(&cfg), &opts()).unwrap();

    // Reopen independently: a real, migrated database, not just a created file.
    let store = Store::open(&config.db_path).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        litrpg_store::migrations::TARGET_VERSION
    );
    assert!(store.table_names().unwrap().contains(&"story".to_string()));
    assert_eq!(store.latest_number().unwrap(), 0);
}

#[test]
fn the_story_row_is_created_so_the_watch_has_a_subject() {
    // /api/character with no subject resolves through story.protagonist, so a
    // missing row means the watch's character screen has nothing to render.
    let dir = tmp();
    let cfg = write_config(dir.path());
    let o = InitOptions {
        title: Some("The Ashen Vale".into()),
        protagonist: Some("Kaelen".into()),
        ..Default::default()
    };
    let (config, r) = init::init(Some(&cfg), &o).unwrap();
    assert_eq!(r.story, Action::Created);
    assert_eq!(r.title, "The Ashen Vale");
    assert_eq!(r.protagonist, "Kaelen");

    let store = Store::open(&config.db_path).unwrap();
    let row = store.story().unwrap().expect("story row must exist");
    assert_eq!(row.title, "The Ashen Vale");
    assert_eq!(row.protagonist, "Kaelen");
    assert_eq!(row.target_words, 1500);
    assert_eq!(row.prompt_hash, r.prompt_hash);
    assert!(
        row.prompt_path.ends_with("prompt.md"),
        "got {:?}",
        row.prompt_path
    );
}

#[test]
fn the_story_rows_hash_matches_the_prompt_actually_on_disk() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (config, r) = init::init(Some(&cfg), &opts()).unwrap();

    let body = std::fs::read_to_string(&r.prompt_path).unwrap();
    assert_eq!(r.prompt_hash, prompt::content_hash(&body));

    let store = Store::open(&config.db_path).unwrap();
    assert_eq!(store.story().unwrap().unwrap().prompt_hash, r.prompt_hash);
}

#[test]
fn directories_created_are_reported() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    assert_eq!(r.dirs_created.len(), 3, "{:?}", r.dirs_created);
    assert!(r.dirs_created.iter().any(|d| d.ends_with("media")));
    assert!(r.dirs_created.iter().any(|d| d.ends_with("story")));
    assert!(r.dirs_created.iter().any(|d| d.ends_with("db")));
}

// ------------------------------------------------------------ idempotency

#[test]
fn a_second_run_changes_nothing() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(Some(&cfg), &opts()).unwrap();

    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    assert_eq!(r.config, Action::Existed);
    assert_eq!(r.prompt, Action::Existed);
    assert_eq!(r.story, Action::Existed);
    assert!(r.dirs_created.is_empty(), "{:?}", r.dirs_created);
    assert!(!r.config.changed() && !r.prompt.changed() && !r.story.changed());
}

#[test]
fn re_running_does_not_touch_an_edited_prompt() {
    // The command people re-run "just to be sure" must not be the command that
    // eats an hour of writing.
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (_, first) = init::init(Some(&cfg), &opts()).unwrap();
    std::fs::write(
        &first.prompt_path,
        "# My actual premise\n\nA thief in the vale.\n",
    )
    .unwrap();

    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    assert_eq!(r.prompt, Action::Existed);
    assert_eq!(
        std::fs::read_to_string(&r.prompt_path).unwrap(),
        "# My actual premise\n\nA thief in the vale.\n"
    );
    assert!(!r.prompt_is_placeholder);
    assert_ne!(r.prompt_hash, first.prompt_hash);
}

#[test]
fn re_running_does_not_touch_an_edited_config() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(Some(&cfg), &opts()).unwrap();
    // Operator tunes a value, keeping the paths.
    let edited = format!(
        "{}buffer_target = 6\n",
        std::fs::read_to_string(&cfg).unwrap()
    );
    std::fs::write(&cfg, &edited).unwrap();

    let (config, r) = init::init(Some(&cfg), &opts()).unwrap();
    assert_eq!(r.config, Action::Existed);
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), edited);
    assert_eq!(
        config.buffer_target, 6,
        "the edited value must be the one used"
    );
}

#[test]
fn re_running_preserves_an_existing_story_row() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let o = InitOptions {
        title: Some("The Ashen Vale".into()),
        protagonist: Some("Kaelen".into()),
        ..Default::default()
    };
    init::init(Some(&cfg), &o).unwrap();

    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    assert_eq!(r.story, Action::Existed);
    assert_eq!(r.title, "The Ashen Vale");
    assert_eq!(r.protagonist, "Kaelen");
}

#[test]
fn re_running_does_not_disturb_existing_chapters_or_ledger() {
    // init on a story already 3 chapters deep must be a no-op on real content.
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (config, _) = init::init(Some(&cfg), &opts()).unwrap();

    {
        let store = Store::open(&config.db_path).unwrap();
        store
            .upsert_cast("Kaelen", "sherpa:piper-en_GB-cori-high:0", "character", 1)
            .unwrap();
        store
            .insert_chapter(&litrpg_store::NewChapter {
                number: 1,
                title: "Chapter 1".into(),
                text_md: "text".into(),
                prompt_hash: "fnv1a64:0000000000000000".into(),
                state_dirty: false,
            })
            .unwrap();
        store
            .append_delta(
                1,
                &litrpg_core::validate::Delta {
                    subject: "Kaelen".into(),
                    field: "hp".into(),
                    op: litrpg_core::ledger::Op::Set,
                    value_num: Some(100),
                    value_txt: None,
                },
            )
            .unwrap()
            .unwrap();
    }

    init::init(Some(&cfg), &opts()).unwrap();

    let store = Store::open(&config.db_path).unwrap();
    assert_eq!(store.latest_number().unwrap(), 1);
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
    assert_eq!(store.applied_count().unwrap(), 1);
}

// ----------------------------------------------------------------- force

#[test]
fn force_rewrites_the_prompt() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (_, first) = init::init(Some(&cfg), &opts()).unwrap();
    std::fs::write(&first.prompt_path, "my own premise\n").unwrap();

    let (_, r) = init::init(Some(&cfg), &forced()).unwrap();
    assert_eq!(r.prompt, Action::Overwritten);
    assert_eq!(
        std::fs::read_to_string(&r.prompt_path).unwrap(),
        prompt::STARTER_PROMPT
    );
    assert!(r.prompt_is_placeholder);
}

#[test]
fn force_does_not_repoint_a_working_installation() {
    // The starter config carries DEFAULT paths, so overwriting a valid config.toml
    // does not reset a file — it repoints the whole installation at
    // ~/.local/share/endlesslitrpg and orphans the operator's real database.
    // `init --force` to reset a placeholder prompt would make the story look gone.
    let dir = tmp();
    let cfg = write_config(dir.path());
    let original = std::fs::read_to_string(&cfg).unwrap();
    let (first, _) = init::init(Some(&cfg), &opts()).unwrap();

    let (config, r) = init::init(Some(&cfg), &forced()).unwrap();

    assert_eq!(
        r.config,
        Action::Existed,
        "a loadable config must survive --force"
    );
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), original);
    assert_eq!(
        config, first,
        "--force must not change where anything lives"
    );
    assert!(config.db_path.starts_with(dir.path()));
    assert!(config.media_dir.starts_with(dir.path()));
    assert_eq!(config.target_words, 1500);
}

#[test]
fn force_leaves_the_configured_database_in_place() {
    // The concrete consequence of the bug above: the story row must still be in the
    // configured database after --force, not in a fresh default-path one.
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(
        Some(&cfg),
        &InitOptions {
            title: Some("The Ashen Vale".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let (config, r) = init::init(Some(&cfg), &forced()).unwrap();
    assert!(
        config.db_path.starts_with(dir.path()),
        "{:?}",
        config.db_path
    );
    assert_eq!(r.title, "The Ashen Vale", "story must not be a fresh row");
    let store = Store::open(&config.db_path).unwrap();
    assert_eq!(store.story().unwrap().unwrap().title, "The Ashen Vale");
}

#[test]
fn force_refreshes_the_story_rows_prompt_hash() {
    // --force rewrote prompt.md, so a stale story.prompt_hash would record a hash
    // for content that no longer exists on disk.
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (config, _) = init::init(Some(&cfg), &opts()).unwrap();

    let prompt_path = config.prompt_path();
    std::fs::write(&prompt_path, "edited premise\n").unwrap();
    // Sync the row to the edited file so we can prove --force moves it back.
    let edited_hash = prompt::content_hash("edited premise\n");

    let (_, r) = init::init(Some(&cfg), &forced()).unwrap();
    assert_eq!(r.story, Action::Overwritten);
    assert_ne!(r.prompt_hash, edited_hash);

    let store = Store::open(&config.db_path).unwrap();
    let row = store.story().unwrap().unwrap();
    assert_eq!(row.prompt_hash, r.prompt_hash);
    assert_eq!(
        row.prompt_hash,
        prompt::content_hash(prompt::STARTER_PROMPT),
        "row must match the file --force just wrote"
    );
}

#[test]
fn force_keeps_story_values_the_operator_did_not_ask_to_change() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(
        Some(&cfg),
        &InitOptions {
            title: Some("The Ashen Vale".into()),
            protagonist: Some("Kaelen".into()),
            ..Default::default()
        },
    )
    .unwrap();

    // --force with only --protagonist: the title must survive.
    let (_, r) = init::init(
        Some(&cfg),
        &InitOptions {
            force: true,
            protagonist: Some("Vessa".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.title, "The Ashen Vale");
    assert_eq!(r.protagonist, "Vessa");
}

/// Config-repair semantics are tested at the `ensure_config` level on purpose: a
/// repaired config is by definition the starter, whose paths are the DEFAULTS, so
/// driving full `init` here would create a real database in the home directory.
#[test]
fn force_repairs_a_config_that_cannot_be_loaded() {
    for broken in [
        "this is not = = toml", // unparseable
        "buffer_target = 1\n",  // parses, fails validation
        "bufer_target = 9\n",   // unknown key
    ] {
        let dir = tmp();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, broken).unwrap();

        // Without --force it is left alone: replacing the operator's file uninvited
        // is the one thing this command must never do.
        assert_eq!(
            init::ensure_config(Some(&cfg), false).unwrap(),
            Action::Existed,
            "{broken:?}"
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), broken);
        // ...and the subsequent load fails loudly rather than silently defaulting.
        assert!(init::init(Some(&cfg), &opts()).is_err(), "{broken:?}");

        // With --force it recovers, and the replacement is loadable.
        assert_eq!(
            init::ensure_config(Some(&cfg), true).unwrap(),
            Action::Overwritten,
            "{broken:?}"
        );
        assert_eq!(Config::load_from(&cfg).unwrap().buffer_target, 3);
    }
}

#[test]
fn force_preserves_the_engine_owned_arc_outline() {
    // init --force refreshes the story row, and `--force` at chapter 60 is a
    // plausible way to fix a title. If the upsert wrote the whole row, the arc
    // outline would be silently erased. Verified here rather than trusted, because
    // init is what would destroy it.
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (config, _) = init::init(Some(&cfg), &opts()).unwrap();

    {
        let store = Store::open(&config.db_path).unwrap();
        store
            .set_arc_outline("## Act II\nKaelen betrays the guild.")
            .unwrap();
    }

    init::init(
        Some(&cfg),
        &InitOptions {
            force: true,
            title: Some("A New Title".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let store = Store::open(&config.db_path).unwrap();
    let row = store.story().unwrap().unwrap();
    assert_eq!(row.title, "A New Title");
    assert_eq!(
        row.arc_outline_md, "## Act II\nKaelen betrays the guild.",
        "arc outline must survive init --force"
    );
}

#[test]
fn force_reports_that_it_kept_a_valid_config() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(Some(&cfg), &opts()).unwrap();

    let (_, r) = init::init(Some(&cfg), &forced()).unwrap();
    assert!(r.config_kept_despite_force);
    let out = init::render_text(&r);
    assert!(out.contains("did not rewrite config.toml"), "{out}");
    assert!(out.contains("Delete the file"), "must say how:\n{out}");
}

#[test]
fn a_repaired_config_is_not_reported_as_kept() {
    let dir = tmp();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "not = = toml").unwrap();
    assert_eq!(
        init::ensure_config(Some(&cfg), true).unwrap(),
        Action::Overwritten
    );
}

#[test]
fn nothing_is_written_outside_the_configured_root() {
    // Regression: an earlier version of `ensure_config` rewrote a valid config to
    // the starter's DEFAULT paths under --force, so init then created a real
    // database under ~/.local/share/endlesslitrpg. Tests passed; the home
    // directory got polluted. Every path init reports must live under the root the
    // config named.
    let dir = tmp();
    let cfg = write_config(dir.path());
    for o in [opts(), forced()] {
        let (config, r) = init::init(Some(&cfg), &o).unwrap();
        for (label, p) in [
            ("db_path", &config.db_path),
            ("media_dir", &config.media_dir),
            ("story_dir", &config.story_dir),
            ("prompt_path", &r.prompt_path),
        ] {
            assert!(
                p.starts_with(dir.path()),
                "{label} escaped the test root: {p:?}"
            );
        }
        for d in &r.dirs_created {
            assert!(d.starts_with(dir.path()), "created {d:?} outside the root");
        }
    }
}

// -------------------------------------------------------- ignored flags

#[test]
fn flags_that_cannot_be_applied_are_reported_not_dropped() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(
        Some(&cfg),
        &InitOptions {
            title: Some("The Ashen Vale".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let (_, r) = init::init(
        Some(&cfg),
        &InitOptions {
            title: Some("Something Else".into()),
            protagonist: Some("Kaelen".into()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(r.title, "The Ashen Vale", "row must be untouched");
    assert_eq!(r.ignored_flags, vec!["--title", "--protagonist"]);
    let out = init::render_text(&r);
    assert!(out.contains("--title"), "{out}");
    assert!(
        out.contains("--force"),
        "must say how to apply them:\n{out}"
    );
}

#[test]
fn a_flag_matching_the_existing_value_is_not_reported_as_ignored() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    init::init(
        Some(&cfg),
        &InitOptions {
            title: Some("The Ashen Vale".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let (_, r) = init::init(
        Some(&cfg),
        &InitOptions {
            title: Some("The Ashen Vale".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(r.ignored_flags.is_empty(), "{:?}", r.ignored_flags);
    assert!(!init::render_text(&r).contains("ignored"));
}

// ------------------------------------------------------------- rendering

#[test]
fn the_summary_names_the_next_step_when_the_prompt_is_a_placeholder() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    let out = init::render_text(&r);
    assert!(out.contains("litrpg prompt"), "{out}");
    assert!(out.contains("placeholder"), "{out}");
    assert!(out.contains(&r.db_path.display().to_string()), "{out}");
    assert!(
        out.contains(&cfg.display().to_string()),
        "config path must be shown:\n{out}"
    );
}

#[test]
fn the_summary_does_not_nag_once_the_prompt_is_written() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (_, first) = init::init(Some(&cfg), &opts()).unwrap();
    std::fs::write(&first.prompt_path, "# A real premise\n").unwrap();

    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    let out = init::render_text(&r);
    assert!(!r.prompt_is_placeholder);
    assert!(out.contains("ready to generate"), "{out}");
    assert!(!out.contains("placeholder"), "{out}");
}

#[test]
fn the_report_serializes_to_json() {
    let dir = tmp();
    let cfg = write_config(dir.path());
    let (_, r) = init::init(Some(&cfg), &opts()).unwrap();
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains("\"story\":\"created\""), "{json}");
    assert!(json.contains("prompt_is_placeholder"), "{json}");
    assert!(json.contains("schema_version"), "{json}");
}
