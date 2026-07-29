//! `Pcm16k` — the normalized output contract every plugin returns (spec §7.1).
//!
//! 16 kHz mono s16le, headerless, exactly 32 000 B/s = 32 B/ms.

use litrpg_tts::pcm::{Span, assemble};
use litrpg_tts::{Pcm16k, PcmError};
use proptest::prelude::*;

// ---------------------------------------------------------------- unit tests

#[test]
fn rejects_odd_byte_length() {
    // Half a sample is always a bug — a truncated read, a bad offset, or a
    // header that leaked into a headerless stream.
    assert_eq!(
        Pcm16k::new(vec![0u8; 33]).unwrap_err(),
        PcmError::OddByteLength(33)
    );
    assert_eq!(
        Pcm16k::new(vec![0u8; 1]).unwrap_err(),
        PcmError::OddByteLength(1)
    );
}

#[test]
fn accepts_even_byte_length() {
    let p = Pcm16k::new(vec![0u8; 64]).unwrap();
    assert_eq!(p.len(), 64);
    assert_eq!(p.samples(), 32);
}

#[test]
fn empty_is_zero_duration() {
    let p = Pcm16k::empty();
    assert!(p.is_empty());
    assert_eq!(p.len(), 0);
    assert_eq!(p.duration_ms(), 0);
    assert!(p.is_whole_ms());
}

#[test]
fn one_second_is_exactly_32000_bytes() {
    let p = Pcm16k::silence_ms(1000);
    assert_eq!(p.len(), 32_000);
    assert_eq!(p.duration_ms(), 1000);
    assert_eq!(Pcm16k::BYTES_PER_SEC, 32_000);
    assert_eq!(Pcm16k::BYTES_PER_MS, 32);
}

#[test]
fn duration_floors_sub_millisecond_tails() {
    // 34 B = 17 samples = 1.0625 ms. Real TTS output lands on arbitrary sample
    // counts, not whole milliseconds, so this case is the norm, not an edge.
    let p = Pcm16k::new(vec![0u8; 34]).unwrap();
    assert_eq!(p.duration_ms(), 1);
    assert!(!p.is_whole_ms());
    assert_eq!(p.remainder_bytes(), 2);
}

#[test]
fn pad_to_whole_ms_restores_the_byte_identity() {
    let mut p = Pcm16k::new(vec![7u8; 34]).unwrap();
    p.pad_to_whole_ms();
    assert_eq!(p.len(), 64);
    assert!(p.is_whole_ms());
    assert_eq!(p.duration_ms() * 32, p.len() as u32);
    // The pad is silence appended to the tail, never a rewrite of the signal.
    assert_eq!(&p.as_bytes()[..34], &[7u8; 34][..]);
    assert_eq!(&p.as_bytes()[34..], &[0u8; 30][..]);
}

#[test]
fn pad_is_a_no_op_when_already_whole() {
    let mut p = Pcm16k::silence_ms(5);
    p.pad_to_whole_ms();
    assert_eq!(p.len(), 160);
}

#[test]
fn concat_of_nothing_is_empty() {
    assert_eq!(Pcm16k::concat(&[]).len(), 0);
}

#[test]
fn extend_appends_in_order() {
    let mut a = Pcm16k::new(vec![1u8, 2]).unwrap();
    a.extend(&Pcm16k::new(vec![3u8, 4]).unwrap());
    assert_eq!(a.as_bytes(), &[1u8, 2, 3, 4]);
}

#[test]
fn an_even_length_buffer_never_needs_more_than_30_pad_bytes() {
    // Remainders of an even length are even, so the pad is in 2..=30 — under a
    // millisecond, inaudible. An odd length cannot reach here at all.
    for samples in 1..64usize {
        let n = samples * 2;
        let p = Pcm16k::new(vec![1u8; n]).unwrap().padded_to_whole_ms();
        assert!(p.len() - n <= 30, "{n} B needed {} pad bytes", p.len() - n);
        assert!(p.is_whole_ms());
    }
}

// ------------------------------------------------------------ chapter assembly

#[test]
fn the_documented_default_gap_is_zero() {
    assert_eq!(
        litrpg_tts::pcm::DEFAULT_GAP_MS,
        0,
        "a gap is a pacing choice someone opts into, not a default"
    );
}

#[test]
fn assembly_defaults_to_no_gap_between_segments() {
    // Joins are click-free without padding (spike Part 2 §2.4): measured join
    // deltas of 6 and 1 against a p99.9 signal delta of ~3595, because sherpa
    // output already begins and ends at ~zero amplitude. So a gap is a narrative
    // pacing choice, never artifact suppression, and the default is none.
    let parts = vec![Pcm16k::silence_ms(100), Pcm16k::silence_ms(200)];
    let a = assemble(&parts, 0);
    assert_eq!(a.pcm.duration_ms(), 300);
    assert_eq!(a.spans, vec![Span::new(0, 100), Span::new(100, 300)]);
}

