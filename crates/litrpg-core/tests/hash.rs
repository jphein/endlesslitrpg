use litrpg_core::hash::{HASH_ALGO, content_hash, content_hash_u64};

/// Pins the FNV-1a 64 constants. If this test ever fails, a stored `prompt_hash`
/// has silently changed meaning for every existing chapter.
#[test]
fn known_vectors_are_pinned() {
    // Canonical FNV-1a 64 test vectors.
    assert_eq!(content_hash_u64(""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(content_hash_u64("a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(content_hash_u64("foobar"), 0x85944171f73967e8);
}

#[test]
fn rendered_form_is_tagged_and_fixed_width() {
    let h = content_hash("");
    assert_eq!(h, "fnv1a64:cbf29ce484222325");
    assert_eq!(HASH_ALGO, "fnv1a64");
    // tag + colon + exactly 16 hex digits
    let (tag, hex) = h.split_once(':').unwrap();
    assert_eq!(tag, HASH_ALGO);
    assert_eq!(hex.len(), 16);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn distinct_content_gives_distinct_hashes() {
    assert_ne!(content_hash("# Story prompt\n"), content_hash("# Story prompt"));
    assert_ne!(content_hash("Kaelen"), content_hash("kaelen"));
}

#[test]
fn identical_content_is_stable_across_calls() {
    let text = "The vale smelled of iron and wet ash.";
    assert_eq!(content_hash(text), content_hash(text));
}

/// A tagged hash makes an algorithm change visible rather than presenting as a
/// digest mismatch on unchanged content.
#[test]
fn tag_lets_a_future_algorithm_change_be_detected() {
    let stored = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let current = content_hash("anything");
    let stored_algo = stored.split_once(':').unwrap().0;
    let current_algo = current.split_once(':').unwrap().0;
    assert_ne!(stored_algo, current_algo);
}
