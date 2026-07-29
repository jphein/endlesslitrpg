//! `GET /feed.xml` — RSS 2.0 with MP3 enclosures.
//!
//! Hand-built rather than templated: the document is small, and an XML writer
//! dependency would still leave the escaping decisions below to be made explicitly.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use litrpg_core::mp3_name;

use crate::AppState;
use crate::datetime::rfc2822_utc;
use crate::error::ApiResult;

/// Escape text for an XML **text node or attribute value**.
///
/// All five predefined entities are escaped, not just the three that text nodes
/// strictly require: the same helper fills attribute values, where an unescaped quote
/// terminates the attribute early. Chapter titles are model-generated, so an
/// apostrophe or ampersand in one is expected input, not a hypothetical.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids most C0 controls outright — they cannot be escaped,
            // only dropped, so drop them rather than emit a document that will not parse.
            c if (c as u32) < 0x20 && c != '\n' && c != '\r' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// `GET /feed.xml`
///
/// Only chapters with audio become items — an enclosure pointing at a missing file
/// makes a podcast client retry forever.
pub async fn get_feed(State(state): State<Arc<AppState>>) -> ApiResult<impl IntoResponse> {
    let store = state.store.lock().await;
    let rows = store.chapters_since(0)?;
    drop(store);

    let cfg = &state.config.story;
    let base = cfg.base_url.trim_end_matches('/');

    let mut items = String::new();
    // Newest first: readers present items in document order.
    for c in rows.iter().filter(|c| c.has_audio).rev() {
        let path = state.config.media_root.join(mp3_name(c.number));

        // `enclosure/@length` is a byte count, so it must come from the file rather
        // than an estimate; clients use it for progress and range planning.
        let len = match tokio::fs::metadata(&path).await {
            Ok(m) => m.len(),
            // Flagged `has_audio` but the file is gone — `.pcm`/`.mp3` are pruned
            // outside the buffer window (spec §8). Skip rather than advertise a 404.
            Err(_) => continue,
        };

        // `pubDate` from `chapters.created_at` (Unix **ms**), not the MP3's mtime: a
        // re-render for a cast change rewrites the file and would otherwise republish
        // an old chapter to the top of every subscriber's feed.
        let published_secs = c.created_at / 1000;

        let url = format!("{base}/media/{}", mp3_name(c.number));
        let title = xml_escape(&format!("Chapter {}: {}", c.number, c.title));
        let link = xml_escape(&format!("{base}/api/chapters/{}", c.number));

        items.push_str(&format!(
            "    <item>\n\
             \x20     <title>{title}</title>\n\
             \x20     <link>{link}</link>\n\
             \x20     <guid isPermaLink=\"false\">chapter-{number}</guid>\n\
             \x20     <pubDate>{pub_date}</pubDate>\n\
             \x20     <itunes:duration>{duration}</itunes:duration>\n\
             \x20     <enclosure url=\"{url}\" length=\"{len}\" type=\"audio/mpeg\"/>\n\
             \x20   </item>\n",
            title = title,
            link = link,
            number = c.number,
            pub_date = rfc2822_utc(published_secs),
            duration = c.duration_ms / 1000,
            url = xml_escape(&url),
            len = len,
        ));
    }

    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" \
         xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\">\n\
         \x20 <channel>\n\
         \x20   <title>{title}</title>\n\
         \x20   <link>{link}</link>\n\
         \x20   <description>{description}</description>\n\
         \x20   <language>{language}</language>\n\
         \x20   <atom:link href=\"{self_link}\" rel=\"self\" type=\"application/rss+xml\"/>\n\
         {items}\
         \x20 </channel>\n\
         </rss>\n",
        title = xml_escape(&cfg.title),
        link = xml_escape(base),
        description = xml_escape(&cfg.description),
        language = xml_escape(&cfg.language),
        self_link = xml_escape(&format!("{base}/feed.xml")),
        items = items,
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        "application/rss+xml; charset=utf-8".parse().unwrap(),
    );
    Ok((StatusCode::OK, headers, xml))
}
