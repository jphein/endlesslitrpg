//! Ember client — the local Qwen3.6-35B-A3B lane on `familiar:8091`.
//!
//! Owns prompt assembly and the two-pass contract: an unconstrained creative pass that
//! emits speaker-tagged prose, then a short `json_schema`-constrained extraction pass over
//! the finished chapter.
//!
//! ```text
//!   prompt::pass1_messages  ──▶  client::chat  ──▶  parse::parse_tagged_prose
//!         (pure)                  (HTTP)                    (pure)
//!                                                             │
//!                                                             ▼  chapter text
//!   prompt::pass2_messages  ──▶  client::chat  ──▶  extract::parse_extraction
//!         (pure)              (+ response_format)              (pure)
//!                                                             │
//!                                                             ▼
//!                                            litrpg_core::Delta  →  the store's gate
//! ```
//!
//! Only [`client`] touches the network. Everything that decides *what Ember is told* and
//! *what its answer means* is a pure function, so the interesting behaviour is covered by
//! unit tests rather than by a test that needs a GPU.
//!
//! # Empirical notes on this llama.cpp build (`b950-555881e`, measured 2026-07-29)
//!
//! 1. `response_format: {"type": "json_schema", …}` is supported and genuinely enforced.
//!    **GBNF `grammar` is not required.**
//! 2. The server compiles the schema and answers **HTTP 400** if it cannot, so a bad
//!    schema fails loudly on the first call.
//! 3. Qwen3.6 **reasons by default** and reasoning is *not* grammar-constrained, so a
//!    constrained call with reasoning on returns `content: ""` and
//!    `finish_reason: "length"`. [`client::EmberConfig::disable_thinking`] defaults to
//!    `true` for exactly this reason.
//! 4. Ember's real pass-1 output is *block* shaped — the tag alone on its line, content
//!    below, blank line ending the block — not one tag per line. [`parse`] handles both.
//!
//! `tests/live_ember.rs` pins every one of these against the real endpoint.

pub mod client;
pub mod error;
pub mod extract;
pub mod parse;
pub mod prompt;

pub use client::{
    ChatSpec, Completion, DEFAULT_BASE_URL, DEFAULT_MODEL, DEFAULT_TIMEOUT, EmberClient,
    EmberConfig, Message, Role,
};
pub use error::EmberError;
pub use extract::{
    Extraction, ProposedDelta, ProposedLore, ProposedSpeaker, QuestUpdate, parse_extraction,
};
pub use parse::{ParsedSegment, parse_tagged_prose};
pub use prompt::{
    ChapterSummary, DEFAULT_TARGET_WORDS, LoreEntry, Pass1Input, match_lore, pass1_messages,
    pass2_messages,
};
