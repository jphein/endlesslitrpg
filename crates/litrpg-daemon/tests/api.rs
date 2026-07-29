//! JSON routes: health, version, story, chapters, state, character, notes.

mod common;

use axum::http::StatusCode;
use common::{
    CH1_DURATION_MS, assert_status, body_json, body_string, fixture, fixture_with_ledger,
    fixture_with_protagonist, fixture_with_story_row, header,
};

#[tokio::test]
async fn healthz_is_ok() {
    let f = fixture();
    let resp = f.get("/healthz").await;
    assert_status(&resp, StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

/// realm-sigil compatibility: every field of the Go `Version` struct must be present,
/// because status.realm.watch and the `<Sigil />` badge read them by name.
#[tokio::test]
async fn version_matches_realm_sigil_schema() {
    let f = fixture();
    let resp = f.get("/api/version").await;

    assert_status(&resp, StatusCode::OK);
    assert_eq!(header(&resp, "cache-control").as_deref(), Some("no-cache"));
    assert_eq!(
        header(&resp, "access-control-allow-origin").as_deref(),
        Some("*")
    );

    let v = body_json(resp).await;
    for field in [
        "name",
        "description",
        "version",
        "hash",
        "branch",
        "dirty",
        "built",
        "started",
        "uptime",
        "realm",
        "runtime",
        "os",
        "host",
        "pid",
        "repo",
        "commit_url",
    ] {
        assert!(
            v.get(field).is_some(),
            "missing /api/version field {field:?}"
        );
    }

    assert_eq!(v["name"], "litrpg-daemon");
    assert!(v["dirty"].is_boolean());
    assert!(v["uptime"].is_i64());
    assert!(v["pid"].is_u64());
    assert!(
        v["started"].as_str().unwrap().ends_with('Z'),
        "started must be RFC3339 UTC"
    );
    assert!(v["runtime"].as_str().unwrap().starts_with("rust"));
}

#[tokio::test]
async fn story_reports_counts_and_audio_constants() {
    let f = fixture();
    let v = body_json(f.get("/api/story").await).await;

    assert_eq!(v["title"], "Endless & Onward");
    assert_eq!(v["protagonist"], "Kael");
    assert_eq!(v["chapter_count"], 2);
    assert_eq!(v["latest_chapter"], 2);
    // Echoed from litrpg-core so no client hardcodes the 32 B/ms its Range math needs.
    assert_eq!(v["sample_rate"], 16000);
    assert_eq!(v["bytes_per_ms"], 32);
    assert_eq!(v["dirty_chapters"], serde_json::json!([2]));
}

/// No `story` row yet: config supplies the fallback and `initialised` says so.
#[tokio::test]
async fn story_falls_back_to_config_before_init() {
    let f = fixture();
    let v = body_json(f.get("/api/story").await).await;

    assert_eq!(v["initialised"], false);
    assert_eq!(v["title"], "Endless & Onward");
    assert_eq!(v["protagonist"], "Kael");
    assert!(v["target_words"].is_null());
    assert!(v["prompt_hash"].is_null());
}

/// Once a `story` row exists it is the authority — config must not shadow the
/// canonical record of what the story is.
#[tokio::test]
async fn story_table_outranks_config() {
    let f = fixture_with_story_row("Seryn", "ConfigFallback");
    let v = body_json(f.get("/api/story").await).await;

    assert_eq!(v["initialised"], true);
    assert_eq!(v["title"], "The Sunken Vale");
    assert_eq!(v["protagonist"], "Seryn");
    assert_ne!(v["title"], "Config Title (must lose)");
    assert_eq!(v["target_words"], 2500);
    assert_eq!(v["prompt_hash"], "storyhash1");
}

/// `/api/character` resolves the protagonist from the store, not config.
#[tokio::test]
async fn protagonist_route_prefers_the_story_table() {
    // The ledger describes "Kael"; the story row names "Kael" too, while config points
    // somewhere else — so reading config would produce a `known: false` body.
    let f = fixture_with_story_row("Kael", "SomeoneElse");
    let v = body_json(f.get("/api/character").await).await;

    assert_eq!(v["subject"], "Kael");
    assert_eq!(
        v["known"], true,
        "must resolve via the story table, not config"
    );
    assert_eq!(v["level"], 3);
}

/// A blank column must not shadow a usable configured fallback with an empty string.
#[tokio::test]
async fn blank_story_protagonist_falls_back_to_config() {
    let f = fixture_with_story_row("   ", "Kael");
    let v = body_json(f.get("/api/character").await).await;
    assert_eq!(v["subject"], "Kael");
    assert_eq!(v["known"], true);
}

#[tokio::test]
async fn chapter_index_lists_both_and_marks_audio() {
    let f = fixture();
    let v = body_json(f.get("/api/chapters").await).await;
    let items = v.as_array().expect("array");

    assert_eq!(items.len(), 2);

    assert_eq!(items[0]["number"], 1);
    assert_eq!(items[0]["has_audio"], true);
    assert_eq!(items[0]["duration_ms"], CH1_DURATION_MS);
    // duration_ms * 32 — the identity that holds because segments are padded to a
    // 32-byte boundary.
    assert_eq!(items[0]["total_bytes"], CH1_DURATION_MS as u64 * 32);
    assert_eq!(items[0]["pcm_url"], "http://10.0.6.107:8093/media/0001.pcm");
    assert!(items[0]["words"].as_u64().unwrap() > 0);

    assert_eq!(items[1]["number"], 2);
    assert_eq!(items[1]["has_audio"], false);
    // No audio means no URL to offer, rather than a URL that 404s.
    assert!(items[1]["pcm_url"].is_null());
    assert!(items[1]["total_bytes"].is_null());
    assert_eq!(items[1]["state_dirty"], true);
}

#[tokio::test]
async fn since_is_exclusive() {
    let f = fixture();

    let v = body_json(f.get("/api/chapters?since=1").await).await;
    let items = v.as_array().unwrap();
    assert_eq!(items.len(), 1, "since=1 must exclude chapter 1");
    assert_eq!(items[0]["number"], 2);

    let v = body_json(f.get("/api/chapters?since=2").await).await;
    assert!(v.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn chapter_detail_carries_text_and_manifest() {
    let f = fixture();
    let v = body_json(f.get("/api/chapters/1").await).await;

    assert_eq!(v["number"], 1);
    assert_eq!(v["prompt_hash"], "abc123");
    assert!(v["text_md"].as_str().unwrap().contains("iron and wet ash"));

    let m = &v["manifest"];
    assert_eq!(m["chapter"], 1);
    assert_eq!(m["sample_rate"], 16000);
    assert_eq!(m["bytes_per_ms"], 32);
    assert_eq!(m["duration_ms"], CH1_DURATION_MS);
    assert_eq!(v["manifest_contiguous"], true);

    let segs = m["segments"].as_array().expect("segments");
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[0]["speaker"], "narrator");
    assert_eq!(segs[0]["kind"], "narrator");
    assert_eq!(segs[0]["voice_ref"], "sherpa:piper-en_GB-cori:0");
    assert_eq!(segs[1]["speaker"], "Kael");
    assert_eq!(segs[1]["kind"], "character");
    assert_eq!(segs[2]["kind"], "system");
    // Segment ordering must follow idx — highlighting walks it in order.
    assert_eq!(segs[1]["start_ms"], 1000);
    assert_eq!(segs[1]["end_ms"], 2000);
}

#[tokio::test]
async fn missing_chapter_is_404() {
    let f = fixture();
    let resp = f.get("/api/chapters/999").await;
    assert_status(&resp, StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert!(v["error"].as_str().unwrap().contains("999"));
}

#[tokio::test]
async fn non_numeric_chapter_is_400() {
    let f = fixture();
    let resp = f.get("/api/chapters/abc").await;
    assert_status(&resp, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn state_nests_subject_and_field() {
    let f = fixture_with_ledger();
    let v = body_json(f.get("/api/state").await).await;

    let kael = &v["subjects"]["Kael"];
    // Flattened, not serde's externally-tagged `{"Num":3}`.
    assert_eq!(kael["level"], 3);
    assert_eq!(kael["hp"], 41);
    assert_eq!(kael["location"], "The Sunken Vale");
    assert_eq!(v["anomalies"], serde_json::json!([]));
}

#[tokio::test]
async fn character_exposes_all_slots_and_traits() {
    let f = fixture_with_ledger();
    let v = body_json(f.get("/api/character/Kael").await).await;

    assert_eq!(v["subject"], "Kael");
    assert_eq!(v["known"], true);
    assert_eq!(v["level"], 3);
    assert_eq!(v["xp"], 150);
    assert_eq!(v["hp"], 41);
    assert_eq!(v["max_hp"], 50);
    assert_eq!(v["gold"], 12);
    assert_eq!(v["location"], "The Sunken Vale");
    assert_eq!(v["status"], "wounded");
    assert_eq!(v["inventory"]["rope"], 2);

    // All eleven slots present regardless of whether they are filled — the watch
    // draws a fixed row per slot and must never infer layout.
    let equip = v["equipment"].as_object().expect("equipment object");
    assert_eq!(equip.len(), 11, "expected all 11 equip slots");
    for slot in [
        "head",
        "chest",
        "legs",
        "feet",
        "hands",
        "cloak",
        "main_hand",
        "off_hand",
        "amulet",
        "ring1",
        "ring2",
    ] {
        assert!(equip.contains_key(slot), "missing equip slot {slot:?}");
    }
    assert_eq!(equip["main_hand"], "Chipped Longsword");
    assert_eq!(equip["head"], "Iron Circlet");
    assert!(equip["ring1"].is_null(), "unset slot must be null");

    let appear = v["appearance"].as_object().expect("appearance object");
    assert_eq!(appear.len(), 6, "expected all 6 appearance traits");
    assert_eq!(appear["hair"], "black, cropped");
    assert!(appear["eyes"].is_null());
}

/// `/api/character` with no subject resolves to the protagonist, so the watch's
/// character screen need not already know whose story this is (spec §9.4.1).
#[tokio::test]
async fn character_without_subject_resolves_to_protagonist() {
    let f = fixture_with_protagonist("Kael");
    let resp = f.get("/api/character").await;
    assert_status(&resp, StatusCode::OK);

    let v = body_json(resp).await;
    assert_eq!(v["subject"], "Kael");
    assert_eq!(v["known"], true);
    assert_eq!(v["level"], 3);
}

/// The bare route and the explicit route must return byte-identical bodies, or the
/// watch would see different data depending on which URL it happened to use.
#[tokio::test]
async fn protagonist_route_matches_explicit_subject_route() {
    let f = fixture_with_protagonist("Kael");
    let implicit = body_json(f.get("/api/character").await).await;
    let explicit = body_json(f.get("/api/character/Kael").await).await;
    assert_eq!(implicit, explicit);
}

/// Whitespace in configuration must not become a request for the subject `"  "`.
#[tokio::test]
async fn protagonist_is_trimmed_before_lookup() {
    let f = fixture_with_protagonist("  Kael  ");
    let v = body_json(f.get("/api/character").await).await;
    assert_eq!(v["subject"], "Kael");
    assert_eq!(v["known"], true);
}

/// An unconfigured protagonist is a 400, not an empty 200: answering for the subject
/// `""` would render a blank screen that looks like a story with no protagonist rather
/// than a daemon that was never told who it is.
#[tokio::test]
async fn unconfigured_protagonist_is_a_clear_error() {
    let f = fixture_with_protagonist("");
    let resp = f.get("/api/character").await;
    assert_status(&resp, StatusCode::BAD_REQUEST);

    let v = body_json(resp).await;
    let msg = v["error"].as_str().unwrap();
    assert!(
        msg.contains("protagonist"),
        "error must name what is missing, got {msg:?}"
    );
    assert!(
        msg.contains("LITRPG_PROTAGONIST"),
        "error must say how to fix it, got {msg:?}"
    );
}

/// An unknown subject is a populated-but-empty 200, not a 404: the watch's screens are
/// a fixed layout and should not need an error path for "not introduced yet".
#[tokio::test]
async fn unknown_character_is_empty_not_404() {
    let f = fixture_with_ledger();
    let resp = f.get("/api/character/Nobody").await;
    assert_status(&resp, StatusCode::OK);

    let v = body_json(resp).await;
    assert_eq!(v["known"], false);
    assert!(v["level"].is_null());
    assert_eq!(v["equipment"].as_object().unwrap().len(), 11);
    assert_eq!(v["inventory"], serde_json::json!({}));
}

#[tokio::test]
async fn note_source_is_whitelisted() {
    let f = fixture();
    for bad in ["web", "", "CLI", "cli; drop"] {
        let resp = f
            .post_json(
                "/api/notes",
                serde_json::json!({"body": "introduce a rival", "source": bad}),
            )
            .await;
        assert_status(&resp, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn note_body_must_be_non_empty() {
    let f = fixture();
    for bad in ["", "   ", "\n\t "] {
        let resp = f
            .post_json(
                "/api/notes",
                serde_json::json!({"body": bad, "source": "cli"}),
            )
            .await;
        assert_status(&resp, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn oversized_note_is_rejected() {
    let f = fixture();
    let huge = "x".repeat(litrpg_daemon::notes::MAX_NOTE_BYTES + 1);
    let resp = f
        .post_json(
            "/api/notes",
            serde_json::json!({"body": huge, "source": "cli"}),
        )
        .await;
    assert_status(&resp, StatusCode::BAD_REQUEST);
}

/// Previously asserted a documented 501; `Store::insert_note` now exists, so a valid
/// note must actually persist and report `201 Created`.
#[tokio::test]
async fn valid_note_is_created() {
    let f = fixture();
    let resp = f
        .post_json(
            "/api/notes",
            serde_json::json!({"body": "  introduce a rival  ", "source": "watch"}),
        )
        .await;

    assert_status(&resp, StatusCode::CREATED);
    let v = body_json(resp).await;
    assert!(
        v["id"].as_i64().unwrap() > 0,
        "must return the stored row id"
    );
    // Echo what was persisted, not what was sent, so a client needn't guess about
    // whitespace.
    assert_eq!(v["body"], "introduce a rival");
    assert_eq!(v["source"], "watch");
}

/// Each accepted source must work, and ids must advance — proof the rows really land
/// rather than the handler returning a constant.
#[tokio::test]
async fn notes_persist_across_sources_with_distinct_ids() {
    let f = fixture();
    let mut ids = Vec::new();

    for source in ["cli", "watch", "candela"] {
        let resp = f
            .post_json(
                "/api/notes",
                serde_json::json!({"body": format!("note from {source}"), "source": source}),
            )
            .await;
        assert_status(&resp, StatusCode::CREATED);
        ids.push(body_json(resp).await["id"].as_i64().unwrap());
    }

    assert_eq!(ids.len(), 3);
    assert!(
        ids.windows(2).all(|w| w[1] > w[0]),
        "note ids must be distinct and increasing, got {ids:?}"
    );
}
