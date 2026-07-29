//! Registry dispatch, tested against a mock backend — no network, no models.

use litrpg_core::SpeakerKind;
use litrpg_tts::{
    Availability, CostClass, Gender, Pcm16k, RenderRequest, TtsBackend, TtsError, TtsRegistry,
    VoiceDesc, async_trait,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A backend that renders `ms_per_char` of silence per character. It deliberately
/// does **not** override `render_batch`, so the trait's default loop is what runs
/// — that default is itself under test here.
struct Mock {
    id: &'static str,
    availability: Availability,
    ms_per_char: u32,
    batch_calls: Arc<AtomicUsize>,
    render_calls: Arc<AtomicUsize>,
}

impl Mock {
    fn ready(id: &'static str, ms_per_char: u32) -> Self {
        Self {
            id,
            availability: Availability::Ready,
            ms_per_char,
            batch_calls: Arc::new(AtomicUsize::new(0)),
            render_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn unavailable(id: &'static str, reason: &str) -> Self {
        Self {
            availability: Availability::missing(reason),
            ..Self::ready(id, 1)
        }
    }
}

#[async_trait]
impl TtsBackend for Mock {
    fn id(&self) -> &str {
        self.id
    }

    fn available(&self) -> Availability {
        self.availability.clone()
    }

    fn voices(&self) -> Vec<VoiceDesc> {
        vec![
            VoiceDesc {
                voice_ref: format!("{}:voice-a", self.id),
                label: format!("{} A", self.id),
                lang: "en-GB".into(),
                gender: Gender::Female,
                cost_class: CostClass::Free,
            },
            VoiceDesc {
                voice_ref: format!("{}:voice-b", self.id),
                label: format!("{} B", self.id),
                lang: "en-US".into(),
                gender: Gender::Male,
                cost_class: CostClass::Metered,
            },
        ]
    }

    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k, TtsError> {
        self.render_calls.fetch_add(1, Ordering::SeqCst);
        if req.voice.backend != self.id {
            return Err(TtsError::UnknownBackend(req.voice.backend.clone()));
        }
        Ok(Pcm16k::silence_ms(
            req.text.chars().count() as u32 * self.ms_per_char,
        ))
    }
}

/// A backend that *does* override `render_batch`, standing in for Azure's single
/// multi-voice request and sherpa's worker pool.
struct BatchingMock {
    batch_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl TtsBackend for BatchingMock {
    fn id(&self) -> &str {
        "batching"
    }
    fn available(&self) -> Availability {
        Availability::Ready
    }
    fn voices(&self) -> Vec<VoiceDesc> {
        vec![]
    }
    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k, TtsError> {
        Ok(Pcm16k::silence_ms(req.text.chars().count() as u32))
    }
    async fn render_batch(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>, TtsError> {
        self.batch_calls.fetch_add(1, Ordering::SeqCst);
        Ok(reqs
            .iter()
            .map(|r| Pcm16k::silence_ms(r.text.chars().count() as u32))
            .collect())
    }
}

fn req(idx: u32, voice_ref: &str, text: &str) -> RenderRequest {
    RenderRequest::parse(idx, voice_ref, text, SpeakerKind::Character).unwrap()
}

// ------------------------------------------------------------------ resolution

#[test]
fn resolves_a_voice_ref_to_its_owning_backend() {
    let reg = TtsRegistry::new()
        .with(Box::new(Mock::ready("sherpa", 1)))
        .with(Box::new(Mock::ready("azure", 2)));

    let b = reg.resolve("sherpa:kokoro-multi-lang-v1_0:18").unwrap();
    assert_eq!(b.id(), "sherpa");

    // First colon only — Azure remainders contain colons.
    let b = reg.resolve("azure:en-GB-Ada:DragonHDLatestNeural").unwrap();
    assert_eq!(b.id(), "azure");
}

#[test]
fn unknown_backend_id_is_a_typed_error() {
    let reg = TtsRegistry::new().with(Box::new(Mock::ready("sherpa", 1)));
    match reg.resolve("elevenlabs:rachel").map(|b| b.id().to_string()) {
        Err(TtsError::UnknownBackend(id)) => assert_eq!(id, "elevenlabs"),
        other => panic!("expected UnknownBackend, got {other:?}"),
    }
}

#[test]
fn unavailable_backend_is_a_distinct_typed_error() {
    let reg = TtsRegistry::new().with(Box::new(Mock::unavailable(
        "sherpa",
        "model root /models not found",
    )));
    match reg
        .resolve("sherpa:piper-en_GB-cori:0")
        .map(|b| b.id().to_string())
    {
        Err(TtsError::BackendUnavailable { id, reason }) => {
            assert_eq!(id, "sherpa");
            assert!(reason.contains("/models"), "reason preserved: {reason}");
        }
        other => panic!("expected BackendUnavailable, got {other:?}"),
    }
}

#[test]
fn malformed_voice_ref_is_rejected_before_dispatch() {
    let reg = TtsRegistry::new().with(Box::new(Mock::ready("sherpa", 1)));
    assert!(matches!(
        reg.resolve("no-colon-here").map(|b| b.id().to_string()),
        Err(TtsError::VoiceRef(_))
    ));
}

#[test]
fn duplicate_backend_ids_are_refused() {
    let mut reg = TtsRegistry::new().with(Box::new(Mock::ready("sherpa", 1)));
    match reg.register(Box::new(Mock::ready("sherpa", 9))) {
        Err(TtsError::DuplicateBackend(id)) => assert_eq!(id, "sherpa"),
        other => panic!("expected DuplicateBackend, got {other:?}"),
    }
}

// --------------------------------------------------------------- voice catalog

#[test]
fn all_voices_aggregates_across_plugins_in_registration_order() {
    let reg = TtsRegistry::new()
        .with(Box::new(Mock::ready("sherpa", 1)))
        .with(Box::new(Mock::ready("azure", 1)));

    let voices = reg.all_voices();
    assert_eq!(voices.len(), 4);
    let refs: Vec<&str> = voices.iter().map(|v| v.voice_ref.as_str()).collect();
    assert_eq!(
        refs,
        vec![
            "sherpa:voice-a",
            "sherpa:voice-b",
            "azure:voice-a",
            "azure:voice-b"
        ]
    );
    // Cost class is what lets the daemon warn before spending Azure quota.
    assert_eq!(voices[1].cost_class, CostClass::Metered);
}

#[test]
fn all_voices_skips_unavailable_backends() {
    let reg = TtsRegistry::new()
        .with(Box::new(Mock::ready("sherpa", 1)))
        .with(Box::new(Mock::unavailable("azure", "no key")));
    let voices = reg.all_voices();
    assert_eq!(voices.len(), 2);
    assert!(voices.iter().all(|v| v.voice_ref.starts_with("sherpa:")));
}

#[test]
fn availability_report_lists_every_backend_including_broken_ones() {
    let reg = TtsRegistry::new()
        .with(Box::new(Mock::ready("sherpa", 1)))
        .with(Box::new(Mock::unavailable("azure", "no key")));
    let report = reg.availability();
    assert_eq!(report.len(), 2);
    assert!(report[0].1.is_ready());
    assert!(!report[1].1.is_ready());
}

// -------------------------------------------------------------------- rendering

#[tokio::test]
async fn registry_render_dispatches_to_the_owning_backend() {
    let reg = TtsRegistry::new()
        .with(Box::new(Mock::ready("sherpa", 10)))
        .with(Box::new(Mock::ready("azure", 100)));

    let a = reg.render(&req(0, "sherpa:cori:0", "abcd")).await.unwrap();
    let b = reg
        .render(&req(1, "azure:en-GB-Ada:DragonHDLatestNeural", "abcd"))
        .await
        .unwrap();

    assert_eq!(a.duration_ms(), 40);
    assert_eq!(b.duration_ms(), 400);
}

#[tokio::test]
async fn default_render_batch_loops_over_render() {
    let mock = Mock::ready("sherpa", 10);
    let calls = mock.render_calls.clone();
    let batches = mock.batch_calls.clone();

    let reqs = vec![
        req(0, "sherpa:cori:0", "ab"),
        req(1, "sherpa:cori:0", "abc"),
        req(2, "sherpa:cori:0", ""),
    ];
    let out = mock.render_batch(&reqs).await.unwrap();

    assert_eq!(out.len(), 3);
    assert_eq!(out[0].duration_ms(), 20);
    assert_eq!(out[1].duration_ms(), 30);
    assert_eq!(out[2].duration_ms(), 0);
    // The default implementation is a plain loop over `render`.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(batches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_overriding_backend_gets_one_batch_call_not_n_renders() {
    let counter = Arc::new(AtomicUsize::new(0));
    let reg = TtsRegistry::new().with(Box::new(BatchingMock {
        batch_calls: counter.clone(),
    }));

    let reqs: Vec<RenderRequest> = (0..5).map(|i| req(i, "batching:v", "hello")).collect();
    let out = reg.render_all(&reqs).await.unwrap();

    assert_eq!(out.len(), 5);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "the whole point of render_batch: one call for the whole shard"
    );
}

#[tokio::test]
async fn render_all_groups_by_backend_and_restores_original_order() {
    let reg = TtsRegistry::new()
        .with(Box::new(Mock::ready("sherpa", 10)))
        .with(Box::new(Mock::ready("azure", 100)));

    // Interleaved chapter: narrator, guest, narrator, guest, narrator.
    let reqs = vec![
        req(0, "sherpa:cori:0", "a"),
        req(1, "azure:en-GB-Ada:DragonHDLatestNeural", "a"),
        req(2, "sherpa:cori:0", "aa"),
        req(3, "azure:en-GB-Ada:DragonHDLatestNeural", "aa"),
        req(4, "sherpa:cori:0", "aaa"),
    ];
    let out = reg.render_all(&reqs).await.unwrap();

    assert_eq!(
        out.iter().map(|p| p.duration_ms()).collect::<Vec<_>>(),
        vec![10, 100, 20, 200, 30],
        "results must come back in request order, not backend-grouped order"
    );
}

#[tokio::test]
async fn render_all_fails_loudly_on_an_unknown_backend() {
    let reg = TtsRegistry::new().with(Box::new(Mock::ready("sherpa", 10)));
    let reqs = vec![req(0, "sherpa:cori:0", "a"), req(1, "nope:v", "a")];
    assert!(matches!(
        reg.render_all(&reqs).await,
        Err(TtsError::UnknownBackend(_))
    ));
}

#[tokio::test]
async fn render_all_of_nothing_is_nothing() {
    let reg = TtsRegistry::new().with(Box::new(Mock::ready("sherpa", 10)));
    assert!(reg.render_all(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn render_joined_defaults_to_concatenating_the_batch() {
    let mock = Mock::ready("sherpa", 10);
    let reqs = vec![
        req(0, "sherpa:cori:0", "ab"),
        req(1, "sherpa:cori:0", "abc"),
    ];
    let joined = mock.render_joined(&reqs).await.unwrap();
    assert_eq!(joined.duration_ms(), 50);
    assert_eq!(joined.len(), 50 * 32);
}
