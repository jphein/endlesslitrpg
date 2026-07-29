//! `Pcm16k` — the normalized output contract every plugin returns (spec §7.1).
//!
//! 16 kHz mono s16le, headerless, exactly 32 000 B/s = 32 B/ms.

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
        prop_assert!(p.len() - n < 32);
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
