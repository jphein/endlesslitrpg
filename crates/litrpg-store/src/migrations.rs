//! Schema DDL, versioned through SQLite's `user_version` pragma.

/// Index N holds the migration that moves the schema from version N to N+1.
///
/// Append only. Editing a shipped migration would leave existing databases in a
/// state no version number describes.
pub const MIGRATIONS: &[&str] = &[
    include_str!("schema/001_initial.sql"),
    include_str!("schema/002_summary_uniqueness.sql"),
    include_str!("schema/003_playback_cursor.sql"),
    include_str!("schema/004_derive_media_paths.sql"),
];

pub const TARGET_VERSION: i64 = MIGRATIONS.len() as i64;
