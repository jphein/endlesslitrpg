//! `litrpg render <N>` — stub.
//!
//! Audio rendering needs the TTS plugin layer (spec §7), which lives in the
//! `litrpg-tts` crate and is being built separately. Nothing here touches audio.

use litrpg_store::Store;
use serde::Serialize;

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderStub {
    pub chapter: u32,
    pub implemented: bool,
    pub chapter_exists: bool,
    pub has_audio: bool,
    pub blocked_on: &'static str,
}

/// Report what a re-render *would* target, without rendering anything.
///
/// The chapter lookup is real so that an obvious mistake (`litrpg render 400` when
/// chapter 400 does not exist) is caught now rather than after TTS lands.
// TODO(litrpg-tts): implement once `TtsBackend::render_batch` exists — re-render
// chapter N from its stored text, write NNNN.pcm/.mp3/.json per spec §8, then
// `Store::attach_audio`. Blocked on the `litrpg-tts` crate.
pub fn render(store: &Store, chapter: u32) -> Result<RenderStub> {
    let row = store.chapter(chapter).ok();
    Ok(RenderStub {
        chapter,
        implemented: false,
        chapter_exists: row.is_some(),
        has_audio: row.map(|r| r.has_audio).unwrap_or(false),
        blocked_on: "litrpg-tts",
    })
}

pub fn render_text(s: &RenderStub) -> String {
    let mut out = String::new();
    if !s.chapter_exists {
        out.push_str(&format!("Chapter {} does not exist.\n\n", s.chapter));
    }
    out.push_str(&format!(
        "render is not implemented yet — it needs the TTS plugin layer in the\n{} crate (spec §7), which is still being built.\n",
        s.blocked_on
    ));
    if s.chapter_exists {
        let state = if s.has_audio {
            "already has audio; a re-render would replace it"
        } else {
            "has no audio yet"
        };
        out.push_str(&format!("\nChapter {} {state}.\n", s.chapter));
    }
    out
}
