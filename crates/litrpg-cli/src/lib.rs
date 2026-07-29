//! Behaviour behind the `litrpg` command.
//!
//! Every command is a function that takes `&Store` (and plain values) and returns
//! data. `main.rs` does argument parsing and printing and nothing else, so the
//! behaviour here is testable against `Store::open_in_memory()` without spawning
//! a process or touching a real database.

use std::path::PathBuf;

use thiserror::Error;

pub mod cast;
pub mod init;
pub mod note;
pub mod prompt;
pub mod render;
pub mod rewind;
pub mod state;
pub mod status;

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Store(#[from] litrpg_store::StoreError),

    #[error(transparent)]
    Config(#[from] litrpg_config::ConfigError),

    #[error("a director note must contain non-whitespace text")]
    EmptyNote,

    // `VoiceRefError` is `no_std` and does not implement `std::error::Error`, so
    // its Display output is captured as a string rather than chained as a source.
    #[error("invalid voice_ref {got:?}: {reason}")]
    BadVoiceRef { got: String, reason: String },

    #[error("{speaker:?} is not in the cast — pass --new to add a new cast member")]
    UnknownSpeaker { speaker: String },

    #[error("{path} is empty — a story prompt must not be blank")]
    EmptyPrompt { path: PathBuf },

    #[error("no usable editor found (tried: {tried})")]
    NoEditor { tried: String },

    #[error("editor {cmd:?} exited unsuccessfully ({status}); {path} left unchanged")]
    EditorFailed {
        cmd: String,
        status: String,
        path: PathBuf,
    },

    #[error("i/o error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = core::result::Result<T, CliError>;

pub(crate) fn io_err(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> CliError + '_ {
    move |source| CliError::Io {
        path: path.to_path_buf(),
        source,
    }
}
