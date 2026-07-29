//! A chapter may interleave sherpa and Azure segments. Because every plugin
//! normalizes to 16 kHz mono s16le at the boundary (spec §7.1), concatenation is
//! `Vec::extend` and the manifest's byte offsets address one continuous stream.

use litrpg_core::{BYTES_PER_MS, Manifest, SAMPLE_RATE_HZ, Segment, SpeakerKind};
use litrpg_tts::{
    Availability, Pcm16k, RenderRequest, TtsBackend, TtsError, TtsRegistry, VoiceDesc, async_trait,
};

/// Stands in for sherpa: a native-rate engine whose renders land on arbitrary
/// sample counts (22 050 / 24 000 Hz resampled to 16 kHz never divides evenly
/// into whole milliseconds), so it returns non-whole-ms buffers.
struct FakeSherpa;

#[async_trait]
impl TtsBackend for FakeSherpa {
    fn id(&self) -> &str {
        "sherpa"
    }
    fn available(&self) -> Availability {
        Availability::Ready
    }
    fn voices(&self) -> Vec<VoiceDesc> {
        vec![]
    }
    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k, TtsError> {
        // 17 samples per character: deliberately not a whole millisecond.
        let samples = req.text.chars().count() * 17 + 1;
        Ok(Pcm16k::new(vec![0x11; samples * 2]).unwrap())
    }
}

/// Stands in for Azure: 16 kHz native, also arbitrary sample counts.
struct FakeAzure;