#[test]
fn a_pacing_gap_goes_between_segments_only() {
    let parts = vec![
        Pcm16k::silence_ms(100),
        Pcm16k::silence_ms(200),
        Pcm16k::silence_ms(300),
    ];
    let a = assemble(&parts, 120);
    // Two gaps for three segments — never a leading or trailing one.
    assert_eq!(a.pcm.duration_ms(), 600 + 240);
    assert_eq!(a.spans.first().unwrap().start_ms, 0);
    assert_eq!(a.spans.last().unwrap().end_ms, a.pcm.duration_ms());
}

#[test]
fn a_gap_is_attributed_to_the_segment_it_follows() {
    // The beat after a line belongs to that line: the watch keeps highlighting
    // the sentence just spoken through the pause, instead of jumping early to a
    // sentence that has not started making sound.
    let parts = vec![Pcm16k::silence_ms(100), Pcm16k::silence_ms(200)];
    let a = assemble(&parts, 50);
    assert_eq!(a.spans[0], Span::new(0, 150), "gap trails segment 0");
    assert_eq!(a.spans[1], Span::new(150, 350));
}

#[test]
fn assembly_spans_are_contiguous_and_cover_the_whole_stream() {
    let parts = vec![
        Pcm16k::silence_ms(10),
        Pcm16k::silence_ms(20),
        Pcm16k::silence_ms(30),
    ];
    let a = assemble(&parts, 15);
    assert_eq!(a.spans[0].start_ms, 0);
    for w in a.spans.windows(2) {
        assert_eq!(w[0].end_ms, w[1].start_ms, "a gap would strand the watch");
    }
    assert_eq!(a.spans.last().unwrap().end_ms, a.pcm.duration_ms());
}

#[test]
fn assembly_aligns_misaligned_parts_so_offsets_stay_exact() {
    // loudnorm's filter delay changes stream length (Reverie's SYSTEM segment came
    // out 104 B shorter), so parts arriving here may not be whole-ms. Offsets are
    // measured from the final PCM, never precomputed.
    let parts = vec![
        Pcm16k::new(vec![1u8; 3_206]).unwrap(), // 3206 % 32 == 6
        Pcm16k::new(vec![2u8; 1_998]).unwrap(), // 1998 % 32 == 14
    ];
    let a = assemble(&parts, 0);
    assert!(a.pcm.is_whole_ms());
    assert_eq!(a.pcm.len() as u32, a.pcm.duration_ms() * 32);
    for (i, s) in a.spans.iter().enumerate() {
        let slice = &a.pcm.as_bytes()[s.start_byte() as usize..s.end_byte() as usize];
        assert_eq!(slice.len() as u32, s.duration_ms() * 32);
        // Each span still opens on its own segment's fill byte.
        assert_eq!(slice[0], parts[i].as_bytes()[0]);
    }
}

#[test]
fn assembling_nothing_is_empty() {
    let a = assemble(&[], 120);
    assert!(a.pcm.is_empty());
    assert!(a.spans.is_empty());
}

#[test]
fn assembling_one_segment_inserts_no_gap() {
    let a = assemble(&[Pcm16k::silence_ms(100)], 500);
    assert_eq!(a.pcm.duration_ms(), 100);
    assert_eq!(a.spans, vec![Span::new(0, 100)]);
}

// ------------------------------------------------------------ property tests

