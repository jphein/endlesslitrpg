//! `/api/voices` — catalog composition and the three availability states.

mod common;

use axum::http::StatusCode;
use common::{StubBackend, assert_status, body_json, fixture, fixture_with_voices};
use litrpg_tts::TtsRegistry;
use litrpg_tts::backend::{Availability, CostClass, Gender, VoiceDesc};
use litrpg_tts::sherpa::SherpaConfig;

/// With an empty registry, the response is exactly the sherpa catalog — the
/// always-compiled `SherpaConfig` table.
#[tokio::test]
async fn sherpa_catalog_is_served_without_the_feature() {
    let f = fixture();
    let v = body_json(f.get("/api/voices").await).await;

    let backends = v["backends"].as_array().expect("backends");
    let sherpa = backends
        .iter()
        .find(|b| b["id"] == "sherpa")
        .expect("sherpa must be reported even when not compiled in");

    // The distinction the cast UI needs: present as a catalog, absent as a build.
    assert_eq!(sherpa["compiled_in"], false);
    assert_eq!(sherpa["assignable"], false);
    assert_eq!(sherpa["availability"]["state"], "missing");
    assert!(
        sherpa["availability"]["reason"].is_string(),
        "a missing backend must say why"
    );

    // Voices are still advertised so a deployment's intended cast is visible.
    assert!(
        sherpa["voice_count"].as_u64().unwrap() > 0,
        "config-derived sherpa catalog must not be empty"
    );
    let voices = v["voices"].as_array().unwrap();
    assert!(voices.iter().any(|x| x["backend"] == "sherpa"));
    assert!(
        voices
            .iter()
            .filter(|x| x["backend"] == "sherpa")
            .all(|x| x["assignable"] == false),
        "sherpa voices must be marked unassignable without the feature"
    );
}

/// Voices are local inference, so they must be advertised free — the UI warns before a
/// cast assignment starts billing.
#[tokio::test]
async fn sherpa_voices_are_free_and_fully_qualified() {
    let f = fixture();
    let v = body_json(f.get("/api/voices").await).await;

    for voice in v["voices"].as_array().unwrap() {
        if voice["backend"] != "sherpa" {
            continue;
        }
        assert_eq!(voice["cost_class"], "free");
        let vr = voice["voice_ref"].as_str().unwrap();
        assert!(
            vr.starts_with("sherpa:"),
            "voice_ref must be directly assignable, got {vr:?}"
        );
        assert!(!voice["label"].as_str().unwrap().is_empty());
        assert!(!voice["lang"].as_str().unwrap().is_empty());
    }
}

#[tokio::test]
async fn totals_agree_with_the_voice_list() {
    let f = fixture();
    let v = body_json(f.get("/api/voices").await).await;

    let voices = v["voices"].as_array().unwrap();
    assert_eq!(v["total"].as_u64().unwrap() as usize, voices.len());

    let assignable = voices.iter().filter(|x| x["assignable"] == true).count();
    assert_eq!(v["assignable_count"].as_u64().unwrap() as usize, assignable);

    // Per-backend counts must sum to the total, or a backend is dropping voices.
    let summed: u64 = v["backends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["voice_count"].as_u64().unwrap())
        .sum();
    assert_eq!(summed as usize, voices.len());
}

/// A registered, **ready** backend contributes assignable voices.
#[tokio::test]
async fn ready_backend_voices_are_assignable() {
    let registry = TtsRegistry::new().with(Box::new(StubBackend::new(
        "azure",
        Availability::Ready,
        vec![VoiceDesc {
            voice_ref: "azure:en-GB-Ada:DragonHDLatestNeural".to_string(),
            label: "Ada (DragonHD)".to_string(),
            lang: "en-GB".to_string(),
            gender: Gender::Female,
            cost_class: CostClass::Metered,
        }],
    )));

    let f = fixture_with_voices(registry, SherpaConfig::default());
    let v = body_json(f.get("/api/voices").await).await;

    let azure = v["backends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "azure")
        .expect("azure backend");

    assert_eq!(azure["compiled_in"], true);
    assert_eq!(azure["assignable"], true);
    assert_eq!(azure["availability"]["state"], "ready");

    let ada = v["voices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["voice_ref"] == "azure:en-GB-Ada:DragonHDLatestNeural")
        .expect("Ada voice");
    assert_eq!(ada["assignable"], true);
    assert_eq!(ada["cost_class"], "metered");
    // Backend split is on the FIRST colon — Azure voice names contain colons.
    assert_eq!(ada["backend"], "azure");
}

/// The middle state: compiled in, but unusable. Distinct from "not compiled in", which
/// is the whole point of carrying both flags.
#[tokio::test]
async fn compiled_but_unconfigured_backend_is_distinguishable() {
    let registry = TtsRegistry::new().with(Box::new(StubBackend::new(
        "azure",
        Availability::missing("no Azure subscription key resolved"),
        vec![VoiceDesc {
            voice_ref: "azure:en-US-Ava:DragonHDLatestNeural".to_string(),
            label: "Ava (DragonHD)".to_string(),
            lang: "en-US".to_string(),
            gender: Gender::Female,
            cost_class: CostClass::Metered,
        }],
    )));

    let f = fixture_with_voices(registry, SherpaConfig::default());
    let v = body_json(f.get("/api/voices").await).await;
    let backends = v["backends"].as_array().unwrap();

    let azure = backends.iter().find(|b| b["id"] == "azure").unwrap();
    let sherpa = backends.iter().find(|b| b["id"] == "sherpa").unwrap();

    // Both unusable, for categorically different reasons.
    assert_eq!(azure["assignable"], false);
    assert_eq!(sherpa["assignable"], false);
    assert_eq!(
        azure["compiled_in"], true,
        "azure is in the build; it just needs a key"
    );
    assert_eq!(
        sherpa["compiled_in"], false,
        "sherpa needs a rebuild, not a key"
    );
    assert!(
        azure["availability"]["reason"]
            .as_str()
            .unwrap()
            .contains("key"),
        "the reason must be actionable"
    );

    // Its catalog is still listed, but nothing is assignable.
    assert_eq!(v["assignable_count"], 0);
    assert!(v["total"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn voices_route_responds_ok() {
    let f = fixture();
    assert_status(&f.get("/api/voices").await, StatusCode::OK);
}
