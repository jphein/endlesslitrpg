//! Shared fixture: an in-memory store plus a temp media root.
//!
//! Cargo compiles this module into *each* integration-test binary independently, so a
//! helper used only by `media.rs` is genuinely dead code from `feed.rs`'s point of
//! view. The allow is about that compilation model, not about unused code.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_daemon::config::{Config, StoryConfig};
use litrpg_daemon::{AppState, router};
use litrpg_store::{NewChapter, Store};
use litrpg_tts::backend::{Availability, RenderRequest, TtsBackend, VoiceDesc};
use litrpg_tts::pcm::Pcm16k;
use litrpg_tts::sherpa::SherpaConfig;
use litrpg_tts::{TtsError, TtsRegistry, async_trait};
use tempfile::TempDir;
use tower::ServiceExt;

/// A `TtsBackend` with a fixed id, availability and voice list.
///
/// Defined here rather than in `litrpg-tts` because that crate belongs to another
/// agent. `/api/voices` only ever reads `id`/`available`/`voices`, so `render` is
/// unreachable for these tests and returns an error rather than fabricating audio —
/// a stub that silently produced PCM could let a real bug pass.
pub struct StubBackend {
    id: String,
    availability: Availability,
    voices: Vec<VoiceDesc>,
}

impl StubBackend {
    pub fn new(id: &str, availability: Availability, voices: Vec<VoiceDesc>) -> Self {
        Self {
            id: id.to_string(),
            availability,
            voices,
        }
    }
}

#[async_trait]
impl TtsBackend for StubBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn available(&self) -> Availability {
        self.availability.clone()
    }

    fn voices(&self) -> Vec<VoiceDesc> {
        self.voices.clone()
    }

    async fn render(&self, _req: &RenderRequest) -> Result<Pcm16k, TtsError> {
        Err(TtsError::BackendUnavailable {
            id: self.id.clone(),
            reason: "StubBackend does not synthesize".to_string(),
        })
    }
}

/// Chapter 1's synthetic duration. Three 1000 ms segments.
pub const CH1_DURATION_MS: u32 = 3000;
/// 3000 ms x 32 B/ms. The identity the watch's Range arithmetic depends on.
pub const CH1_PCM_LEN: u64 = CH1_DURATION_MS as u64 * 32;
pub const CH1_MP3_LEN: u64 = 512;

pub struct Fixture {
    pub app: Router,
    /// Held so the temp dir outlives the test.
    pub _media: TempDir,
}

fn segment(idx: u32, speaker: &str, kind: SpeakerKind, voice: &str, start: u32) -> Segment {
    Segment {
        idx,
        speaker: speaker.to_string(),
        kind,
        voice_ref: voice.to_string(),
        text: format!("Segment {idx} text."),
        start_ms: start,
        end_ms: start + 1000,
    }
}

/// Build a fixture with:
/// * chapter 1 — audio attached, `NNNN.pcm` (96000 B) and `NNNN.mp3` (512 B) on disk
/// * chapter 2 — text only, no audio, no media files
///
/// A title containing `&` and `'` so the RSS escaping test has real input rather than
/// a hypothetical.
pub fn fixture() -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let store = Store::open_in_memory().expect("store");

    store
        .insert_chapter(&NewChapter {
            number: 1,
            title: "Iron & Ash <the> \"Vale's\" Edge".to_string(),
            text_md: "The vale smelled of iron and wet ash. Kael counted the turns.".to_string(),
            prompt_hash: "abc123".to_string(),
            state_dirty: false,
        })
        .expect("insert ch1");

    let manifest = Manifest::new(
        1,
        vec![
            segment(
                0,
                "narrator",
                SpeakerKind::Narrator,
                "sherpa:piper-en_GB-cori:0",
                0,
            ),
            segment(
                1,
                "Kael",
                SpeakerKind::Character,
                "sherpa:kokoro-multi-lang-v1_0:18",
                1000,
            ),
            segment(
                2,
                "SYSTEM",
                SpeakerKind::System,
                "sherpa:kokoro-multi-lang-v1_0:11",
                2000,
            ),
        ],
    );
    assert_eq!(manifest.duration_ms, CH1_DURATION_MS);
    assert_eq!(manifest.total_bytes(), CH1_PCM_LEN);

    store
        // Migration 004 dropped `pcm_path`/`mp3_path`: the filesystem is the authority,
        // and `serve_media` derives `media_root/{n:04}.{ext}` itself.
        .attach_audio(1, &manifest)
        .expect("attach audio");

    store
        .insert_chapter(&NewChapter {
            number: 2,
            title: "No Audio Yet".to_string(),
            text_md: "Text ships even when rendering fails.".to_string(),
            prompt_hash: "def456".to_string(),
            state_dirty: true,
        })
        .expect("insert ch2");

    // Byte i = i % 256, so a Range response can be checked for *correct offset*, not
    // merely correct length — a seek bug that returns the right count of wrong bytes
    // would otherwise pass.
    let pcm: Vec<u8> = (0..CH1_PCM_LEN).map(|i| (i % 256) as u8).collect();
    std::fs::write(media.path().join("0001.pcm"), &pcm).expect("write pcm");
    std::fs::write(
        media.path().join("0001.mp3"),
        vec![0xFFu8; CH1_MP3_LEN as usize],
    )
    .expect("write mp3");

    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig {
        title: "Endless & Onward".to_string(),
        description: "A <serial>".to_string(),
        protagonist: "Kael".to_string(),
        base_url: "http://10.0.6.107:8093".to_string(),
        language: "en-us".to_string(),
    });

    let app = router(Arc::new(AppState::new(store, cfg)));
    Fixture { app, _media: media }
}

