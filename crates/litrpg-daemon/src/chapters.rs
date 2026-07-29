//! `/api/story`, `/api/chapters`, `/api/chapters/{n}`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use litrpg_core::manifest::Manifest;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiResult;

#[derive(Debug, Serialize)]
pub struct StoryResponse {
    pub title: String,
    pub description: String,
    pub protagonist: String,
    pub language: String,
    pub chapter_count: usize,
    pub latest_chapter: u32,
    /// Chapters whose ledger deltas were recorded but whose audio is stale.
    pub dirty_chapters: Vec<u32>,
    pub sample_rate: u32,
    pub bytes_per_ms: u32,
    /// From the `story` table; `None` before `litrpg init` has written a row.
    pub target_words: Option<u32>,
    pub prompt_hash: Option<String>,
    /// `true` when a `story` row exists. Lets a client distinguish an initialised
    /// deployment from one still serving configured placeholders.
    pub initialised: bool,
}

/// `GET /api/story`
///
/// `title` and `protagonist` come from the **`story` table** when a row exists, falling
/// back to config otherwise. That order is deliberate: the table is the canonical
/// record of what the story *is*, while config carries the bootstrap default used
/// before `litrpg init` has run. `description`/`language` stay config-only — they are
/// publishing concerns with no column.
///
/// `sample_rate`/`bytes_per_ms` are echoed from `litrpg-core` so a client never
/// hardcodes the 32 B/ms figure its Range arithmetic depends on.
pub async fn get_story(State(state): State<Arc<AppState>>) -> ApiResult<Json<StoryResponse>> {
    let store = state.store.lock().await;
    let latest = store.latest_number()?;
    let chapters = store.chapters_since(0)?;
    let dirty = store.dirty_chapters()?;
    let story = store.story()?;
    drop(store);

    let cfg = &state.config.story;
    // A blank column is treated as absent so an empty `title` cannot shadow the
    // configured fallback with an empty string.
    let pick = |from_store: Option<&str>, fallback: &str| {
        from_store
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback)
            .to_string()
    };

    Ok(Json(StoryResponse {
        title: pick(story.as_ref().map(|s| s.title.as_str()), &cfg.title),
        description: cfg.description.clone(),
        protagonist: pick(
            story.as_ref().map(|s| s.protagonist.as_str()),
            &cfg.protagonist,
        ),
        language: cfg.language.clone(),
        chapter_count: chapters.len(),
        latest_chapter: latest,
        dirty_chapters: dirty,
        sample_rate: litrpg_core::manifest::SAMPLE_RATE_HZ,
        bytes_per_ms: litrpg_core::manifest::BYTES_PER_MS,
        target_words: story.as_ref().map(|s| s.target_words),
        prompt_hash: story.as_ref().map(|s| s.prompt_hash.clone()),
        initialised: story.is_some(),
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct SinceQuery {
    pub since: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ChapterSummary {
    pub number: u32,
    pub title: String,
    pub duration_ms: u32,
    pub has_audio: bool,
    pub words: usize,
    pub state_dirty: bool,
    /// Present only when audio exists, so the watch can go straight to the byte
    /// stream without a second round trip to discover the URL.
    pub pcm_url: Option<String>,
    pub mp3_url: Option<String>,
    pub total_bytes: Option<u64>,
}

/// `GET /api/chapters?since=N`
///
/// `since` is exclusive (`number > N`), matching `Store::chapters_since`, so a client
/// polls with the highest number it already holds and gets only what is new.
pub async fn list_chapters(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SinceQuery>,
) -> ApiResult<Json<Vec<ChapterSummary>>> {
    let store = state.store.lock().await;
    let rows = store.chapters_since(q.since.unwrap_or(0))?;
    drop(store);

    let base = &state.config.story.base_url;
    let out = rows
        .into_iter()
        .map(|c| ChapterSummary {
            number: c.number,
            title: c.title,
            duration_ms: c.duration_ms,
            has_audio: c.has_audio,
            words: c.text_md.split_whitespace().count(),
            state_dirty: c.state_dirty,
            pcm_url: c
                .has_audio
                .then(|| format!("{base}/media/{:04}.pcm", c.number)),
            mp3_url: c
                .has_audio
                .then(|| format!("{base}/media/{:04}.mp3", c.number)),
            // duration_ms * 32, the same identity the manifest precomputes. Exact
            // because every segment is zero-padded to a 32-byte boundary (spec §8.1).
            total_bytes: c
                .has_audio
                .then(|| c.duration_ms as u64 * litrpg_core::manifest::BYTES_PER_MS as u64),
        })
        .collect();

    Ok(Json(out))
}

#[derive(Debug, Serialize)]
pub struct ChapterDetail {
    pub number: u32,
    pub title: String,
    pub text_md: String,
    pub prompt_hash: String,
    pub duration_ms: u32,
    pub has_audio: bool,
    pub state_dirty: bool,
    pub pcm_url: Option<String>,
    pub mp3_url: Option<String>,
    /// Rebuilt from the `segments` rows rather than the stored `manifest_json`, so
    /// the response cannot disagree with the table the highlighting also reads.
    pub manifest: Manifest,
    /// `is_contiguous()` from `litrpg-core`. Surfaced rather than asserted: if it is
    /// ever false the byte offsets do not address one continuous stream, and a client
    /// should know that before it starts issuing ranges.
    pub manifest_contiguous: bool,
}

/// `GET /api/chapters/{n}` — text + segments + manifest.
pub async fn get_chapter(
    State(state): State<Arc<AppState>>,
    Path(n): Path<u32>,
) -> ApiResult<Json<ChapterDetail>> {
    let store = state.store.lock().await;
    let row = store.chapter(n)?;
    let segments = store.segments(n)?;
    drop(store);

    let manifest = Manifest::new(n, segments);
    let base = &state.config.story.base_url;

    Ok(Json(ChapterDetail {
        number: row.number,
        title: row.title,
        text_md: row.text_md,
        prompt_hash: row.prompt_hash,
        duration_ms: row.duration_ms,
        has_audio: row.has_audio,
        state_dirty: row.state_dirty,
        pcm_url: row.has_audio.then(|| format!("{base}/media/{n:04}.pcm")),
        mp3_url: row.has_audio.then(|| format!("{base}/media/{n:04}.mp3")),
        manifest_contiguous: manifest.is_contiguous(),
        manifest,
    }))
}
