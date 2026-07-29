use litrpg_core::manifest::{BYTES_PER_MS, Manifest, SAMPLE_RATE_HZ, Segment, SpeakerKind};
use proptest::prelude::*;

fn seg(idx: u32, start_ms: u32, end_ms: u32) -> Segment {
    Segment {
        idx,
        speaker: "narrator".into(),
        kind: SpeakerKind::Narrator,
        voice_ref: "sherpa:piper-en_GB-cori:0".into(),
        text: "The vale smelled of iron and wet ash.".into(),
        start_ms,
        end_ms,
    }
}

#[test]
fn pcm_constants_match_16khz_mono_s16le() {
    assert_eq!(SAMPLE_RATE_HZ, 16_000);
    // 16000 samples/sec * 2 bytes/sample / 1000 ms
    assert_eq!(BYTES_PER_MS, 32);
}

#[test]
fn byte_offsets_are_ms_times_32() {
    let s = seg(0, 0, 4120);
    assert_eq!(s.start_byte(), 0);
    assert_eq!(s.end_byte(), 131_840);
    assert_eq!(s.duration_ms(), 4120);
}

#[test]
fn manifest_derives_duration_and_total_bytes() {
    let m = Manifest::new(42, vec![seg(0, 0, 4120), seg(1, 4120, 9000)]);
    assert_eq!(m.chapter, 42);
    assert_eq!(m.sample_rate, 16_000);
    assert_eq!(m.duration_ms, 9000);
    assert_eq!(m.total_bytes(), 288_000);
}

#[test]
fn empty_manifest_is_zero_length_not_a_panic() {
    let m = Manifest::new(1, vec![]);
    assert_eq!(m.duration_ms, 0);
    assert_eq!(m.total_bytes(), 0);
    assert!(m.is_contiguous());
    assert!(m.segment_at_ms(0).is_none());
}

#[test]
fn segment_at_ms_uses_half_open_intervals() {
    let m = Manifest::new(1, vec![seg(0, 0, 100), seg(1, 100, 200)]);
    assert_eq!(m.segment_at_ms(0).unwrap().idx, 0);
    assert_eq!(m.segment_at_ms(99).unwrap().idx, 0);
    assert_eq!(m.segment_at_ms(100).unwrap().idx, 1);
    assert!(m.segment_at_ms(200).is_none());
}

#[test]
fn detects_a_gap_between_segments() {
    let m = Manifest::new(1, vec![seg(0, 0, 100), seg(1, 150, 200)]);
    assert!(!m.is_contiguous());
}

#[test]
fn round_trips_through_json() {
    let m = Manifest::new(42, vec![seg(0, 0, 4120)]);
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
}

proptest! {
    /// The invariant the watch's Range requests depend on.
    #[test]
    fn start_byte_always_equals_start_ms_times_32(start_ms in 0u32..10_000_000) {
        let s = seg(0, start_ms, start_ms.saturating_add(1));
        prop_assert_eq!(s.start_byte(), start_ms as u64 * 32);
    }

    /// A contiguous manifest's total byte count is exactly the sum of its segments'.
    #[test]
    fn total_bytes_equals_sum_of_segment_bytes(lens in prop::collection::vec(1u32..5_000, 1..40)) {
        let mut segments = Vec::new();
        let mut cursor = 0u32;
        for (i, len) in lens.iter().enumerate() {
            segments.push(seg(i as u32, cursor, cursor + len));
            cursor += len;
        }
        let m = Manifest::new(1, segments);
        prop_assert!(m.is_contiguous());
        let summed: u64 = m.segments.iter().map(|s| s.end_byte() - s.start_byte()).sum();
        prop_assert_eq!(m.total_bytes(), summed);
    }
}

/// The load-bearing test for [`SpeakerKind::as_str`]: the hand-written canonical form
/// and serde's `rename_all = "lowercase"` must agree. They are two independent
/// spellings of one fact, and this is the only thing making them match.
///
/// If serde's output ever changes — a rename, a different casing convention — this
/// fails here rather than as a mis-voiced chapter noticed by ear weeks later.
#[test]
fn as_str_agrees_with_the_serde_representation() {
    for kind in SpeakerKind::ALL {
        let json = serde_json::to_string(&kind).unwrap();
        let unquoted = json.trim_matches('"');
        assert_eq!(
            kind.as_str(),
            unquoted,
            "as_str and serde disagree for {kind:?} — kind selects the voice, so a drift \
             here mis-voices a chapter"
        );
    }
}

#[test]
fn from_canonical_round_trips_every_variant() {
    for kind in SpeakerKind::ALL {
        assert_eq!(SpeakerKind::from_canonical(kind.as_str()), Some(kind));
    }
}

/// Strict on purpose. The store used to coerce anything unrecognised to `Narrator`,
/// which would silently re-voice a character — the same destructive-default asymmetry
/// that made `op_from_str` strict.
#[test]
fn from_canonical_rejects_anything_else() {
    for bad in ["Narrator", "SYSTEM", "System", "", "narrater", "npc"] {
        assert_eq!(
            SpeakerKind::from_canonical(bad),
            None,
            "{bad:?} must not silently become a kind"
        );
    }
}
