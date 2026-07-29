//! `litrpg prompt` — edit `story_dir/prompt.md`, the git-tracked source of truth
//! for the story prompt (§9.3).
//!
//! The editor command is injected rather than discovered inside the edit function,
//! so a test can substitute `true` for `nano`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{CliError, Result, io_err};

/// Re-exported from `litrpg-core`, which owns the single implementation.
///
/// The CLI writes `chapters.prompt_hash` and the engine reads it. Two
/// implementations that must agree, with nothing forcing them to, is the same class
/// of problem `litrpg-config` exists to solve for paths — so the hash lives in the
/// shared crate, not in either consumer. See `litrpg_core::hash` for why FNV-1a
/// rather than SHA-256, and why the algorithm tag is part of the output.
pub use litrpg_core::hash::{HASH_ALGO, content_hash};

pub const STARTER_PROMPT: &str = r#"# Story prompt

This file is the source of truth for the story. It is read at chapter
boundaries only, never mid-chapter, and every chapter records a hash of it, so
six months from now you can tell your own edits apart from model drift.

## Premise

<Describe the world, the tone and the protagonist. Replace this section.>

## Protagonist

<Name, what they want, what stands in the way.>

## Tone

<Prose style, pacing, how much combat, how much downtime.>

## Rules of the system

<How levels, stats and loot behave in this world. The validation gate enforces
numeric sanity, but the flavour is yours.>
"#;

/// What an edit did. `created` distinguishes "I made you a starter file" from
/// "I opened the file you already had".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptOutcome {
    pub path: PathBuf,
    pub created: bool,
    pub changed: bool,
    pub hash: String,
    pub previous_hash: String,
    pub bytes: usize,
}

/// Create `prompt.md` (and its parent) from the starter template if absent.
/// Returns `true` when a file was created.
pub fn ensure_prompt_file(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let mut f = std::fs::File::create(path).map_err(io_err(path))?;
    f.write_all(STARTER_PROMPT.as_bytes())
        .map_err(io_err(path))?;
    Ok(true)
}

/// Editor candidates in priority order: `$EDITOR`, then `nano`, then `vi`.
///
/// `$EDITOR` is split on whitespace so values like `code --wait` work; there is no
/// shell involved, so quoting is not honoured.
pub fn resolve_editor(env_editor: Option<&str>) -> Vec<Vec<String>> {
    let mut candidates = Vec::new();
    if let Some(e) = env_editor {
        let argv: Vec<String> = e.split_whitespace().map(str::to_string).collect();
        if !argv.is_empty() {
            candidates.push(argv);
        }
    }
    candidates.push(vec!["nano".to_string()]);
    candidates.push(vec!["vi".to_string()]);
    candidates
}

/// Open `path` in the first candidate that can actually be spawned, then validate
/// the result and hash it.
///
/// A missing binary falls through to the next candidate; a binary that runs and
/// *fails* is reported rather than skipped, because "nano exited 1" and "nano is
/// not installed" call for different responses.
pub fn edit_prompt(path: &Path, candidates: &[Vec<String>]) -> Result<PromptOutcome> {
    let created = ensure_prompt_file(path)?;
    let before = std::fs::read_to_string(path).map_err(io_err(path))?;
    let previous_hash = content_hash(&before);

    let mut tried = Vec::new();
    for argv in candidates {
        let (cmd, args) = argv.split_first().expect("candidate argv is never empty");
        tried.push(cmd.clone());

        match Command::new(cmd).args(args).arg(path).status() {
            Ok(status) if status.success() => {
                return finish(path, created, previous_hash);
            }
            Ok(status) => {
                return Err(CliError::EditorFailed {
                    cmd: argv.join(" "),
                    status: status
                        .code()
                        .map(|c| format!("exit code {c}"))
                        .unwrap_or_else(|| "terminated by signal".to_string()),
                    path: path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(CliError::Io {
                    path: PathBuf::from(cmd),
                    source,
                });
            }
        }
    }

    Err(CliError::NoEditor {
        tried: tried.join(", "),
    })
}

fn finish(path: &Path, created: bool, previous_hash: String) -> Result<PromptOutcome> {
    let after = std::fs::read_to_string(path).map_err(io_err(path))?;
    if after.trim().is_empty() {
        return Err(CliError::EmptyPrompt {
            path: path.to_path_buf(),
        });
    }
    let hash = content_hash(&after);
    Ok(PromptOutcome {
        path: path.to_path_buf(),
        created,
        changed: hash != previous_hash,
        previous_hash,
        bytes: after.len(),
        hash,
    })
}

pub fn render_text(o: &PromptOutcome) -> String {
    let mut out = String::new();
    if o.created {
        out.push_str(&format!(
            "Created {} from the starter template.\n",
            o.path.display()
        ));
    }
    out.push_str(&format!("{}\n", o.path.display()));
    out.push_str(&format!("  hash   {}\n", o.hash));
    out.push_str(&format!("  bytes  {}\n", o.bytes));
    match (o.created, o.changed) {
        (_, true) => out.push_str(
            "\nPrompt changed. Takes effect at the next chapter boundary —\nthe chapter currently rendering keeps the old prompt (spec §9.3).\n",
        ),
        // Created but left alone: "no change" would be technically true and
        // practically misleading — the story has no premise yet.
        (true, false) => out.push_str(
            "\nStill the unedited starter template. Fill in the premise, protagonist\nand tone before the next chapter is written.\n",
        ),
        (false, false) => out.push_str("\nNo change to the prompt.\n"),
    }
    out
}
