//! The chapter loop (spec §5.1).
//!
//! Joins the other crates into one cycle: check the buffer, assemble a prompt,
//! generate, parse, assign voices, extract, validate into the ledger, render audio,
//! publish artifacts. Every stage is idempotent by chapter number, so a crash
//! resumes from the last completed stage rather than corrupting one.
