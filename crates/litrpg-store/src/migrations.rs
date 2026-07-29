//! Schema DDL, versioned through SQLite's `user_version` pragma.

/// One migration: its file name, for error reporting, and its DDL.
///
/// A struct rather than two parallel arrays. Two lists that must stay index-aligned is the
/// duplication this project keeps tripping over — a name array and a SQL array could drift by
/// one and every subsequent error would then blame the wrong file.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

/// Index N holds the migration that moves the schema from version N to N+1.
///
/// Append only. Editing a shipped migration would leave existing databases in a
/// state no version number describes.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "001_initial",
        sql: include_str!("schema/001_initial.sql"),
    },
    Migration {
        name: "002_summary_uniqueness",
        sql: include_str!("schema/002_summary_uniqueness.sql"),
    },
    Migration {
        name: "003_playback_cursor",
        sql: include_str!("schema/003_playback_cursor.sql"),
    },
    Migration {
        name: "004_derive_media_paths",
        sql: include_str!("schema/004_derive_media_paths.sql"),
    },
    Migration {
        name: "005_engine_heartbeat",
        sql: include_str!("schema/005_engine_heartbeat.sql"),
    },
    Migration {
        name: "006_cast_identity_key",
        sql: include_str!("schema/006_cast_identity_key.sql"),
    },
    Migration {
        name: "007_subject_alias",
        sql: include_str!("schema/007_subject_alias.sql"),
    },
];

pub const TARGET_VERSION: i64 = MIGRATIONS.len() as i64;
