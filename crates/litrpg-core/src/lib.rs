//! Shared wire types for the endless-litrpg system.
//!
//! This crate is `no_std` + `alloc` and contains **no I/O**. It is depended on by
//! the daemon (std) and by the ESP32-C6 watch firmware (bare metal), so both agree
//! on the wire format by construction rather than by convention.

#![no_std]

extern crate alloc;

pub mod ledger;
pub mod manifest;
pub mod validate;
pub mod voice;

pub use ledger::{LedgerEntry, Op, StateSnapshot, Value, fold, rewind};
pub use manifest::{BYTES_PER_MS, Manifest, SAMPLE_RATE_HZ, Segment, SpeakerKind};
pub use validate::{Delta, Rejection, validate_delta};
pub use voice::{VoiceRef, VoiceRefError};
