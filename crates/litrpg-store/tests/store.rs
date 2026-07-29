use litrpg_store::Store;

#[test]
fn opens_in_memory_and_applies_migrations() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);
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
            "ledger",
            "lore",
            "notes",
            "segments",
            "story",
            "summaries",
        ]
    );
}

#[test]
fn migrations_are_idempotent() {
    let store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);
}
