use litrpg_store::Store;
use litrpg_store::migrations::TARGET_VERSION;

#[test]
fn opens_in_memory_and_applies_migrations() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.schema_version().unwrap(), TARGET_VERSION);
}

#[test]
fn all_expected_tables_exist() {
    let store = Store::open_in_memory().unwrap();
    let mut tables = store.table_names().unwrap();
    tables.sort();
    assert_eq!(
        tables,
        vec![
            "cast",
            "chapters",
            "engine_heartbeat",
            "ledger",
            "lore",
            "notes",
            "segments",
            "story",
            // Added by migration 007: one character recorded under two names, resolved at
            // read time so the append-only ledger is never rewritten (#11).
            "subject_alias",
            "summaries",
        ]
    );
}

/// A database from a newer build must be refused, not silently accepted. Every migration
/// index sits below such a `user_version`, so the loop applies nothing and the pragma
/// update is skipped — an old binary would proceed against a schema it does not know, and
/// stay harmless only until some migration drops a column, at which point the symptom is
/// an obscure SQL error rather than "your binary is too old".
///
/// Mixed binaries are the normal state here: the engine runs under systemd while the CLI
/// is rebuilt against the same file.
#[test]
fn a_database_from_a_newer_build_is_refused() {
    let store = Store::open_in_memory().unwrap();
    let future = TARGET_VERSION + 1;
    store
        .raw_execute_for_tests(&format!("PRAGMA user_version = {future}"))
        .unwrap();

    let err = store.migrate().unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(&format!("version {future}")), "{msg}");
    assert!(
        msg.contains(&format!("supports {TARGET_VERSION}")),
        "the error should name both versions: {msg}"
    );
}

#[test]
fn migrations_are_idempotent() {
    let store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();
    assert_eq!(store.schema_version().unwrap(), TARGET_VERSION);
}
