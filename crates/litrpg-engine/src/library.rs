//! The real [`Library`] over `litrpg-store`.
//!
//! Thin by design: the store returns *every* lore row and selection stays with
//! [`litrpg_ember::match_lore`], so keyword matching remains a pure function that is
//! testable without a database. Pushing the filtering into SQL would trade that for
//! nothing — the whole lore table is a few kilobytes.
//!
//! # Why it shares the engine's connection
//!
//! [`StoreLibrary`] holds the **same** `Arc<Mutex<Store>>` the engine does, rather than
//! opening a second connection to the same file. Two connections would work for reads but
//! invite `SQLITE_BUSY` on `put_summary` whenever the engine's connection is mid-write, and
//! would rule out `open_in_memory()` entirely — each in-memory connection gets its own
//! private database, so the adapter would silently read an empty one.
//!
//! The mutex is **not reentrant**, so a `Library` call must never happen while a
//! [`Engine::with_store`](crate::Engine::with_store) guard is alive. The cycle is written so
//! every guard is released before the next statement; keep it that way.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use litrpg_ember::prompt::{ChapterSummary, LoreEntry};
use litrpg_store::Store;

use crate::error::EngineError;
use crate::ports::{Library, StoryMeta};

/// [`Library`] backed by the story database.
pub struct StoreLibrary {
    store: Arc<Mutex<Store>>,
    /// Base for `story.prompt_path`, which is stored **relative** (migration 004) so a project
    /// folder can be moved without leaving the database pointing at where it used to be.
    story_dir: PathBuf,
}

impl StoreLibrary {
    pub fn new(store: Arc<Mutex<Store>>, story_dir: impl Into<PathBuf>) -> Self {
        Self {
            store,
            story_dir: story_dir.into(),
        }
    }

    fn with_store<T>(
        &self,
        f: impl FnOnce(&Store) -> litrpg_store::Result<T>,
    ) -> Result<T, EngineError> {
        let guard = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard).map_err(EngineError::Store)
    }
}

impl Library for StoreLibrary {
    /// The `story` row, with the premise read from `prompt_path`.
    ///
    /// The prompt text lives on disk and is git-tracked (§9.3) — the database stores only
    /// the path and the hash of what the CLI last saw. Reading the file here means an edit
    /// JP made in `$EDITOR` takes effect at the next chapter boundary with no extra step,
    /// which is exactly the behaviour §9.3 asks for.
    ///
    /// `prompt_path` is relative to `story_dir` since migration 004. An absolute value still
    /// works, because `Path::join` lets it win — so a deployment that deliberately points the
    /// premise somewhere else is not broken by the change.
    fn story(&self) -> Result<StoryMeta, EngineError> {
        let row = self
            .with_store(|s| s.story())?
            .ok_or_else(|| EngineError::Library {
                detail: "no story row; run `litrpg init` first".to_string(),
            })?;

        // `Path::join` lets an absolute value win, so a hand-set absolute path still resolves.
        // Forgiving on the way in, relative on the way out.
        let path = self.story_dir.join(Path::new(&row.prompt_path));
        let prompt_md = std::fs::read_to_string(&path).map_err(|e| EngineError::Library {
            detail: format!("reading story prompt {}: {e}", path.display()),
        })?;

        if prompt_md.trim().is_empty() {
            // Generating against an empty premise would produce a chapter with no
            // connection to the story, and it would look like a successful cycle.
            return Err(EngineError::Library {
                detail: format!("story prompt {} is empty", path.display()),
            });
        }

        Ok(StoryMeta {
            title: row.title,
            protagonist: row.protagonist,
            prompt_md,
            arc_outline_md: row.arc_outline_md,
            target_words: row.target_words,
        })
    }

    fn lore(&self) -> Result<Vec<LoreEntry>, EngineError> {
        // `updated_chapter` is dropped: it is provenance for the operator, not context for
        // the model. Ordering is re-derived by `match_lore` anyway.
        Ok(self
            .with_store(|s| s.lore())?
            .into_iter()
            .map(|r| LoreEntry {
                name: r.name,
                kind: r.kind,
                keywords: r.keywords,
                body_md: r.body_md,
                priority: r.priority,
                always_on: r.always_on,
            })
            .collect())
    }

