//! Stable content hashing, used for provenance.
//!
//! Every chapter records a hash of the story prompt that produced it, so months
//! later you can tell your own edits apart from model drift. That value persists
//! for the life of a story, which drives every choice here.
//!
//! # Why FNV-1a and not SHA-256
//!
//! This is a **change detector, not a security primitive** — nobody is trying to
//! forge a prompt hash. Against that, `litrpg-core` is depended on by the ESP32-C6
//! firmware, where every dependency costs flash and build complexity, and FNV-1a is
//! twenty lines with none. A 64-bit digest is ample for distinguishing a handful of
//! prompt revisions.
//!
//! # Why not `std::hash::DefaultHasher`
//!
//! Its output is explicitly **not** guaranteed stable across Rust releases. A stored
//! provenance value that silently changes meaning on a toolchain upgrade is worse
//! than no provenance at all.
//!
//! # Why the algorithm tag
//!
//! Hashes render as `fnv1a64:<16 hex digits>`. If this is ever switched to SHA-256,
//! old and new values are **distinguishable on sight** rather than presenting as a
//! digest mismatch on unchanged content.
//!
//! This lives in `litrpg-core` rather than in a consumer because the CLI writes the
//! value and the engine reads it. Two implementations that must agree, and nothing
//! forcing them to, is exactly the failure this crate exists to prevent.

use alloc::string::String;

/// Algorithm tag prefixing every hash produced here.
pub const HASH_ALGO: &str = "fnv1a64";

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 over the raw bytes.
pub fn content_hash_u64(text: &str) -> u64 {
    let mut h = FNV_OFFSET;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Stable content hash, rendered as `fnv1a64:<16 hex digits>`.
pub fn content_hash(text: &str) -> String {
    alloc::format!("{HASH_ALGO}:{:016x}", content_hash_u64(text))
}
