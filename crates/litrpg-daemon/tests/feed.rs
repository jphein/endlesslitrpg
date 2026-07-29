//! `/feed.xml` — RSS 2.0 shape, escaping, and enclosure correctness.

mod common;

use axum::http::StatusCode;
use common::{CH1_MP3_LEN, assert_status, body_string, fixture, header};
use tower::ServiceExt;

#[tokio::test]
async fn feed_is_rss_with_correct_mime() {
    let f = fixture();
    let resp = f.get("/feed.xml").await;

    assert_status(&resp, StatusCode::OK);
    assert_eq!(
        header(&resp, "content-type").as_deref(),
        Some("application/rss+xml; charset=utf-8")
    );

    let xml = body_string(resp).await;
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(xml.contains("<rss version=\"2.0\""));
    assert!(xml.contains("<channel>") && xml.contains("</channel>"));
    assert!(xml.trim_end().ends_with("</rss>"));
}

#[tokio::test]
async fn channel_carries_required_elements() {
    let f = fixture();
    let xml = body_string(f.get("/feed.xml").await).await;

    // RSS 2.0 requires title, link and description on the channel.
    assert!(xml.contains("<title>Endless &amp; Onward</title>"));
    assert!(xml.contains("<link>http://10.0.6.107:8093</link>"));
    assert!(xml.contains("<description>A &lt;serial&gt;</description>"));
    assert!(xml.contains("<language>en-us</language>"));
    assert!(xml.contains("rel=\"self\""));
}

#[tokio::test]
async fn enclosure_url_length_and_type_are_correct() {
    let f = fixture();
    let xml = body_string(f.get("/feed.xml").await).await;

    let expected = format!(
        "<enclosure url=\"http://10.0.6.107:8093/media/0001.mp3\" length=\"{CH1_MP3_LEN}\" type=\"audio/mpeg\"/>"
    );
    assert!(
        xml.contains(&expected),
        "missing or wrong enclosure.\nwant: {expected}\ngot:\n{xml}"
    );
}

/// Only chapter 1 has audio; chapter 2 must not appear. An enclosure pointing at a
/// missing file makes podcast clients retry forever.
#[tokio::test]
async fn only_chapters_with_audio_become_items() {
    let f = fixture();
    let xml = body_string(f.get("/feed.xml").await).await;

    assert_eq!(xml.matches("<item>").count(), 1);
    assert_eq!(xml.matches("</item>").count(), 1);
    assert!(xml.contains("chapter-1"));
    assert!(!xml.contains("No Audio Yet"));
    assert!(!xml.contains("0002.mp3"));
}

/// Chapter titles are model-generated, so `&`, `<`, `>` and quotes are expected input.
/// Unescaped, any one of them yields a document no reader will parse.
#[tokio::test]
async fn model_generated_titles_are_xml_escaped() {
    let f = fixture();
    let xml = body_string(f.get("/feed.xml").await).await;

    // Fixture title: `Iron & Ash <the> "Vale's" Edge`
    assert!(xml.contains("Iron &amp; Ash &lt;the&gt;"));
    assert!(xml.contains("&quot;Vale&apos;s&quot;"));

    // No raw markup leaked from the title into the document.
    assert!(!xml.contains("<the>"));
    assert!(!xml.contains("Iron & Ash"));
}

#[tokio::test]
async fn item_has_guid_pubdate_and_duration() {
    let f = fixture();
    let xml = body_string(f.get("/feed.xml").await).await;

    assert!(xml.contains("<guid isPermaLink=\"false\">chapter-1</guid>"));
    // 3000 ms -> 3 s
    assert!(xml.contains("<itunes:duration>3</itunes:duration>"));
    assert!(xml.contains("<pubDate>"));
    // RFC 822 date, as RSS 2.0 requires.
    assert!(
        xml.contains(" GMT</pubDate>"),
        "pubDate must be an RFC 822 GMT timestamp:\n{xml}"
    );
}

/// Every `<` that opens a tag must have a matching close, and no stray raw `&` may
/// survive. A cheap structural check that catches escaping regressions.
#[tokio::test]
async fn feed_is_well_formed_enough_to_parse() {
    let f = fixture();
    let xml = body_string(f.get("/feed.xml").await).await;

    assert_eq!(
        xml.matches("<title>").count(),
        xml.matches("</title>").count()
    );
    assert_eq!(
        xml.matches("<channel>").count(),
        xml.matches("</channel>").count()
    );

    // Any '&' must begin a valid entity reference.
    for (i, _) in xml.match_indices('&') {
        let tail = &xml[i..];
        assert!(
            ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"]
                .iter()
                .any(|e| tail.starts_with(e)),
            "raw '&' at byte {i} is not an entity: {:?}",
            &tail[..tail.len().min(12)]
        );
    }
}

#[tokio::test]
async fn empty_story_still_produces_valid_rss() {
    // A feed with no items must still be a parseable document, not an empty body.
    use litrpg_daemon::config::{Config, StoryConfig};
    use litrpg_daemon::{AppState, router};
    use std::sync::Arc;

    let media = tempfile::TempDir::new().unwrap();
    let store = litrpg_store::Store::open_in_memory().unwrap();
    let cfg = Config::new(
        "127.0.0.1:8093".parse::<std::net::SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig::default());

    let app = router(Arc::new(AppState::new(store, cfg)));
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/feed.xml")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_status(&resp, StatusCode::OK);
    let xml = body_string(resp).await;
    assert!(xml.contains("<channel>"));
    assert!(!xml.contains("<item>"));
    assert!(xml.trim_end().ends_with("</rss>"));
}
