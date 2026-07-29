//! Schema DDL, versioned through SQLite's `user_version` pragma.

/// Index N holds the migration that moves the schema from version N to N+1.
pub const MIGRATIONS: &[&str] = &[include_str!("schema/001_initial.sql")];

pub const TARGET_VERSION: i64 = MIGRATIONS.len() as i64;