/// An in-memory store seeded with one fully-described character.
///
/// Shared by `fixture_with_ledger` and `fixture_with_protagonist` so the two cannot
/// drift — a `/api/character/Kael` test and a `/api/character` test must agree about
/// what Kael looks like or neither proves anything.
pub fn seed_ledger_store() -> Store {
    use litrpg_core::ledger::Op;
    use litrpg_core::validate::Delta;

    let store = Store::open_in_memory().expect("store");

    // Establishes "Kael" as a known subject; the gate rejects deltas for unknown ones.
    store
        .upsert_cast("Kael", "sherpa:kokoro-multi-lang-v1_0:18", "character", 1)
        .expect("cast");

    let d = |field: &str, op: Op, num: Option<i64>, txt: Option<&str>| Delta {
        subject: "Kael".to_string(),
        field: field.to_string(),
        op,
        value_num: num,
        value_txt: txt.map(|s| s.to_string()),
    };

    for delta in [
        d("level", Op::Set, Some(3), None),
        d("xp", Op::Set, Some(150), None),
        d("max_hp", Op::Set, Some(50), None),
        d("hp", Op::Set, Some(41), None),
        d("gold", Op::Set, Some(12), None),
        d("location", Op::Set, None, Some("The Sunken Vale")),
        d("status", Op::Set, None, Some("wounded")),
        d("inv:rope", Op::Set, Some(2), None),
        d("equip:main_hand", Op::Set, None, Some("Chipped Longsword")),
        d("equip:head", Op::Set, None, Some("Iron Circlet")),
        d("appear:hair", Op::Set, None, Some("black, cropped")),
    ] {
        let verdict = store.append_delta(1, &delta).expect("append");
        assert!(
            verdict.is_ok(),
            "delta {:?} rejected: {verdict:?}",
            delta.field
        );
    }

    store
}

/// Ledger fixture with a caller-supplied TTS registry, so cast renderability can be
/// tested against a known backend set.
pub fn fixture_ledger_with_registry(registry: TtsRegistry) -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig {
        protagonist: "Kael".to_string(),
        ..StoryConfig::default()
    });
    let app = router(Arc::new(
        AppState::new(seed_ledger_store(), cfg).with_tts(registry),
    ));
    Fixture { app, _media: media }
}

/// A store with ledger entries, for the state/character routes.
pub fn fixture_with_ledger() -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig {
        protagonist: "Kael".to_string(),
        ..StoryConfig::default()
    });

    let app = router(Arc::new(AppState::new(seed_ledger_store(), cfg)));
    Fixture { app, _media: media }
}

/// Fixture with a caller-supplied TTS registry and sherpa config, for `/api/voices`.
pub fn fixture_with_voices(registry: TtsRegistry, sherpa: SherpaConfig) -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let store = Store::open_in_memory().expect("store");
    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    );
    let app = router(Arc::new(
        AppState::new(store, cfg)
            .with_tts(registry)
            .with_sherpa(sherpa),
    ));
    Fixture { app, _media: media }
}

/// Fixture with a ledger plus a **`story` table row**, for testing that the store
/// outranks config. `config_protagonist` is the fallback that must lose.
pub fn fixture_with_story_row(story_protagonist: &str, config_protagonist: &str) -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let store = seed_ledger_store();
    store
        .upsert_story(&litrpg_store::NewStory {
            title: "The Sunken Vale".to_string(),
            protagonist: story_protagonist.to_string(),
            prompt_path: "story/prompt.md".to_string(),
            prompt_hash: "storyhash1".to_string(),
            target_words: 2500,
        })
        .expect("upsert story");

    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig {
        title: "Config Title (must lose)".to_string(),
        protagonist: config_protagonist.to_string(),
        ..StoryConfig::default()
    });

    let app = router(Arc::new(AppState::new(store, cfg)));
    Fixture { app, _media: media }
}

