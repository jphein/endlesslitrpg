use litrpg_core::artifact::{
    CHAPTER_DIGITS, chapter_stem, manifest_name, media_name, mp3_name, pcm_name, text_name,
};

#[test]
fn pads_to_four_digits() {
    assert_eq!(CHAPTER_DIGITS, 4);
    assert_eq!(chapter_stem(1), "0001");
    assert_eq!(chapter_stem(42), "0042");
    assert_eq!(chapter_stem(999), "0999");
    assert_eq!(chapter_stem(1000), "1000");
}

/// Four digits is a **minimum**, not a width. Truncating would make two chapters share
/// a filename, and the failure would land 12,000 chapters in — long after anyone would
/// think to look.
#[test]
fn a_long_serial_is_not_truncated() {
    assert_eq!(chapter_stem(12_345), "12345");
    assert_eq!(chapter_stem(1_000_000), "1000000");
    assert_eq!(mp3_name(12_345), "12345.mp3");
}

#[test]
fn zero_is_representable_even_though_chapters_start_at_one() {
    // Not a valid chapter, but a caller passing 0 should get a name rather than a panic.
    assert_eq!(chapter_stem(0), "0000");
}

#[test]
fn the_four_artifacts_of_section_8() {
    assert_eq!(text_name(1), "0001.md");
    assert_eq!(manifest_name(1), "0001.json");
    assert_eq!(mp3_name(1), "0001.mp3");
    assert_eq!(pcm_name(1), "0001.pcm");
}

#[test]
fn media_name_is_the_shared_primitive() {
    // The named helpers must agree with the general one, or a caller reaching for
    // either would get a different answer.
    for n in [1u32, 9, 10, 99, 100, 9_999, 10_000] {
        assert_eq!(mp3_name(n), media_name(n, "mp3"));
        assert_eq!(pcm_name(n), media_name(n, "pcm"));
        assert_eq!(text_name(n), media_name(n, "md"));
        assert_eq!(manifest_name(n), media_name(n, "json"));
    }
}

/// Pins the exact strings the daemon serves, the CLI resolves and the watch firmware
/// requests. If this changes, every chapter already on disk becomes unreachable.
#[test]
fn the_wire_format_is_pinned() {
    assert_eq!(pcm_name(1), "0001.pcm");
    assert_eq!(format!("/media/{}", mp3_name(7)), "/media/0007.mp3");
}