proptest! {
    /// The core contract: for whole-millisecond audio, duration and byte count
    /// are the same fact expressed two ways.
    #[test]
    fn whole_ms_audio_satisfies_duration_times_32_equals_len(ms in 0u32..5_000) {
        let p = Pcm16k::silence_ms(ms);
        prop_assert_eq!(p.duration_ms() * 32, p.len() as u32);
        prop_assert!(p.is_whole_ms());
    }

    /// Any even length is a legal PCM buffer; duration floors to whole ms.
    #[test]
    fn even_lengths_are_accepted_and_floor(samples in 0usize..20_000) {
        let n = samples * 2;
        let p = Pcm16k::new(vec![0u8; n]).unwrap();
        prop_assert_eq!(p.len(), n);
        prop_assert_eq!(p.samples(), samples);
        prop_assert_eq!(p.duration_ms() as usize, n / 32);
        prop_assert!(p.duration_ms() as usize * 32 <= p.len());
    }

    /// Odd lengths are rejected regardless of size.
    #[test]
    fn odd_lengths_are_always_rejected(samples in 0usize..20_000) {
        let n = samples * 2 + 1;
        prop_assert_eq!(Pcm16k::new(vec![0u8; n]).unwrap_err(), PcmError::OddByteLength(n));
    }

    /// Padding always establishes the identity, and never removes audio.
    #[test]
    fn padding_always_establishes_the_identity(samples in 0usize..20_000) {
        let n = samples * 2;
        let p = Pcm16k::new(vec![0u8; n]).unwrap().padded_to_whole_ms();
        prop_assert!(p.is_whole_ms());
        prop_assert_eq!(p.duration_ms() * 32, p.len() as u32);
        prop_assert!(p.len() >= n);
        prop_assert!(p.len() - n <= 30, "even lengths never need 31 pad bytes");
    }

    /// Assembly is exact for any part sizes and any gap — this is the identity the
    /// watch's Range requests and sentence highlighting rest on.
    #[test]
    fn assembly_arithmetic_is_always_exact(
        parts in prop::collection::vec(1usize..600, 1..10),
        gap_ms in 0u32..250,
    ) {
        let pcms: Vec<Pcm16k> = parts
            .iter()
            .map(|&s| Pcm16k::new(vec![9u8; s * 2]).unwrap())
            .collect();
        let a = assemble(&pcms, gap_ms);

        prop_assert!(a.pcm.is_whole_ms());
        prop_assert_eq!(a.pcm.len() as u32, a.pcm.duration_ms() * 32);
        prop_assert_eq!(a.spans.len(), pcms.len());
        prop_assert_eq!(a.spans[0].start_ms, 0);
        prop_assert_eq!(a.spans[a.spans.len() - 1].end_ms, a.pcm.duration_ms());
        for w in a.spans.windows(2) {
            prop_assert_eq!(w[0].end_ms, w[1].start_ms);
        }
        // Every span addresses real bytes inside the stream.
        for s in &a.spans {
            prop_assert!(s.end_byte() <= a.pcm.len() as u64);
            prop_assert_eq!(s.end_byte() - s.start_byte(), s.duration_ms() as u64 * 32);
        }
    }

    /// Concatenation preserves total duration — this is what lets one chapter
    /// interleave backends and still address one continuous stream.
    #[test]
    fn concat_preserves_total_duration(parts in prop::collection::vec(0u32..500, 0..12)) {
        let pcms: Vec<Pcm16k> = parts.iter().copied().map(Pcm16k::silence_ms).collect();
        let total_ms: u32 = parts.iter().sum();
        let joined = Pcm16k::concat(&pcms);
        prop_assert_eq!(joined.duration_ms(), total_ms);
        prop_assert_eq!(joined.len() as u32, total_ms * 32);
    }

    /// Byte-count additivity holds for arbitrary (non-whole-ms) parts too.
    #[test]
    fn concat_preserves_total_bytes(parts in prop::collection::vec(0usize..600, 0..12)) {
        let pcms: Vec<Pcm16k> = parts
            .iter()
            .map(|&s| Pcm16k::new(vec![0u8; s * 2]).unwrap())
            .collect();
        let expected: usize = parts.iter().map(|s| s * 2).sum();
        let joined = Pcm16k::concat(&pcms);
        prop_assert_eq!(joined.len(), expected);
        prop_assert_eq!(joined.len() % 2, 0);
    }

    /// Per-segment padding makes the chapter-level identity exact even when
    /// every individual render had a sub-millisecond tail.
    #[test]
    fn padding_each_part_makes_the_join_exact(parts in prop::collection::vec(1usize..600, 1..12)) {
        let pcms: Vec<Pcm16k> = parts
            .iter()
            .map(|&s| Pcm16k::new(vec![0u8; s * 2]).unwrap().padded_to_whole_ms())
            .collect();
        let total_ms: u32 = pcms.iter().map(|p| p.duration_ms()).sum();
        let joined = Pcm16k::concat(&pcms);
        prop_assert!(joined.is_whole_ms());
        prop_assert_eq!(joined.duration_ms(), total_ms);
        prop_assert_eq!(joined.len() as u32, total_ms * 32);
    }

    /// `extend` and `concat` are the same operation.
    #[test]
    fn extend_matches_concat(a in 0usize..400, b in 0usize..400) {
        let pa = Pcm16k::new(vec![1u8; a * 2]).unwrap();
        let pb = Pcm16k::new(vec![2u8; b * 2]).unwrap();
        let mut acc = pa.clone();
        acc.extend(&pb);
        let joined = Pcm16k::concat(&[pa, pb]);
        prop_assert_eq!(acc.as_bytes(), joined.as_bytes());
    }

    /// The newtype is transparent: bytes in, same bytes out.
    #[test]
    fn bytes_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..2_000)) {
        let even: Vec<u8> = bytes[..bytes.len() / 2 * 2].to_vec();
        let p = Pcm16k::new(even.clone()).unwrap();
        prop_assert_eq!(p.into_bytes(), even);
    }
}