/// Fixture with a ledger and an explicit configured protagonist, for `/api/character`.
///
/// Pass `""` to model a daemon that was never told whose story this is.
pub fn fixture_with_protagonist(protagonist: &str) -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig {
        protagonist: protagonist.to_string(),
        ..StoryConfig::default()
    });
    let app = router(Arc::new(AppState::new(seed_ledger_store(), cfg)));
    Fixture { app, _media: media }
}

/// Fixture with a story row and `total` chapters, the first `with_audio` of which have
/// rendered audio. For the playback cursor.
///
/// Durations differ per chapter so a wrong-chapter bug cannot hide behind identical
/// numbers.
pub fn fixture_progress(total: u32, with_audio: u32) -> Fixture {
    let media = TempDir::new().expect("temp media dir");
    let store = Store::open_in_memory().expect("store");

    store
        .upsert_story(&litrpg_store::NewStory {
            title: "The Sunken Vale".to_string(),
            protagonist: "Kael".to_string(),
            prompt_path: "story/prompt.md".to_string(),
            prompt_hash: "cursorhash".to_string(),
            target_words: 2000,
        })
        .expect("upsert story");

    for n in 1..=total {
        store
            .insert_chapter(&NewChapter {
                number: n,
                title: format!("Chapter {n}"),
                text_md: format!("Body of chapter {n}."),
                prompt_hash: "cursorhash".to_string(),
                state_dirty: false,
            })
            .expect("insert chapter");

        if n <= with_audio {
            // One segment, 1000 ms * n, so durations are distinguishable.
            let manifest = Manifest::new(
                n,
                vec![Segment {
                    idx: 0,
                    speaker: "narrator".to_string(),
                    kind: SpeakerKind::Narrator,
                    voice_ref: "sherpa:piper-en_GB-cori:0".to_string(),
                    text: format!("Chapter {n} narration."),
                    start_ms: 0,
                    end_ms: 1000 * n,
                }],
            );
            store.attach_audio(n, &manifest).expect("attach audio");
            std::fs::write(
                media.path().join(format!("{n:04}.pcm")),
                vec![0u8; manifest.total_bytes() as usize],
            )
            .expect("write pcm");
            std::fs::write(media.path().join(format!("{n:04}.mp3")), vec![0xFFu8; 256])
                .expect("write mp3");
        }
    }

    let cfg = Config::new(
        "127.0.0.1:8093".parse::<SocketAddr>().unwrap(),
        media.path(),
    )
    .with_story(StoryConfig {
        base_url: "http://10.0.6.107:8093".to_string(),
        protagonist: "Kael".to_string(),
        ..StoryConfig::default()
    });

    let app = router(Arc::new(AppState::new(store, cfg)));
    Fixture { app, _media: media }
}

impl Fixture {
    /// The temp media root, so a test can place an extra artifact (e.g. a
    /// five-digit chapter) without a bespoke fixture.
    pub fn media_dir(&self) -> &std::path::Path {
        self._media.path()
    }

    pub async fn get(&self, uri: &str) -> Response<Body> {
        self.request(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
    }

    /// GET with a `Range` header.
    pub async fn get_range(&self, uri: &str, range: &str) -> Response<Body> {
        self.request(
            Request::builder()
                .uri(uri)
                .header("Range", range)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn post_json(&self, uri: &str, body: serde_json::Value) -> Response<Body> {
        self.json_request("POST", uri, body).await
    }

    pub async fn put_json(&self, uri: &str, body: serde_json::Value) -> Response<Body> {
        self.json_request("PUT", uri, body).await
    }

    async fn json_request(
        &self,
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> Response<Body> {
        self.request(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    pub async fn request(&self, req: Request<Body>) -> Response<Body> {
        self.app
            .clone()
            .oneshot(req)
            .await
            .expect("router response")
    }
}

pub async fn body_bytes(resp: Response<Body>) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec()
}

pub async fn body_string(resp: Response<Body>) -> String {
    String::from_utf8(body_bytes(resp).await).expect("utf8 body")
}

pub async fn body_json(resp: Response<Body>) -> serde_json::Value {
    serde_json::from_slice(&body_bytes(resp).await).expect("json body")
}

pub fn header(resp: &Response<Body>, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

pub fn assert_status(resp: &Response<Body>, want: StatusCode) {
    assert_eq!(resp.status(), want, "unexpected status");
}