#[async_trait]
impl TtsBackend for FakeAzure {
    fn id(&self) -> &str {
        "azure"
    }
    fn available(&self) -> Availability {
        Availability::Ready
    }
    fn voices(&self) -> Vec<VoiceDesc> {
        vec![]
    }
    async fn render(&self, req: &RenderRequest) -> Result<Pcm16k, TtsError> {
        let samples = req.text.chars().count() * 23 + 5;
        Ok(Pcm16k::new(vec![0x22; samples * 2]).unwrap())
    }
    async fn render_batch(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>, TtsError> {
        // One request for the whole shard, as the real plugin does.
        let mut out = Vec::with_capacity(reqs.len());
        for r in reqs {
            out.push(self.render(r).await?);
        }
        Ok(out)
    }
}

fn chapter() -> Vec<RenderRequest> {
    let rows: &[(&str, &str, SpeakerKind)] = &[
        (
            "sherpa:piper-en_GB-cori:0",
            "The vale smelled of iron and wet ash.",
            SpeakerKind::Narrator,
        ),
        (
            "sherpa:kokoro-multi-lang-v1_0:18",
            "\"We don't have long,\" Kael said.",
            SpeakerKind::Character,
        ),
        (
            "azure:en-GB-Ada:DragonHDLatestNeural",
            "A guest voice answered from the dark.",
            SpeakerKind::Character,
        ),
        (
            "sherpa:kokoro-multi-lang-v1_0:26",
            "You have gained a level.",
            SpeakerKind::System,
        ),
        (
            "azure:en-US-Ava:DragonHDLatestNeural",
            "And then the ward broke.",
            SpeakerKind::Character,
        ),
        (
            "sherpa:piper-en_GB-cori:0",
            "Nothing moved for a long moment.",
            SpeakerKind::Narrator,
        ),
    ];
    rows.iter()
        .enumerate()
        .map(|(i, (v, t, k))| RenderRequest::parse(i as u32, v, *t, *k).unwrap())
        .collect()
}

fn registry() -> TtsRegistry {
    TtsRegistry::new()
        .with(Box::new(FakeSherpa))
        .with(Box::new(FakeAzure))
}

#[tokio::test]
async fn a_mixed_backend_chapter_concatenates_into_one_continuous_stream() {
    let reqs = chapter();
    let parts = registry().render_all(&reqs).await.unwrap();
    assert_eq!(parts.len(), 6);

    let joined = Pcm16k::concat(&parts);
    let expected: usize = parts.iter().map(|p| p.len()).sum();
    assert_eq!(joined.len(), expected);
    assert_eq!(joined.len() % 2, 0, "no half-samples survive a join");
}

#[tokio::test]
async fn per_segment_padding_makes_total_bytes_exactly_32000_times_seconds() {
    let reqs = chapter();
    // Padding each render to a whole millisecond is what lets `ms * 32` address
    // the joined stream — without it, segment N's manifest offset drifts by the
    // accumulated sub-millisecond remainders of segments 0..N.
    let parts: Vec<Pcm16k> = registry()
        .render_all(&reqs)
        .await
        .unwrap()
        .into_iter()
        .map(Pcm16k::padded_to_whole_ms)
        .collect();

    let joined = Pcm16k::concat(&parts);
    let seconds = joined.duration_ms() as f64 / 1000.0;

    assert!(joined.is_whole_ms());
    assert_eq!(joined.len() as f64, 32_000.0 * seconds);
    assert_eq!(joined.len() as u32, joined.duration_ms() * BYTES_PER_MS);
}

#[tokio::test]
async fn manifest_byte_offsets_address_the_joined_mixed_stream_exactly() {
    let reqs = chapter();
    let parts: Vec<Pcm16k> = registry()
        .render_all(&reqs)
        .await
        .unwrap()
        .into_iter()
        .map(Pcm16k::padded_to_whole_ms)
        .collect();

    let mut cursor_ms = 0u32;
    let mut segments = Vec::new();
    for (r, pcm) in reqs.iter().zip(&parts) {
        let start_ms = cursor_ms;
        cursor_ms += pcm.duration_ms();
        segments.push(Segment {
            idx: r.idx,
            speaker: format!("s{}", r.idx),
            kind: r.kind,
            voice_ref: r.voice.to_string(),
            text: r.text.clone(),
            start_ms,
            end_ms: cursor_ms,
        });
    }

    let manifest = Manifest::new(1, segments);
    let joined = Pcm16k::concat(&parts);

    assert_eq!(manifest.sample_rate, SAMPLE_RATE_HZ);
    assert!(
        manifest.is_contiguous(),
        "no gaps between mixed-backend segments"
    );
    assert_eq!(
        manifest.total_bytes(),
        joined.len() as u64,
        "manifest byte total must equal the real PCM length"
    );

    // Every segment's Range request must land on its own audio. Both fakes fill
    // with a backend-distinct byte, so a drifted offset is detectable.
    for (seg, pcm) in manifest.segments.iter().zip(&parts) {
        let slice = &joined.as_bytes()[seg.start_byte() as usize..seg.end_byte() as usize];
        assert_eq!(slice.len(), pcm.len(), "segment {} length", seg.idx);
        assert_eq!(slice, pcm.as_bytes(), "segment {} content", seg.idx);
        let tag = if seg.voice_ref.starts_with("sherpa:") {
            0x11
        } else {
            0x22
        };
        assert_eq!(
            slice[0], tag,
            "segment {} came from the wrong backend",
            seg.idx
        );
    }
}

#[tokio::test]
async fn assemble_builds_a_mixed_backend_manifest_that_agrees_with_hand_arithmetic() {
    use litrpg_tts::{DEFAULT_GAP_MS, assemble};

    let reqs = chapter();
    let parts = registry().render_all(&reqs).await.unwrap();
    let a = assemble(&parts, DEFAULT_GAP_MS);

    let segments: Vec<Segment> = reqs
        .iter()
        .zip(&a.spans)
        .map(|(r, s)| Segment {
            idx: r.idx,
            speaker: format!("s{}", r.idx),
            kind: r.kind,
            voice_ref: r.voice.to_string(),
            text: r.text.clone(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
        })
        .collect();
    let manifest = Manifest::new(1, segments);

    assert!(manifest.is_contiguous());
    assert_eq!(manifest.total_bytes(), a.pcm.len() as u64);
    assert_eq!(a.pcm.len() as u32, a.pcm.duration_ms() * BYTES_PER_MS);

    // Byte offsets from the manifest and from the spans must be the same numbers.
    for (seg, span) in manifest.segments.iter().zip(&a.spans) {
        assert_eq!(seg.start_byte(), span.start_byte());
        assert_eq!(seg.end_byte(), span.end_byte());
        let slice = &a.pcm.as_bytes()[seg.start_byte() as usize..seg.end_byte() as usize];
        let tag = if seg.voice_ref.starts_with("sherpa:") {
            0x11
        } else {
            0x22
        };
        assert_eq!(
            slice[0], tag,
            "segment {} came from the wrong backend",
            seg.idx
        );
    }

    // And the watch can look up any millisecond without falling in a hole.
    for ms in [0, 1, a.pcm.duration_ms() / 2, a.pcm.duration_ms() - 1] {
        assert!(
            manifest.segment_at_ms(ms).is_some(),
            "no segment at {ms} ms"
        );
    }
}

#[tokio::test]
async fn silence_padding_between_segments_is_pacing_and_keeps_the_identity() {
    // Reverie measured joins as click-free without padding (spike Part 2 §2.4),
    // so inter-segment silence is a narrative-pacing choice. It must not break
    // the byte identity.
    let reqs = chapter();
    let rendered: Vec<Pcm16k> = registry()
        .render_all(&reqs)
        .await
        .unwrap()
        .into_iter()
        .map(Pcm16k::padded_to_whole_ms)
        .collect();

    let gap = Pcm16k::silence_ms(120);
    let mut with_gaps: Vec<Pcm16k> = Vec::new();
    for (i, p) in rendered.iter().enumerate() {
        if i > 0 {
            with_gaps.push(gap.clone());
        }
        with_gaps.push(p.clone());
    }

    let plain = Pcm16k::concat(&rendered);
    let padded = Pcm16k::concat(&with_gaps);

    assert_eq!(padded.duration_ms(), plain.duration_ms() + 120 * 5);
    assert_eq!(padded.len() as u32, padded.duration_ms() * BYTES_PER_MS);
}