    /// The last `limit` chapter summaries, **oldest first** — the order they belong in a
    /// prompt. The store already returns them that way, so this must not reverse them.
    fn recent_summaries(&self, limit: usize) -> Result<Vec<ChapterSummary>, EngineError> {
        Ok(self
            .with_store(|s| s.recent_chapter_summaries(limit))?
            .into_iter()
            .map(|r| ChapterSummary {
                // For a level-0 row, `to_ch` is the chapter it summarises.
                chapter: r.to_ch,
                body_md: r.body_md,
            })
            .collect())
    }

    fn put_summary(&self, chapter: u32, body_md: &str) -> Result<(), EngineError> {
        // Idempotent by chapter in the store (unique index on level/from_ch/to_ch), so
        // re-extracting a `state_dirty` chapter replaces its summary rather than adding a
        // second row that would double that chapter's weight in retrieval.
        self.with_store(|s| s.put_chapter_summary(chapter, body_md))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(Store::open_in_memory().unwrap()))
    }

    #[test]
    fn a_missing_story_row_is_a_clear_error_not_a_default() {
        let lib = StoreLibrary::new(shared(), "");
        let err = lib.story().unwrap_err();
        assert!(format!("{err}").contains("litrpg init"), "got {err}");
    }

    #[test]
    fn lore_rows_map_onto_ember_lore_entries() {
        let store = shared();
        store
            .lock()
            .unwrap()
            .upsert_lore("Ashen Vale", "place", "vale,ash", "A vale.", 10, true, 1)
            .unwrap();

        let lib = StoreLibrary::new(store, "");
        let lore = lib.lore().unwrap();
        assert_eq!(lore.len(), 1);
        assert_eq!(lore[0].name, "Ashen Vale");
        assert_eq!(lore[0].keywords, "vale,ash");
        assert_eq!(lore[0].priority, 10);
        assert!(lore[0].always_on);
    }

    #[test]
    fn summaries_come_back_oldest_first_and_are_idempotent_by_chapter() {
        let store = shared();
        let lib = StoreLibrary::new(store, "");

        lib.put_summary(1, "first").unwrap();
        lib.put_summary(2, "second").unwrap();
        lib.put_summary(3, "third").unwrap();

        let got = lib.recent_summaries(5).unwrap();
        assert_eq!(
            got.iter().map(|s| s.chapter).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "oldest first is the order a prompt wants; do not reverse it"
        );
        assert_eq!(got[0].body_md, "first");

        // Re-extraction replaces rather than duplicating.
        lib.put_summary(2, "second, corrected").unwrap();
        let got = lib.recent_summaries(5).unwrap();
        assert_eq!(got.len(), 3, "a re-extraction must not add a second row");
        assert_eq!(got[1].body_md, "second, corrected");
    }

    #[test]
    fn the_window_keeps_the_most_recent_summaries() {
        let store = shared();
        let lib = StoreLibrary::new(store, "");
        for c in 1..=8 {
            lib.put_summary(c, &format!("ch{c}")).unwrap();
        }
        let got = lib.recent_summaries(5).unwrap();
        assert_eq!(
            got.iter().map(|s| s.chapter).collect::<Vec<_>>(),
            vec![4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn an_empty_prompt_file_is_refused_rather_than_generating_from_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "   \n").unwrap();

        let store = shared();
        store
            .lock()
            .unwrap()
            .insert_story_if_absent(&litrpg_store::NewStory {
                title: "T".into(),
                protagonist: "K".into(),
                prompt_path: prompt.to_string_lossy().to_string(),
                prompt_hash: litrpg_core::content_hash("   \n"),
                target_words: 500,
            })
            .unwrap();

        let err = StoreLibrary::new(store, "").story().unwrap_err();
        assert!(format!("{err}").contains("empty"), "got {err}");
    }

    /// Migration 004 made `prompt_path` relative to `story_dir`, so a project folder can move
    /// without leaving the database pointing at where it used to be — which is exactly the bug
    /// that made `litrpg play 1` fail on the live story.
    #[test]
    fn a_relative_prompt_path_resolves_against_story_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prompt.md"), "Relative premise.").unwrap();

        let store = shared();
        store
            .lock()
            .unwrap()
            .insert_story_if_absent(&litrpg_store::NewStory {
                title: "T".into(),
                protagonist: "K".into(),
                // Just the basename, as migration 004 stores it.
                prompt_path: "prompt.md".into(),
                prompt_hash: litrpg_core::content_hash("Relative premise."),
                target_words: 500,
            })
            .unwrap();

        let lib = StoreLibrary::new(store, dir.path());
        assert_eq!(lib.story().unwrap().prompt_md, "Relative premise.");
    }

    #[test]
    fn the_same_relative_path_follows_a_moved_story_dir() {
        // The point of the migration: the row does not change, only the base does.
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("prompt.md"), "In A.").unwrap();
        std::fs::write(b.path().join("prompt.md"), "In B.").unwrap();

        let store = shared();
        store
            .lock()
            .unwrap()
            .insert_story_if_absent(&litrpg_store::NewStory {
                title: "T".into(),
                protagonist: "K".into(),
                prompt_path: "prompt.md".into(),
                prompt_hash: litrpg_core::content_hash("In A."),
                target_words: 500,
            })
            .unwrap();

        assert_eq!(
            StoreLibrary::new(Arc::clone(&store), a.path())
                .story()
                .unwrap()
                .prompt_md,
            "In A."
        );
        assert_eq!(
            StoreLibrary::new(store, b.path())
                .story()
                .unwrap()
                .prompt_md,
            "In B.",
            "the identical row must resolve against whichever story_dir is configured"
        );
    }

    /// Forgiving on the way in: an operator who deliberately points the premise outside the
    /// project keeps working, because `Path::join` lets an absolute value win.
    #[test]
    fn an_absolute_prompt_path_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("elsewhere.md");
        std::fs::write(&prompt, "Absolute premise.").unwrap();

        let store = shared();
        store
            .lock()
            .unwrap()
            .insert_story_if_absent(&litrpg_store::NewStory {
                title: "T".into(),
                protagonist: "K".into(),
                prompt_path: prompt.to_string_lossy().to_string(),
                prompt_hash: litrpg_core::content_hash("Absolute premise."),
                target_words: 500,
            })
            .unwrap();

        // A completely unrelated story_dir, to prove the absolute path is what is used.
        let other = tempfile::tempdir().unwrap();
        let lib = StoreLibrary::new(store, other.path());
        assert_eq!(lib.story().unwrap().prompt_md, "Absolute premise.");
    }

    #[test]
    fn a_missing_prompt_file_names_the_resolved_path_not_the_stored_one() {
        // The error has to show where we actually looked, or a relative row plus the wrong
        // story_dir is undiagnosable.
        let dir = tempfile::tempdir().unwrap();
        let store = shared();
        store
            .lock()
            .unwrap()
            .insert_story_if_absent(&litrpg_store::NewStory {
                title: "T".into(),
                protagonist: "K".into(),
                prompt_path: "prompt.md".into(),
                prompt_hash: "x".into(),
                target_words: 500,
            })
            .unwrap();

        let err = StoreLibrary::new(store, dir.path()).story().unwrap_err();
        let shown = format!("{err}");
        assert!(
            shown.contains(&dir.path().display().to_string()),
            "the error must name the resolved path: {shown}"
        );
    }

    #[test]
    fn the_premise_is_read_from_disk_so_an_editor_change_takes_effect() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        std::fs::write(&prompt, "Original premise.").unwrap();

        let store = shared();
        store
            .lock()
            .unwrap()
            .insert_story_if_absent(&litrpg_store::NewStory {
                title: "T".into(),
                protagonist: "K".into(),
                prompt_path: prompt.to_string_lossy().to_string(),
                prompt_hash: litrpg_core::content_hash("Original premise."),
                target_words: 500,
            })
            .unwrap();

        let lib = StoreLibrary::new(store, "");
        assert_eq!(lib.story().unwrap().prompt_md, "Original premise.");

        std::fs::write(&prompt, "Edited premise.").unwrap();
        assert_eq!(
            lib.story().unwrap().prompt_md,
            "Edited premise.",
            "the prompt file is the source of truth, re-read each cycle"
        );
    }
}
