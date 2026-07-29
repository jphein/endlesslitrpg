//! `litrpg init` — get from a clean machine to a story in one command.
//!
//! Idempotent and safe to re-run: without `--force`, anything that already exists
//! is reported and left strictly alone. That property is the whole point — the
//! command people re-run "just to be sure" must not be the command that overwrites
//! a prompt they spent an hour on.

use std::path::{Path, PathBuf};

use litrpg_config::Config;
use litrpg_core::hash::content_hash;
use litrpg_store::Store;
use serde::Serialize;

use crate::{Result, io_err, prompt};

/// Placeholder title used when `--title` is not given. Deliberately obvious: it
/// should read as "you have not named this yet".
pub const DEFAULT_TITLE: &str = "Untitled Story";

/// What one step of `init` did. `Created` vs `Existed` is the distinction the whole
/// command reports on, so it is modelled rather than stringly-typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Created,
    /// Already present and left untouched.
    Existed,
    /// Already present and rewritten because `--force` was given.
    Overwritten,
}

impl Action {
    pub fn changed(self) -> bool {
        !matches!(self, Self::Existed)
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Existed => "exists",
            Self::Overwritten => "overwritten",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitReport {
    pub config_path: Option<PathBuf>,
    pub config: Action,
    pub db_path: PathBuf,
    pub media_dir: PathBuf,
    pub story_dir: PathBuf,
    /// Directories that did not exist and were created.
    pub dirs_created: Vec<PathBuf>,
    pub prompt_path: PathBuf,
    pub prompt: Action,
    pub prompt_hash: String,
    pub schema_version: i64,
    pub story: Action,
    pub title: String,
    pub protagonist: String,
    pub target_words: u32,
    /// True when the prompt is still the unedited starter template, so the caller
    /// can tell the operator the premise is a placeholder.
    pub prompt_is_placeholder: bool,
    /// Flags that were supplied but not applied because a story row already exists
    /// and `--force` was not given. Reported rather than silently dropped.
    pub ignored_flags: Vec<String>,
}

/// Options mirroring the CLI flags, so `main.rs` stays a thin translation layer.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub force: bool,
    pub title: Option<String>,
    pub protagonist: Option<String>,
}

/// Create the config file if absent. Returns the action taken.
///
/// `config_path` is `None` when the platform has no config dir *and*
/// `$LITRPG_CONFIG` is unset; there is nowhere to write, which is reported rather
/// than guessed at.
pub fn ensure_config(path: Option<&Path>, force: bool) -> Result<Action> {
    let Some(path) = path else {
        return Ok(Action::Existed);
    };
    if path.exists() {
        if !force {
            return Ok(Action::Existed);
        }
        write_config(path)?;
        return Ok(Action::Overwritten);
    }
    write_config(path)?;
    Ok(Action::Created)
}

fn write_config(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    std::fs::write(path, litrpg_config::STARTER_CONFIG).map_err(io_err(path))
}

/// Create `db_path`'s parent, `media_dir` and `story_dir`. Returns those that did
/// not already exist, so a re-run can report honestly that it changed nothing.
pub fn ensure_dirs(config: &Config) -> Result<Vec<PathBuf>> {
    let mut created = Vec::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    if let Some(parent) = config.db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        targets.push(parent.to_path_buf());
    }
    targets.push(config.media_dir.clone());
    targets.push(config.story_dir.clone());

