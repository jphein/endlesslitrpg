//! Pure unit tests for Range parsing (RFC 7233).

use litrpg_daemon::range::{RangeOutcome, content_range, content_range_unsatisfied, parse_range};

/// Chapter 1's size in the fixture: 3000 ms x 32 B/ms.
const TOTAL: u64 = 96_000;

fn partial(start: u64, end: u64) -> RangeOutcome {
    RangeOutcome::Partial { start, end }
}

#[test]
fn no_header_serves_full() {
    assert_eq!(parse_range(None, TOTAL), RangeOutcome::Full);
}

#[test]
fn exact_closed_interval() {
    assert_eq!(parse_range(Some("bytes=0-99"), TOTAL), partial(0, 99));
    assert_eq!(
        parse_range(Some("bytes=32000-63999"), TOTAL),
        partial(32_000, 63_999)
    );
}

#[test]
fn open_ended_range() {
    assert_eq!(parse_range(Some("bytes=0-"), TOTAL), partial(0, TOTAL - 1));
    assert_eq!(
        parse_range(Some("bytes=1000-"), TOTAL),
        partial(1_000, TOTAL - 1)
    );
}

#[test]
fn last_byte_is_satisfiable() {
    assert_eq!(
        parse_range(Some("bytes=95999-"), TOTAL),
        partial(95_999, 95_999)
    );
    assert_eq!(
        parse_range(Some("bytes=95999-95999"), TOTAL).content_length(TOTAL),
        1
    );
}

#[test]
fn suffix_range() {
    assert_eq!(
        parse_range(Some("bytes=-1000"), TOTAL),
        partial(95_000, TOTAL - 1)
    );
}

#[test]
fn suffix_longer_than_file_clamps_to_whole_file() {
    // RFC 7233 §2.1: a suffix larger than the representation is satisfied by all of it.
    assert_eq!(
        parse_range(Some("bytes=-200000"), TOTAL),
        partial(0, TOTAL - 1)
    );
}

#[test]
fn end_past_eof_clamps_rather_than_failing() {
    assert_eq!(
        parse_range(Some("bytes=0-200000"), TOTAL),
        partial(0, TOTAL - 1)
    );
    assert_eq!(
        parse_range(Some("bytes=95000-200000"), TOTAL),
        partial(95_000, TOTAL - 1)
    );
}

#[test]
fn start_at_or_past_eof_is_unsatisfiable() {
    assert_eq!(
        parse_range(Some("bytes=96000-"), TOTAL),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range(Some("bytes=100000-200000"), TOTAL),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range(Some("bytes=96000-96010"), TOTAL),
        RangeOutcome::Unsatisfiable
    );
}

#[test]
fn zero_length_suffix_is_unsatisfiable() {
    // `bytes=-0` requests the last zero bytes: nothing can satisfy it.
    assert_eq!(
        parse_range(Some("bytes=-0"), TOTAL),
        RangeOutcome::Unsatisfiable
    );
}

#[test]
fn empty_representation_cannot_satisfy_any_range() {
    assert_eq!(
        parse_range(Some("bytes=0-"), 0),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range(Some("bytes=0-10"), 0),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(
        parse_range(Some("bytes=-10"), 0),
        RangeOutcome::Unsatisfiable
    );
    // No header on an empty file is still a plain 200 with an empty body.
    assert_eq!(parse_range(None, 0), RangeOutcome::Full);
}

#[test]
fn malformed_headers_are_ignored_not_rejected() {
    // RFC 7233 §3.1 — a Range that cannot be understood MUST be ignored.
    for h in [
        "bytes=500-100", // start > end: invalid per the grammar
        "bytes=abc-def",
        "bytes=-",
        "bytes=",
        "items=0-99", // unsupported unit
        "0-99",       // no unit
        "bytes 0-99", // missing '='
        "bytes=1.5-3",
        "bytes=-abc",
        "bytes=abc-",
    ] {
        assert_eq!(
            parse_range(Some(h), TOTAL),
            RangeOutcome::Full,
            "expected {h:?} to be ignored"
        );
    }
}

#[test]
fn multi_range_degrades_to_full() {
    // We do not emit multipart/byteranges, and a single 206 would violate the
    // client's expectation, so 200 is the only honest answer.
    assert_eq!(
        parse_range(Some("bytes=0-1,5-6"), TOTAL),
        RangeOutcome::Full
    );
    assert_eq!(
        parse_range(Some("bytes=0-99, 200-299"), TOTAL),
        RangeOutcome::Full
    );
}

#[test]
fn surrounding_whitespace_tolerated() {
    assert_eq!(parse_range(Some("  bytes=0-99  "), TOTAL), partial(0, 99));
    assert_eq!(parse_range(Some("bytes= 0 - 99 "), TOTAL), partial(0, 99));
}

#[test]
fn content_length_matches_outcome() {
    assert_eq!(RangeOutcome::Full.content_length(TOTAL), TOTAL);
    assert_eq!(partial(0, 99).content_length(TOTAL), 100);
    assert_eq!(partial(0, 0).content_length(TOTAL), 1);
    assert_eq!(partial(32_000, 63_999).content_length(TOTAL), 32_000);
    assert_eq!(RangeOutcome::Unsatisfiable.content_length(TOTAL), 0);
}

#[test]
fn content_range_wire_format() {
    assert_eq!(content_range(0, 99, TOTAL), "bytes 0-99/96000");
    assert_eq!(
        content_range(32_000, 63_999, TOTAL),
        "bytes 32000-63999/96000"
    );
    assert_eq!(content_range_unsatisfied(TOTAL), "bytes */96000");
}

/// The real usage pattern: a segment's manifest offsets used verbatim as a Range.
///
/// Segment 1 spans 1000..2000 ms, so 32000..64000 bytes; as an inclusive HTTP range
/// that is `32000-63999`, and it must yield exactly `duration_ms * 32` bytes.
#[test]
fn manifest_offsets_translate_to_exact_segment_bytes() {
    let (start_ms, end_ms) = (1000u64, 2000u64);
    let (start_byte, end_byte) = (start_ms * 32, end_ms * 32);

    let header = format!("bytes={}-{}", start_byte, end_byte - 1);
    let outcome = parse_range(Some(&header), TOTAL);

    assert_eq!(outcome, partial(32_000, 63_999));
    assert_eq!(
        outcome.content_length(TOTAL),
        (end_ms - start_ms) * 32,
        "a segment's byte count must equal its duration times 32"
    );
}
