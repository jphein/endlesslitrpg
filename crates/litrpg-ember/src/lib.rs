//! Ember client — the local Qwen3.6-35B-A3B lane on `familiar:8091`.
//!
//! Owns prompt assembly and the two-pass contract: an unconstrained creative pass
//! that emits speaker-tagged prose, then a short `json_schema`-constrained
//! extraction pass over the finished chapter.