    for dir in targets {
        if dir.is_dir() {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        if !created.contains(&dir) {
            created.push(dir);
        }
    }
    Ok(created)
}

/// Write `prompt.md` from the starter template if absent, or unconditionally under
/// `force`. Returns the action and the resulting content hash.
pub fn ensure_prompt(path: &Path, force: bool) -> Result<(Action, String, bool)> {
    let existed = path.exists();
    let action = if existed && !force {
        Action::Existed
    } else {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        std::fs::write(path, prompt::STARTER_PROMPT).map_err(io_err(path))?;
        if existed {
            Action::Overwritten
        } else {
            Action::Created
        }
    };
    let body = std::fs::read_to_string(path).map_err(io_err(path))?;
    let is_placeholder = body == prompt::STARTER_PROMPT;
    Ok((action, content_hash(&body), is_placeholder))
}

/// Outcome of the story-row step.
struct StoryOutcome {
    action: Action,
    title: String,
    protagonist: String,
    target_words: u32,
    ignored_flags: Vec<String>,
}

/// Create the singleton `story` row if absent (spec §6 / §9.1).
///
/// Nothing else writes this table, and `/api/character` with no subject resolves
/// through `story.protagonist`, so without this row the watch's character screen
/// has no subject to render.
///
/// Under `--force` the row is rewritten. That is not cosmetic: `--force` has just
/// replaced `prompt.md` with the starter template, so leaving the old
/// `story.prompt_hash` in place would record a hash for content that no longer
/// exists on disk. `Store::upsert_story` preserves `arc_outline_md`, which is
/// engine-owned and none of init's business.
fn ensure_story(
    store: &Store,
    config: &Config,
    prompt_path: &Path,
    prompt_hash: &str,
    opts: &InitOptions,
) -> Result<StoryOutcome> {
    let prompt_path_str = prompt_path.display().to_string();

    match store.story()? {
        // Exists and we were not told to overwrite: leave it strictly alone.
        Some(row) if !opts.force => {
            let mut ignored_flags = Vec::new();
            if opts.title.as_deref().is_some_and(|t| t != row.title) {
                ignored_flags.push("--title".to_string());
            }
            if opts
                .protagonist
                .as_deref()
                .is_some_and(|p| p != row.protagonist)
            {
                ignored_flags.push("--protagonist".to_string());
            }
            Ok(StoryOutcome {
                action: Action::Existed,
                title: row.title,
                protagonist: row.protagonist,
                target_words: row.target_words,
                ignored_flags,
            })
        }
        // --force: refresh, but keep values the operator did not ask to change.
        Some(row) => {
            let title = opts.title.clone().unwrap_or(row.title);
            let protagonist = opts.protagonist.clone().unwrap_or(row.protagonist);
            store.upsert_story(&litrpg_store::NewStory {
                title: title.clone(),
                protagonist: protagonist.clone(),
                prompt_path: prompt_path_str,
                prompt_hash: prompt_hash.to_string(),
                target_words: config.target_words,
            })?;
            Ok(StoryOutcome {
                action: Action::Overwritten,
                title,
                protagonist,
                target_words: config.target_words,
                ignored_flags: Vec::new(),
            })
        }
        None => {
            let title = opts
                .title
                .clone()
                .unwrap_or_else(|| DEFAULT_TITLE.to_string());
            let protagonist = opts.protagonist.clone().unwrap_or_default();
            store.insert_story_if_absent(&litrpg_store::NewStory {
                title: title.clone(),
                protagonist: protagonist.clone(),
                prompt_path: prompt_path_str,
                prompt_hash: prompt_hash.to_string(),
                target_words: config.target_words,
            })?;
            Ok(StoryOutcome {
                action: Action::Created,
                title,
                protagonist,
                target_words: config.target_words,
                ignored_flags: Vec::new(),
            })
        }
    }
}

/// Run the whole sequence.
///
/// `config` is passed in already loaded so the caller controls resolution
/// (`--config` overrides), and `config_path` is where a starter file should be
/// written — the two can differ, which is why they are separate arguments.
pub fn init(
    config: &Config,
    config_path: Option<&Path>,
    opts: &InitOptions,
) -> Result<InitReport> {
    let config_action = ensure_config(config_path, opts.force)?;
    let dirs_created = ensure_dirs(config)?;

    let prompt_path = config.prompt_path();
    let (prompt_action, prompt_hash, prompt_is_placeholder) =
        ensure_prompt(&prompt_path, opts.force)?;

    // Opening runs migrations.
    let store = Store::open(&config.db_path)?;
    let schema_version = store.schema_version()?;

    let story = ensure_story(&store, config, &prompt_path, &prompt_hash, opts)?;

    Ok(InitReport {
        config_path: config_path.map(Path::to_path_buf),
        config: config_action,
        db_path: config.db_path.clone(),
        media_dir: config.media_dir.clone(),
        story_dir: config.story_dir.clone(),
        dirs_created,
        prompt_path,
        prompt: prompt_action,
        prompt_hash,
        schema_version,
        story: story.action,
        title: story.title,
        protagonist: story.protagonist,
        target_words: story.target_words,
        prompt_is_placeholder,
        ignored_flags: story.ignored_flags,
    })
}

pub fn render_text(r: &InitReport) -> String {
    let mut out = String::new();
    out.push_str("Initialised endless-litrpg\n\n");

    match &r.config_path {
        Some(p) => out.push_str(&format!("  config     {} ({})\n", p.display(), r.config.verb())),
        None => out.push_str(
            "  config     no config directory on this platform and $LITRPG_CONFIG is unset;\n\
             \x20            running on built-in defaults\n",
        ),
    }
    out.push_str(&format!(
        "  database   {} (schema v{})\n",
        r.db_path.display(),
        r.schema_version
    ));
    out.push_str(&format!("  media      {}\n", r.media_dir.display()));
    out.push_str(&format!("  story      {}\n", r.story_dir.display()));
    out.push_str(&format!(
        "  prompt     {} ({})\n",
        r.prompt_path.display(),
        r.prompt.verb()
    ));
    out.push_str(&format!("  hash       {}\n", r.prompt_hash));
    out.push_str(&format!(
        "  story row  {} — {:?} / protagonist {:?} / {} words\n",
        r.story.verb(),
        r.title,
        r.protagonist,
        r.target_words
    ));

    if !r.dirs_created.is_empty() {
        out.push_str("\n  Created directories:\n");
        for d in &r.dirs_created {
            out.push_str(&format!("    {}\n", d.display()));
        }
    }

    out.push_str("\nNext\n");
    if r.prompt_is_placeholder {
        out.push_str(&format!(
            "  1. litrpg prompt      — the premise is still the starter placeholder;\n                          the story cannot be written until you replace it\n  2. litrpg status      — check buffer and drift once chapters exist\n\n  Config: {}\n",
            r.config_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(defaults)".to_string())
        ));
    } else {
        out.push_str("  litrpg status         — the prompt is already written; you are ready to generate\n");
    }
    out
}
