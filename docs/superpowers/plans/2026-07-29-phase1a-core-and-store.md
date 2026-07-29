# Endless LitRPG — Phase 1a: Core Types & Store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundation two crates — `litrpg-core` (the `no_std` wire types shared with the ESP32-C6 firmware) and `litrpg-store` (SQLite persistence with the append-only ledger and its validation gate).

**Architecture:** `litrpg-core` is `#![no_std]` + `alloc` with zero I/O — pure data types, the ledger fold, and the validation gate, so both the daemon and the watch firmware depend on exactly the same definitions. `litrpg-store` is std-only, wraps SQLite via rusqlite, and is the only crate that writes anything. Current game state is never stored: it is a fold over `ledger WHERE applied = 1 ORDER BY seq`.

**Tech Stack:** Rust 1.97.1 (edition 2024), rusqlite 0.40.1 (bundled SQLite), serde 1.0.229, serde_json 1.0.151, thiserror 2.0.19, proptest 1.11.0.

**Spec:** `docs/superpowers/specs/2026-07-29-endless-litrpg-design.md` — §5 (crates), §6 (data model), §7.3 (voice refs), §8.1 (manifest).

---

## Environment note — read first

`cargo` is **not on the default non-interactive PATH** on this machine. Every command in this plan assumes:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Verify before starting:

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo --version
# Expected: cargo 1.97.1 (c980f4866 2026-06-30)
```

Work from the repo root: `/home/jp/Projects/endlesslitrpg`.

---

## File structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Workspace root, shared dependency versions |
| `rust-toolchain.toml` | Pin stable channel |
| `crates/litrpg-core/Cargo.toml` | Core manifest — `no_std`-compatible deps only |
| `crates/litrpg-core/src/lib.rs` | `#![no_std]`, module wiring, re-exports |
| `crates/litrpg-core/src/voice.rs` | `VoiceRef` + first-colon-only parsing |
| `crates/litrpg-core/src/manifest.rs` | `Segment`, `Manifest`, PCM constants, byte offsets |
| `crates/litrpg-core/src/ledger.rs` | `LedgerEntry`, `Op`, `Value`, `StateSnapshot`, `fold`, `rewind` |
| `crates/litrpg-core/src/validate.rs` | Field whitelist + the validation gate |
| `crates/litrpg-core/tests/voice.rs` | Voice ref parsing tests |
| `crates/litrpg-core/tests/manifest.rs` | Offset arithmetic + contiguity tests |
| `crates/litrpg-core/tests/ledger.rs` | Fold unit + property tests |
| `crates/litrpg-core/tests/validate.rs` | One test per validation rule |
| `crates/litrpg-store/Cargo.toml` | Store manifest |
| `crates/litrpg-store/src/lib.rs` | `Store`, error type, re-exports |
| `crates/litrpg-store/src/migrations.rs` | Schema DDL + `user_version` migration runner |
| `crates/litrpg-store/src/ledger.rs` | `append_delta`, `snapshot`, `known_subjects`, `rewind` |
| `crates/litrpg-store/src/chapters.rs` | Chapter insert/query |
| `crates/litrpg-store/tests/store.rs` | Integration tests against in-memory SQLite |

Tests live in `tests/` (integration tests) rather than `#[cfg(test)]` modules **on purpose**: `litrpg-core` is `#![no_std]`, and integration tests are separate crates that link std, so proptest works without `no_std` gymnastics.

---

### Task 1: Workspace scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `crates/litrpg-core/Cargo.toml`
- Create: `crates/litrpg-core/src/lib.rs`

- [ ] **Step 1: Create the workspace root**

`Cargo.toml`:

```toml
[workspace]
members = ["crates/litrpg-core", "crates/litrpg-store"]
resolver = "3"

[workspace.package]
edition = "2024"
license = "MIT OR Apache-2.0"
rust-version = "1.97"

[workspace.dependencies]
serde = { version = "1.0.229", default-features = false, features = ["derive", "alloc"] }
serde_json = { version = "1.0.151", default-features = false, features = ["alloc"] }
thiserror = "2.0.19"
rusqlite = { version = "0.40.1", features = ["bundled"] }
proptest = "1.11.0"

litrpg-core = { path = "crates/litrpg-core" }
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
```

- [ ] **Step 2: Create the core crate manifest**

`crates/litrpg-core/Cargo.toml`:

```toml
[package]
name = "litrpg-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "no_std wire types shared by the endless-litrpg daemon and the ESP32-C6 watch firmware"

[dependencies]
serde = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
serde_json = { version = "1.0.151", features = ["std"] }
```

- [ ] **Step 3: Create a minimal `no_std` lib that compiles**

`crates/litrpg-core/src/lib.rs`:

```rust
//! Shared wire types for the endless-litrpg system.
//!
//! This crate is `no_std` + `alloc` and contains **no I/O**. It is depended on by
//! the daemon (std) and by the ESP32-C6 watch firmware (bare metal), so both agree
//! on the wire format by construction rather than by convention.

#![no_std]

extern crate alloc;
```

- [ ] **Step 4: Verify it builds**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo build -p litrpg-core
```

Expected: `Finished` with no errors. (Warnings about an empty crate are fine.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml crates/litrpg-core/Cargo.toml crates/litrpg-core/src/lib.rs
git commit -m "feat(core): scaffold no_std workspace and core crate"
```

---

### Task 2: `VoiceRef` — first-colon-only parsing

The spec (§7.3) warns this bites if left implicit: Azure voice names *contain* a colon, so `split(':')` into three parts is right for sherpa and wrong for Azure.

**Files:**
- Create: `crates/litrpg-core/src/voice.rs`
- Modify: `crates/litrpg-core/src/lib.rs`
- Test: `crates/litrpg-core/tests/voice.rs`

- [ ] **Step 1: Write the failing tests**

`crates/litrpg-core/tests/voice.rs`:

```rust
use litrpg_core::voice::{VoiceRef, VoiceRefError};

#[test]
fn parses_sherpa_ref() {
    let v = VoiceRef::parse("sherpa:piper-en_GB-cori:0").unwrap();
    assert_eq!(v.backend, "sherpa");
    assert_eq!(v.remainder, "piper-en_GB-cori:0");
}

#[test]
fn azure_voice_name_keeps_its_own_colon() {
    // The bug this test exists to prevent: a naive split(':') into three parts
    // truncates the Azure voice name at "en-GB-Ada".
    let v = VoiceRef::parse("azure:en-GB-Ada:DragonHDLatestNeural").unwrap();
    assert_eq!(v.backend, "azure");
    assert_eq!(v.remainder, "en-GB-Ada:DragonHDLatestNeural");
}

#[test]
fn round_trips_through_display() {
    let raw = "azure:en-GB-Ada:DragonHDLatestNeural";
    assert_eq!(VoiceRef::parse(raw).unwrap().to_string(), raw);
}

#[test]
fn rejects_malformed_refs() {
    assert_eq!(VoiceRef::parse("sherpa"), Err(VoiceRefError::MissingColon));
    assert_eq!(VoiceRef::parse(":piper"), Err(VoiceRefError::EmptyBackend));
    assert_eq!(VoiceRef::parse("sherpa:"), Err(VoiceRefError::EmptyRemainder));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test voice
```

Expected: FAIL — `unresolved import litrpg_core::voice`.

- [ ] **Step 3: Implement**

`crates/litrpg-core/src/voice.rs`:

```rust
//! Fully-qualified voice references: `backend_id` `:` backend-specific remainder.

use alloc::string::{String, ToString};
use core::fmt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceRefError {
    MissingColon,
    EmptyBackend,
    EmptyRemainder,
}

impl fmt::Display for VoiceRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColon => f.write_str("voice_ref must contain ':'"),
            Self::EmptyBackend => f.write_str("voice_ref backend is empty"),
            Self::EmptyRemainder => f.write_str("voice_ref remainder is empty"),
        }
    }
}

/// A voice reference such as `sherpa:kokoro-multi-lang-v1_0:18` or
/// `azure:en-GB-Ada:DragonHDLatestNeural`.
///
/// Split on the **first colon only**. The remainder is opaque to the engine and
/// parsed by the owning plugin — Azure voice names legitimately contain colons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceRef {
    pub backend: String,
    pub remainder: String,
}

impl VoiceRef {
    pub fn parse(s: &str) -> Result<Self, VoiceRefError> {
        let (backend, remainder) = s.split_once(':').ok_or(VoiceRefError::MissingColon)?;
        if backend.is_empty() {
            return Err(VoiceRefError::EmptyBackend);
        }
        if remainder.is_empty() {
            return Err(VoiceRefError::EmptyRemainder);
        }
        Ok(Self {
            backend: backend.to_string(),
            remainder: remainder.to_string(),
        })
    }
}

impl fmt::Display for VoiceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.backend, self.remainder)
    }
}
```

Append to `crates/litrpg-core/src/lib.rs`:

```rust
pub mod voice;

pub use voice::{VoiceRef, VoiceRefError};
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test voice
```

Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-core/src/voice.rs crates/litrpg-core/src/lib.rs crates/litrpg-core/tests/voice.rs
git commit -m "feat(core): VoiceRef with first-colon-only parsing"
```

---

### Task 3: `Segment` and `Manifest` — PCM offset arithmetic

Spec §8.1. Byte offsets are derivable from `ms × 32` but precomputed on purpose so the watch does zero arithmetic and can issue a `Range` request straight from the manifest.

**Files:**
- Create: `crates/litrpg-core/src/manifest.rs`
- Modify: `crates/litrpg-core/src/lib.rs`
- Test: `crates/litrpg-core/tests/manifest.rs`

- [ ] **Step 1: Write the failing tests**

`crates/litrpg-core/tests/manifest.rs`:

```rust
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind, BYTES_PER_MS, SAMPLE_RATE_HZ};
use proptest::prelude::*;

fn seg(idx: u32, start_ms: u32, end_ms: u32) -> Segment {
    Segment {
        idx,
        speaker: "narrator".into(),
        kind: SpeakerKind::Narrator,
        voice_ref: "sherpa:piper-en_GB-cori:0".into(),
        text: "The vale smelled of iron and wet ash.".into(),
        start_ms,
        end_ms,
    }
}

#[test]
fn pcm_constants_match_16khz_mono_s16le() {
    assert_eq!(SAMPLE_RATE_HZ, 16_000);
    // 16000 samples/sec * 2 bytes/sample / 1000 ms
    assert_eq!(BYTES_PER_MS, 32);
}

#[test]
fn byte_offsets_are_ms_times_32() {
    let s = seg(0, 0, 4120);
    assert_eq!(s.start_byte(), 0);
    assert_eq!(s.end_byte(), 131_840);
    assert_eq!(s.duration_ms(), 4120);
}

#[test]
fn manifest_derives_duration_and_total_bytes() {
    let m = Manifest::new(42, vec![seg(0, 0, 4120), seg(1, 4120, 9000)]);
    assert_eq!(m.chapter, 42);
    assert_eq!(m.sample_rate, 16_000);
    assert_eq!(m.duration_ms, 9000);
    assert_eq!(m.total_bytes(), 288_000);
}

#[test]
fn empty_manifest_is_zero_length_not_a_panic() {
    let m = Manifest::new(1, vec![]);
    assert_eq!(m.duration_ms, 0);
    assert_eq!(m.total_bytes(), 0);
    assert!(m.is_contiguous());
    assert!(m.segment_at_ms(0).is_none());
}

#[test]
fn segment_at_ms_uses_half_open_intervals() {
    let m = Manifest::new(1, vec![seg(0, 0, 100), seg(1, 100, 200)]);
    assert_eq!(m.segment_at_ms(0).unwrap().idx, 0);
    assert_eq!(m.segment_at_ms(99).unwrap().idx, 0);
    assert_eq!(m.segment_at_ms(100).unwrap().idx, 1);
    assert!(m.segment_at_ms(200).is_none());
}

#[test]
fn detects_a_gap_between_segments() {
    let m = Manifest::new(1, vec![seg(0, 0, 100), seg(1, 150, 200)]);
    assert!(!m.is_contiguous());
}

#[test]
fn round_trips_through_json() {
    let m = Manifest::new(42, vec![seg(0, 0, 4120)]);
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
}

proptest! {
    /// The invariant the watch's Range requests depend on.
    #[test]
    fn start_byte_always_equals_start_ms_times_32(start_ms in 0u32..10_000_000) {
        let s = seg(0, start_ms, start_ms.saturating_add(1));
        prop_assert_eq!(s.start_byte(), start_ms as u64 * 32);
    }

    /// A contiguous manifest's total byte count is exactly the sum of its segments'.
    #[test]
    fn total_bytes_equals_sum_of_segment_bytes(lens in prop::collection::vec(1u32..5_000, 1..40)) {
        let mut segments = Vec::new();
        let mut cursor = 0u32;
        for (i, len) in lens.iter().enumerate() {
            segments.push(seg(i as u32, cursor, cursor + len));
            cursor += len;
        }
        let m = Manifest::new(1, segments);
        prop_assert!(m.is_contiguous());
        let summed: u64 = m.segments.iter().map(|s| s.end_byte() - s.start_byte()).sum();
        prop_assert_eq!(m.total_bytes(), summed);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test manifest
```

Expected: FAIL — `unresolved import litrpg_core::manifest`.

- [ ] **Step 3: Implement**

`crates/litrpg-core/src/manifest.rs`:

```rust
//! Chapter audio manifest — the artifact that drives Range requests and
//! sentence highlighting on every client.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::voice::{VoiceRef, VoiceRefError};

/// Every TTS plugin normalizes to this rate (spec §7.1). It is byte-for-byte what
/// the watch's `audio_out::play_pcm` consumes, which is why no decoder exists.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// 16 kHz * 2 bytes/sample / 1000 = 32 bytes per millisecond, exactly.
pub const BYTES_PER_MS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeakerKind {
    Narrator,
    Character,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub idx: u32,
    pub speaker: String,
    pub kind: SpeakerKind,
    pub voice_ref: String,
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
}

impl Segment {
    pub fn start_byte(&self) -> u64 {
        self.start_ms as u64 * BYTES_PER_MS as u64
    }

    pub fn end_byte(&self) -> u64 {
        self.end_ms as u64 * BYTES_PER_MS as u64
    }

    pub fn duration_ms(&self) -> u32 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    pub fn voice(&self) -> Result<VoiceRef, VoiceRefError> {
        VoiceRef::parse(&self.voice_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub chapter: u32,
    pub sample_rate: u32,
    pub bytes_per_ms: u32,
    pub duration_ms: u32,
    pub segments: Vec<Segment>,
}

impl Manifest {
    pub fn new(chapter: u32, segments: Vec<Segment>) -> Self {
        let duration_ms = segments.last().map(|s| s.end_ms).unwrap_or(0);
        Self {
            chapter,
            sample_rate: SAMPLE_RATE_HZ,
            bytes_per_ms: BYTES_PER_MS,
            duration_ms,
            segments,
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.duration_ms as u64 * BYTES_PER_MS as u64
    }

    /// Half-open lookup: `start_ms <= ms < end_ms`.
    pub fn segment_at_ms(&self, ms: u32) -> Option<&Segment> {
        self.segments.iter().find(|s| ms >= s.start_ms && ms < s.end_ms)
    }

    /// True when segments start at 0 and leave no gaps — required for the byte
    /// offsets to address one continuous PCM stream.
    pub fn is_contiguous(&self) -> bool {
        self.segments.first().map(|s| s.start_ms == 0).unwrap_or(true)
            && self.segments.windows(2).all(|w| w[0].end_ms == w[1].start_ms)
    }
}
```

Append to `crates/litrpg-core/src/lib.rs`:

```rust
pub mod manifest;

pub use manifest::{Manifest, Segment, SpeakerKind, BYTES_PER_MS, SAMPLE_RATE_HZ};
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test manifest
```

Expected: PASS, 7 unit tests + 2 property tests.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-core/src/manifest.rs crates/litrpg-core/src/lib.rs crates/litrpg-core/tests/manifest.rs
git commit -m "feat(core): Segment/Manifest with exact PCM byte offsets"
```

---

### Task 4: The ledger fold

Spec §6.1 — the load-bearing decision. There is no `characters.hp` column; state is a fold.

**Files:**
- Create: `crates/litrpg-core/src/ledger.rs`
- Modify: `crates/litrpg-core/src/lib.rs`
- Test: `crates/litrpg-core/tests/ledger.rs`

- [ ] **Step 1: Write the failing tests**

`crates/litrpg-core/tests/ledger.rs`:

```rust
use litrpg_core::ledger::{fold, rewind, LedgerEntry, Op, Value};
use proptest::prelude::*;

fn entry(seq: u64, chapter: u32, subject: &str, field: &str, op: Op, n: i64) -> LedgerEntry {
    LedgerEntry {
        seq,
        chapter,
        subject: subject.into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
        applied: true,
    }
}

#[test]
fn set_then_add_then_sub() {
    let e = vec![
        entry(1, 1, "Kaelen", "hp", Op::Set, 100),
        entry(2, 1, "Kaelen", "hp", Op::Sub, 30),
        entry(3, 2, "Kaelen", "hp", Op::Add, 10),
    ];
    assert_eq!(fold(&e).num("Kaelen", "hp"), Some(80));
}

#[test]
fn add_to_absent_field_treats_it_as_zero() {
    let e = vec![entry(1, 1, "Kaelen", "xp", Op::Add, 250)];
    assert_eq!(fold(&e).num("Kaelen", "xp"), Some(250));
}

#[test]
fn rejected_entries_are_inert() {
    let mut rejected = entry(2, 1, "Kaelen", "hp", Op::Sub, 999);
    rejected.applied = false;
    let e = vec![entry(1, 1, "Kaelen", "hp", Op::Set, 100), rejected];
    assert_eq!(fold(&e).num("Kaelen", "hp"), Some(100));
}

#[test]
fn text_values_are_set_only() {
    let e = vec![LedgerEntry {
        seq: 1,
        chapter: 1,
        subject: "Kaelen".into(),
        field: "location".into(),
        op: Op::Set,
        value_num: None,
        value_txt: Some("Ashen Vale".into()),
        applied: true,
    }];
    assert_eq!(fold(&e).txt("Kaelen", "location"), Some("Ashen Vale"));
}

#[test]
fn arithmetic_against_a_text_value_is_recorded_as_an_anomaly() {
    let e = vec![
        LedgerEntry {
            seq: 1,
            chapter: 1,
            subject: "Kaelen".into(),
            field: "location".into(),
            op: Op::Set,
            value_num: None,
            value_txt: Some("Ashen Vale".into()),
            applied: true,
        },
        entry(2, 1, "Kaelen", "location", Op::Add, 5),
    ];
    let snap = fold(&e);
    assert_eq!(snap.txt("Kaelen", "location"), Some("Ashen Vale"));
    assert_eq!(snap.anomalies.len(), 1);
}

#[test]
fn subjects_are_enumerated() {
    let e = vec![
        entry(1, 1, "Kaelen", "hp", Op::Set, 100),
        entry(2, 1, "Vessa", "hp", Op::Set, 80),
    ];
    let snap = fold(&e);
    let subjects = snap.subjects();
    assert!(subjects.contains("Kaelen"));
    assert!(subjects.contains("Vessa"));
    assert_eq!(subjects.len(), 2);
}

#[test]
fn rewind_includes_the_boundary_chapter() {
    let e = vec![
        entry(1, 40, "Kaelen", "hp", Op::Set, 100),
        entry(2, 41, "Kaelen", "hp", Op::Sub, 50),
    ];
    let kept = rewind(&e, 40);
    assert_eq!(kept.len(), 1);
    assert_eq!(fold(&kept).num("Kaelen", "hp"), Some(100));
}

proptest! {
    /// The fold sorts by `seq` itself, so input order cannot change the result.
    /// This is what lets the store hand over rows in any order SQLite returns them.
    #[test]
    fn fold_is_order_independent(mut deltas in prop::collection::vec(-50i64..50, 1..30)) {
        let entries: Vec<LedgerEntry> = deltas
            .iter()
            .enumerate()
            .map(|(i, d)| entry(i as u64 + 1, 1, "Kaelen", "gold", Op::Add, *d))
            .collect();

        let forward = fold(&entries);
        let mut reversed = entries.clone();
        reversed.reverse();
        prop_assert_eq!(forward.num("Kaelen", "gold"), fold(&reversed).num("Kaelen", "gold"));

        deltas.reverse(); // keep the generated value used, avoids an unused warning
        prop_assert_eq!(forward.num("Kaelen", "gold"), Some(deltas.iter().sum::<i64>()));
    }

    /// Flipping every entry to rejected must yield an empty snapshot.
    #[test]
    fn all_rejected_yields_empty_snapshot(n in 1usize..20) {
        let entries: Vec<LedgerEntry> = (0..n)
            .map(|i| {
                let mut e = entry(i as u64 + 1, 1, "Kaelen", "hp", Op::Add, 10);
                e.applied = false;
                e
            })
            .collect();
        prop_assert!(fold(&entries).values.is_empty());
    }

    /// rewind(N) keeps exactly the entries at or before chapter N.
    #[test]
    fn rewind_keeps_prefix(chapters in prop::collection::vec(1u32..100, 1..40), cut in 1u32..100) {
        let entries: Vec<LedgerEntry> = chapters
            .iter()
            .enumerate()
            .map(|(i, c)| entry(i as u64 + 1, *c, "Kaelen", "xp", Op::Add, 1))
            .collect();
        let kept = rewind(&entries, cut);
        prop_assert!(kept.iter().all(|e| e.chapter <= cut));
        prop_assert_eq!(kept.len(), entries.iter().filter(|e| e.chapter <= cut).count());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test ledger
```

Expected: FAIL — `unresolved import litrpg_core::ledger`.

- [ ] **Step 3: Implement**

`crates/litrpg-core/src/ledger.rs`:

```rust
//! The append-only ledger and its fold.
//!
//! Current state is never stored. It is computed by folding
//! `ledger WHERE applied = 1 ORDER BY seq`, which is what makes `rewind N` free:
//! deactivate rows past chapter N and the snapshot is simply correct again.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Set,
    Add,
    Sub,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    Num(i64),
    Txt(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub seq: u64,
    pub chapter: u32,
    pub subject: String,
    pub field: String,
    pub op: Op,
    pub value_num: Option<i64>,
    pub value_txt: Option<String>,
    /// `false` means the validation gate rejected it. Kept for audit; inert in the fold.
    pub applied: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateSnapshot {
    pub values: BTreeMap<(String, String), Value>,
    /// Malformed-but-applied entries. Non-fatal by design: the fold is total so a
    /// bookkeeping oddity can never panic the engine mid-chapter.
    pub anomalies: Vec<String>,
}

impl StateSnapshot {
    pub fn num(&self, subject: &str, field: &str) -> Option<i64> {
        match self.values.get(&(String::from(subject), String::from(field))) {
            Some(Value::Num(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn txt(&self, subject: &str, field: &str) -> Option<&str> {
        match self.values.get(&(String::from(subject), String::from(field))) {
            Some(Value::Txt(t)) => Some(t.as_str()),
            _ => None,
        }
    }

    pub fn subjects(&self) -> BTreeSet<&str> {
        self.values.keys().map(|(s, _)| s.as_str()).collect()
    }
}

/// Fold ledger entries into current state. Sorts by `seq` internally, so callers
/// may pass rows in any order.
pub fn fold(entries: &[LedgerEntry]) -> StateSnapshot {
    let mut ordered: Vec<&LedgerEntry> = entries.iter().filter(|e| e.applied).collect();
    ordered.sort_by_key(|e| e.seq);

    let mut snap = StateSnapshot::default();
    for e in ordered {
        let key = (e.subject.clone(), e.field.clone());
        match e.op {
            Op::Set => {
                if let Some(n) = e.value_num {
                    snap.values.insert(key, Value::Num(n));
                } else if let Some(t) = &e.value_txt {
                    snap.values.insert(key, Value::Txt(t.clone()));
                } else {
                    snap.anomalies.push(anomaly(e, "set with no value"));
                }
            }
            Op::Add | Op::Sub => {
                let Some(magnitude) = e.value_num else {
                    snap.anomalies.push(anomaly(e, "add/sub with no numeric value"));
                    continue;
                };
                let signed = if matches!(e.op, Op::Sub) { -magnitude } else { magnitude };
                match snap.values.get(&key) {
                    Some(Value::Num(cur)) => {
                        let next = cur.saturating_add(signed);
                        snap.values.insert(key, Value::Num(next));
                    }
                    Some(Value::Txt(_)) => {
                        snap.anomalies.push(anomaly(e, "add/sub against a text value"));
                    }
                    None => {
                        snap.values.insert(key, Value::Num(signed));
                    }
                }
            }
        }
    }
    snap
}

fn anomaly(e: &LedgerEntry, why: &str) -> String {
    alloc::format!("seq {} {}.{}: {}", e.seq, e.subject, e.field, why)
}

/// Keep only entries at or before `through_chapter` (inclusive).
pub fn rewind(entries: &[LedgerEntry], through_chapter: u32) -> Vec<LedgerEntry> {
    entries
        .iter()
        .filter(|e| e.chapter <= through_chapter)
        .cloned()
        .collect()
}
```

Append to `crates/litrpg-core/src/lib.rs`:

```rust
pub mod ledger;

pub use ledger::{fold, rewind, LedgerEntry, Op, StateSnapshot, Value};
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test ledger
```

Expected: PASS, 7 unit tests + 3 property tests.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-core/src/ledger.rs crates/litrpg-core/src/lib.rs crates/litrpg-core/tests/ledger.rs
git commit -m "feat(core): append-only ledger with order-independent fold"
```

---

### Task 5: The validation gate

Spec §6.2. Ember only *proposes* deltas; this is what stops stat drift becoming canon.

**Files:**
- Create: `crates/litrpg-core/src/validate.rs`
- Modify: `crates/litrpg-core/src/lib.rs`
- Test: `crates/litrpg-core/tests/validate.rs`

- [ ] **Step 1: Write the failing tests**

`crates/litrpg-core/tests/validate.rs`:

```rust
use litrpg_core::ledger::{fold, LedgerEntry, Op};
use litrpg_core::validate::{validate_delta, Delta, Rejection};
use std::collections::BTreeSet;

fn known(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| String::from(*s)).collect()
}

fn set(subject: &str, field: &str, n: i64) -> LedgerEntry {
    LedgerEntry {
        seq: 1,
        chapter: 1,
        subject: subject.into(),
        field: field.into(),
        op: Op::Set,
        value_num: Some(n),
        value_txt: None,
        applied: true,
    }
}

fn delta(subject: &str, field: &str, op: Op, n: i64) -> Delta {
    Delta {
        subject: subject.into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
    }
}

#[test]
fn accepts_a_plain_damage_delta() {
    let snap = fold(&[set("Kaelen", "hp", 100), {
        let mut e = set("Kaelen", "max_hp", 100);
        e.seq = 2;
        e
    }]);
    let d = delta("Kaelen", "hp", Op::Sub, 12);
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_unknown_subject() {
    let snap = fold(&[]);
    let d = delta("Kaelenn", "hp", Op::Sub, 5); // typo
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownSubject)
    );
}

#[test]
fn rejects_unknown_field() {
    let snap = fold(&[]);
    let d = delta("Kaelen", "charisma", Op::Set, 18);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownField)
    );
}

#[test]
fn rejects_hp_below_zero() {
    let snap = fold(&[set("Kaelen", "hp", 10)]);
    let d = delta("Kaelen", "hp", Op::Sub, 50);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::HpBelowZero)
    );
}

#[test]
fn rejects_hp_above_max() {
    let snap = fold(&[set("Kaelen", "hp", 50), {
        let mut e = set("Kaelen", "max_hp", 100);
        e.seq = 2;
        e
    }]);
    let d = delta("Kaelen", "hp", Op::Add, 500);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::HpAboveMax { max: 100 })
    );
}

#[test]
fn allows_hp_above_current_when_max_is_unknown() {
    let snap = fold(&[set("Kaelen", "hp", 50)]);
    let d = delta("Kaelen", "hp", Op::Add, 500);
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_level_decrease() {
    let snap = fold(&[set("Kaelen", "level", 7)]);
    let d = delta("Kaelen", "level", Op::Set, 6);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::LevelWouldDecrease)
    );
}

#[test]
fn rejects_xp_decrease() {
    let snap = fold(&[set("Kaelen", "xp", 4000)]);
    let d = delta("Kaelen", "xp", Op::Sub, 1);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::XpWouldDecrease)
    );
}

#[test]
fn rejects_negative_inventory() {
    let snap = fold(&[set("Kaelen", "inv:ration", 2)]);
    let d = delta("Kaelen", "inv:ration", Op::Sub, 5);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::InventoryWouldGoNegative)
    );
}

#[test]
fn accepts_inventory_within_bounds() {
    let snap = fold(&[set("Kaelen", "inv:ration", 5)]);
    let d = delta("Kaelen", "inv:ration", Op::Sub, 5);
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn text_fields_require_set_with_text() {
    let snap = fold(&[]);
    let subjects = known(&["Kaelen"]);

    let ok = Delta {
        subject: "Kaelen".into(),
        field: "location".into(),
        op: Op::Set,
        value_num: None,
        value_txt: Some("Ashen Vale".into()),
    };
    assert_eq!(validate_delta(&snap, &subjects, &ok), Ok(()));

    let no_text = Delta {
        subject: "Kaelen".into(),
        field: "location".into(),
        op: Op::Set,
        value_num: None,
        value_txt: None,
    };
    assert_eq!(
        validate_delta(&snap, &subjects, &no_text),
        Err(Rejection::MissingTextValue)
    );

    let arithmetic = delta("Kaelen", "location", Op::Add, 1);
    assert_eq!(
        validate_delta(&snap, &subjects, &arithmetic),
        Err(Rejection::TextFieldRequiresSet)
    );
}

#[test]
fn numeric_field_without_a_number_is_rejected() {
    let snap = fold(&[]);
    let d = Delta {
        subject: "Kaelen".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: None,
        value_txt: Some("lots".into()),
    };
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::MissingNumericValue)
    );
}

fn text_delta(field: &str, op: Op, value: Option<&str>) -> Delta {
    Delta {
        subject: "Kaelen".into(),
        field: field.into(),
        op,
        value_num: None,
        value_txt: value.map(String::from),
    }
}

#[test]
fn accepts_equipping_a_whitelisted_slot() {
    let snap = fold(&[]);
    let d = text_delta("equip:main_hand", Op::Set, Some("Ashen Blade"));
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn empty_string_unequips_a_slot() {
    let snap = fold(&[]);
    let d = text_delta("equip:cloak", Op::Set, Some(""));
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_an_invented_equipment_slot() {
    let snap = fold(&[]);
    let d = text_delta("equip:third_arm", Op::Set, Some("Spare Sword"));
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownEquipSlot)
    );
}

#[test]
fn rejects_arithmetic_on_an_equipment_slot() {
    let snap = fold(&[]);
    let d = delta("Kaelen", "equip:head", Op::Add, 1);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::TextFieldRequiresSet)
    );
}

#[test]
fn accepts_a_whitelisted_appearance_trait() {
    let snap = fold(&[]);
    let d = text_delta("appear:hair", Op::Set, Some("black, shorn at the temples"));
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_an_invented_appearance_trait() {
    let snap = fold(&[]);
    let d = text_delta("appear:aura", Op::Set, Some("crackling violet"));
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownAppearTrait)
    );
}

#[test]
fn all_eleven_slots_are_accepted() {
    let snap = fold(&[]);
    let subjects = known(&["Kaelen"]);
    for slot in litrpg_core::validate::EQUIP_SLOTS {
        let d = text_delta(&format!("equip:{slot}"), Op::Set, Some("Something"));
        assert_eq!(validate_delta(&snap, &subjects, &d), Ok(()), "slot {slot}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core --test validate
```

Expected: FAIL — `unresolved import litrpg_core::validate`.

- [ ] **Step 3: Implement**

`crates/litrpg-core/src/validate.rs`:

```rust
//! The validation gate (spec §6.2).
//!
//! Ember *proposes* deltas; this decides whether they become canon. Pure and
//! `no_std` so it is trivially testable and cannot depend on I/O ordering.

use alloc::collections::BTreeSet;
use alloc::string::String;

use crate::ledger::{Op, StateSnapshot};

pub const NUMERIC_FIELDS: &[&str] = &["hp", "max_hp", "level", "xp", "gold"];
pub const TEXT_FIELDS: &[&str] = &["location", "status"];

/// Inventory counts are dynamic field names: `inv:<item>`. Item names are free-form.
pub const INVENTORY_PREFIX: &str = "inv:";

/// Equipped items: `equip:<slot>`. The slot is whitelisted because each slot is a
/// row on the watch's character screen — an invented slot would break the renderer.
pub const EQUIP_PREFIX: &str = "equip:";
pub const EQUIP_SLOTS: &[&str] = &[
    "head", "chest", "legs", "feet", "hands", "cloak", "main_hand", "off_hand", "amulet",
    "ring1", "ring2",
];

/// Appearance descriptors: `appear:<trait>`. Whitelisted for the same reason.
pub const APPEAR_PREFIX: &str = "appear:";
pub const APPEAR_TRAITS: &[&str] = &["hair", "eyes", "skin", "build", "height", "notable"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub subject: String,
    pub field: String,
    pub op: Op,
    pub value_num: Option<i64>,
    pub value_txt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    UnknownSubject,
    UnknownField,
    MissingNumericValue,
    MissingTextValue,
    TextFieldRequiresSet,
    HpBelowZero,
    HpAboveMax { max: i64 },
    LevelWouldDecrease,
    XpWouldDecrease,
    InventoryWouldGoNegative,
    UnknownEquipSlot,
    UnknownAppearTrait,
}

/// `known_subjects` is the union of cast speakers, existing ledger subjects, and
/// `lore` rows of kind `character`. Pass 2's `new_lore` is applied *before* its
/// deltas, so a character introduced this chapter is already known here.
pub fn validate_delta(
    snap: &StateSnapshot,
    known_subjects: &BTreeSet<String>,
    d: &Delta,
) -> Result<(), Rejection> {
    if !known_subjects.contains(&d.subject) {
        return Err(Rejection::UnknownSubject);
    }

    // Prefixed namespaces are checked first so an unknown slot or trait is
    // reported precisely rather than as a generic UnknownField.
    if let Some(slot) = d.field.strip_prefix(EQUIP_PREFIX) {
        if !EQUIP_SLOTS.contains(&slot) {
            return Err(Rejection::UnknownEquipSlot);
        }
        return validate_text_set(d);
    }

    if let Some(trait_name) = d.field.strip_prefix(APPEAR_PREFIX) {
        if !APPEAR_TRAITS.contains(&trait_name) {
            return Err(Rejection::UnknownAppearTrait);
        }
        return validate_text_set(d);
    }

    if TEXT_FIELDS.contains(&d.field.as_str()) {
        return validate_text_set(d);
    }

    let is_inventory = d.field.starts_with(INVENTORY_PREFIX);
    if !is_inventory && !NUMERIC_FIELDS.contains(&d.field.as_str()) {
        return Err(Rejection::UnknownField);
    }

    let magnitude = d.value_num.ok_or(Rejection::MissingNumericValue)?;
    let current = snap.num(&d.subject, &d.field).unwrap_or(0);
    let next = match d.op {
        Op::Set => magnitude,
        Op::Add => current.saturating_add(magnitude),
        Op::Sub => current.saturating_sub(magnitude),
    };

    if is_inventory {
        if next < 0 {
            return Err(Rejection::InventoryWouldGoNegative);
        }
        return Ok(());
    }

    match d.field.as_str() {
        "hp" => {
            if next < 0 {
                return Err(Rejection::HpBelowZero);
            }
            if let Some(max) = snap.num(&d.subject, "max_hp") {
                if next > max {
                    return Err(Rejection::HpAboveMax { max });
                }
            }
        }
        "level" if next < current => return Err(Rejection::LevelWouldDecrease),
        "xp" if next < current => return Err(Rejection::XpWouldDecrease),
        _ => {}
    }

    Ok(())
}

/// Text-valued fields are absolute assignments only — arithmetic is meaningless on
/// them. An **empty string is legal** and means "slot is empty" / "trait unknown".
fn validate_text_set(d: &Delta) -> Result<(), Rejection> {
    if !matches!(d.op, Op::Set) {
        return Err(Rejection::TextFieldRequiresSet);
    }
    if d.value_txt.is_none() {
        return Err(Rejection::MissingTextValue);
    }
    Ok(())
}
```

Append to `crates/litrpg-core/src/lib.rs`:

```rust
pub mod validate;

pub use validate::{validate_delta, Delta, Rejection};
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-core
```

Expected: PASS — all four test files green.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-core/src/validate.rs crates/litrpg-core/src/lib.rs crates/litrpg-core/tests/validate.rs
git commit -m "feat(core): validation gate for proposed state deltas"
```

---

### Task 6: Store scaffold and schema migrations

**Files:**
- Create: `crates/litrpg-store/Cargo.toml`
- Create: `crates/litrpg-store/src/lib.rs`
- Create: `crates/litrpg-store/src/migrations.rs`
- Test: `crates/litrpg-store/tests/store.rs`

- [ ] **Step 1: Write the failing test**

`crates/litrpg-store/tests/store.rs`:

```rust
use litrpg_store::Store;

#[test]
fn opens_in_memory_and_applies_migrations() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);
}

#[test]
fn all_expected_tables_exist() {
    let store = Store::open_in_memory().unwrap();
    let mut tables = store.table_names().unwrap();
    tables.sort();
    assert_eq!(
        tables,
        vec![
            "cast", "chapters", "ledger", "lore", "notes", "segments", "story", "summaries",
        ]
    );
}

#[test]
fn migrations_are_idempotent() {
    let store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-store
```

Expected: FAIL — `error: package ID specification 'litrpg-store' did not match any packages`.

- [ ] **Step 3: Implement**

`crates/litrpg-store/Cargo.toml`:

```toml
[package]
name = "litrpg-store"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "SQLite persistence for endless-litrpg: chapters, lore, and the append-only ledger"

[dependencies]
litrpg-core = { workspace = true }
rusqlite = { workspace = true }
thiserror = { workspace = true }
serde_json = { version = "1.0.151", features = ["std"] }
```

`crates/litrpg-store/src/migrations.rs`:

```rust
//! Schema DDL, versioned through SQLite's `user_version` pragma.

/// Index N holds the migration that moves the schema from version N to N+1.
pub const MIGRATIONS: &[&str] = &[include_str!("schema/001_initial.sql")];

pub const TARGET_VERSION: i64 = MIGRATIONS.len() as i64;
```

`crates/litrpg-store/src/schema/001_initial.sql`:

```sql
CREATE TABLE story (
    id              INTEGER PRIMARY KEY,
    title           TEXT    NOT NULL,
    protagonist     TEXT    NOT NULL DEFAULT '',
    prompt_path     TEXT    NOT NULL,
    prompt_hash     TEXT    NOT NULL DEFAULT '',
    arc_outline_md  TEXT    NOT NULL DEFAULT '',
    target_words    INTEGER NOT NULL DEFAULT 2000,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE chapters (
    id            INTEGER PRIMARY KEY,
    number        INTEGER NOT NULL UNIQUE,
    title         TEXT    NOT NULL,
    text_md       TEXT    NOT NULL,
    prompt_hash   TEXT    NOT NULL DEFAULT '',
    pcm_path      TEXT,
    mp3_path      TEXT,
    manifest_json TEXT,
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    has_audio     INTEGER NOT NULL DEFAULT 0,
    state_dirty   INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE segments (
    id         INTEGER PRIMARY KEY,
    chapter_id INTEGER NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL,
    speaker    TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    text       TEXT    NOT NULL,
    voice_ref  TEXT    NOT NULL,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    UNIQUE (chapter_id, idx)
);

CREATE TABLE cast (
    id            INTEGER PRIMARY KEY,
    speaker       TEXT    NOT NULL UNIQUE,
    voice_ref     TEXT    NOT NULL,
    kind          TEXT    NOT NULL,
    first_chapter INTEGER NOT NULL
);

CREATE TABLE lore (
    id              INTEGER PRIMARY KEY,
    name            TEXT    NOT NULL UNIQUE,
    kind            TEXT    NOT NULL,
    keywords        TEXT    NOT NULL DEFAULT '',
    body_md         TEXT    NOT NULL,
    priority        INTEGER NOT NULL DEFAULT 0,
    always_on       INTEGER NOT NULL DEFAULT 0,
    updated_chapter INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE ledger (
    id         INTEGER PRIMARY KEY,
    chapter_id INTEGER,
    chapter    INTEGER NOT NULL,
    seq        INTEGER NOT NULL UNIQUE,
    subject    TEXT    NOT NULL,
    field      TEXT    NOT NULL,
    op         TEXT    NOT NULL,
    value_num  INTEGER,
    value_txt  TEXT,
    reason     TEXT    NOT NULL DEFAULT '',
    applied    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

CREATE INDEX ledger_seq_idx ON ledger (seq);
CREATE INDEX ledger_chapter_idx ON ledger (chapter);

CREATE TABLE summaries (
    id      INTEGER PRIMARY KEY,
    level   INTEGER NOT NULL,
    from_ch INTEGER NOT NULL,
    to_ch   INTEGER NOT NULL,
    body_md TEXT    NOT NULL
);

CREATE TABLE notes (
    id               INTEGER PRIMARY KEY,
    body             TEXT    NOT NULL,
    source           TEXT    NOT NULL,
    created_at       INTEGER NOT NULL,
    consumed_chapter INTEGER
);
```

`crates/litrpg-store/src/lib.rs`:

```rust
//! SQLite persistence. The only crate in the workspace that writes state.

pub mod migrations;

use rusqlite::Connection;
use thiserror::Error;

use migrations::{MIGRATIONS, TARGET_VERSION};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("chapter {0} not found")]
    ChapterNotFound(u32),
    #[error("delta rejected: {0}")]
    Rejected(String),
}

pub type Result<T> = core::result::Result<T, StoreError>;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.configure()?;
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.configure()?;
        store.migrate()?;
        Ok(store)
    }

    fn configure(&self) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Apply any migrations the database has not yet seen. Idempotent.
    pub fn migrate(&self) -> Result<()> {
        let current = self.schema_version()?;
        for (i, sql) in MIGRATIONS.iter().enumerate() {
            let version = i as i64;
            if version < current {
                continue;
            }
            self.conn.execute_batch(sql)?;
        }
        if current < TARGET_VERSION {
            self.conn
                .pragma_update(None, "user_version", TARGET_VERSION)?;
        }
        Ok(())
    }

    pub fn table_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

Note the WAL pragma from the spec is deliberately **not** set here — it fails on in-memory databases. It belongs in `open()` only, added in Task 8 alongside the on-disk path test.

- [ ] **Step 4: Run tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-store
```

Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-store/
git commit -m "feat(store): SQLite schema with user_version migrations"
```

---

### Task 7: Ledger persistence — append with validation, snapshot, rewind

**Files:**
- Create: `crates/litrpg-store/src/ledger.rs`
- Modify: `crates/litrpg-store/src/lib.rs`
- Test: `crates/litrpg-store/tests/ledger.rs`

- [ ] **Step 1: Write the failing tests**

`crates/litrpg-store/tests/ledger.rs`:

```rust
use litrpg_core::ledger::Op;
use litrpg_core::validate::{Delta, Rejection};
use litrpg_store::Store;

fn store_with_kaelen() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.upsert_cast("Kaelen", "sherpa:kokoro-multi-lang-v1_0:18", "character", 1).unwrap();
    store
}

fn delta(field: &str, op: Op, n: i64) -> Delta {
    Delta {
        subject: "Kaelen".into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
    }
}

#[test]
fn cast_members_are_known_subjects() {
    let store = store_with_kaelen();
    assert!(store.known_subjects().unwrap().contains("Kaelen"));
}

#[test]
fn accepted_delta_lands_in_the_snapshot() {
    let store = store_with_kaelen();
    store.append_delta(1, &delta("hp", Op::Set, 100)).unwrap();
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
}

#[test]
fn seq_increments_across_appends() {
    let store = store_with_kaelen();
    store.append_delta(1, &delta("hp", Op::Set, 100)).unwrap();
    store.append_delta(1, &delta("hp", Op::Sub, 40)).unwrap();
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(60));
}

#[test]
fn rejected_delta_is_stored_but_inert() {
    let store = store_with_kaelen();
    store.append_delta(1, &delta("hp", Op::Set, 10)).unwrap();

    let outcome = store.append_delta(1, &delta("hp", Op::Sub, 999)).unwrap();
    assert_eq!(outcome, Err(Rejection::HpBelowZero));

    // Still 10 — the rejection did not apply.
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(10));
    // But it was recorded for audit.
    assert_eq!(store.rejected_count().unwrap(), 1);
}

#[test]
fn unknown_subject_is_rejected_not_silently_created() {
    let store = store_with_kaelen();
    let d = Delta {
        subject: "Kaelenn".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: Some(50),
        value_txt: None,
    };
    assert_eq!(store.append_delta(1, &d).unwrap(), Err(Rejection::UnknownSubject));
    assert!(store.snapshot().unwrap().num("Kaelenn", "hp").is_none());
}

#[test]
fn rewind_deactivates_later_chapters() {
    let store = store_with_kaelen();
    store.append_delta(40, &delta("hp", Op::Set, 100)).unwrap();
    store.append_delta(41, &delta("hp", Op::Sub, 60)).unwrap();
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(40));

    let touched = store.rewind(40).unwrap();
    assert_eq!(touched, 1);
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
}

#[test]
fn rewind_is_idempotent() {
    let store = store_with_kaelen();
    store.append_delta(40, &delta("hp", Op::Set, 100)).unwrap();
    store.append_delta(41, &delta("hp", Op::Sub, 60)).unwrap();
    store.rewind(40).unwrap();
    assert_eq!(store.rewind(40).unwrap(), 0);
    assert_eq!(store.snapshot().unwrap().num("Kaelen", "hp"), Some(100));
}

#[test]
fn lore_characters_are_known_subjects_too() {
    let store = Store::open_in_memory().unwrap();
    store.upsert_lore("Vessa", "character", "vessa,thief", "A thief.", 0, false, 3).unwrap();
    assert!(store.known_subjects().unwrap().contains("Vessa"));
    let d = Delta {
        subject: "Vessa".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: Some(70),
        value_txt: None,
    };
    assert_eq!(store.append_delta(3, &d).unwrap(), Ok(()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-store --test ledger
```

Expected: FAIL — `no method named upsert_cast found for struct Store`.

- [ ] **Step 3: Implement**

`crates/litrpg-store/src/ledger.rs`:

```rust
//! Ledger persistence: append-with-validation, snapshot, rewind.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use litrpg_core::ledger::{fold, LedgerEntry, Op, StateSnapshot};
use litrpg_core::validate::{validate_delta, Delta, Rejection};
use rusqlite::params;

use crate::{Result, Store};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn op_str(op: Op) -> &'static str {
    match op {
        Op::Set => "set",
        Op::Add => "add",
        Op::Sub => "sub",
    }
}

fn op_from_str(s: &str) -> Op {
    match s {
        "add" => Op::Add,
        "sub" => Op::Sub,
        _ => Op::Set,
    }
}

impl Store {
    pub fn upsert_cast(
        &self,
        speaker: &str,
        voice_ref: &str,
        kind: &str,
        first_chapter: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cast (speaker, voice_ref, kind, first_chapter) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(speaker) DO UPDATE SET voice_ref = excluded.voice_ref",
            params![speaker, voice_ref, kind, first_chapter],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_lore(
        &self,
        name: &str,
        kind: &str,
        keywords: &str,
        body_md: &str,
        priority: i64,
        always_on: bool,
        updated_chapter: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO lore (name, kind, keywords, body_md, priority, always_on, updated_chapter)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(name) DO UPDATE SET
                 kind = excluded.kind,
                 keywords = excluded.keywords,
                 body_md = excluded.body_md,
                 priority = excluded.priority,
                 always_on = excluded.always_on,
                 updated_chapter = excluded.updated_chapter",
            params![name, kind, keywords, body_md, priority, always_on as i64, updated_chapter],
        )?;
        Ok(())
    }

    /// Cast speakers ∪ existing ledger subjects ∪ lore entries of kind `character`.
    pub fn known_subjects(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT speaker FROM cast
             UNION SELECT subject FROM ledger
             UNION SELECT name FROM lore WHERE kind = 'character'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<BTreeSet<_>>>()?)
    }

    fn entries(&self) -> Result<Vec<LedgerEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, chapter, subject, field, op, value_num, value_txt, applied
             FROM ledger ORDER BY seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LedgerEntry {
                seq: r.get::<_, i64>(0)? as u64,
                chapter: r.get::<_, i64>(1)? as u32,
                subject: r.get(2)?,
                field: r.get(3)?,
                op: op_from_str(&r.get::<_, String>(4)?),
                value_num: r.get(5)?,
                value_txt: r.get(6)?,
                applied: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn snapshot(&self) -> Result<StateSnapshot> {
        Ok(fold(&self.entries()?))
    }

    fn next_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM ledger", [], |r| {
                r.get(0)
            })?)
    }

    /// Validate a proposed delta and record it either way.
    ///
    /// The outer `Result` is I/O failure; the inner one is the gate's verdict. A
    /// rejection is **not** an error — it is a stored, auditable outcome.
    pub fn append_delta(
        &self,
        chapter: u32,
        d: &Delta,
    ) -> Result<core::result::Result<(), Rejection>> {
        let snapshot = self.snapshot()?;
        let known = self.known_subjects()?;
        let verdict = validate_delta(&snapshot, &known, d);

        let (applied, reason) = match &verdict {
            Ok(()) => (1i64, String::new()),
            Err(r) => (0i64, format!("{r:?}")),
        };

        self.conn.execute(
            "INSERT INTO ledger
                (chapter, seq, subject, field, op, value_num, value_txt, reason, applied, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                chapter,
                self.next_seq()?,
                d.subject,
                d.field,
                op_str(d.op),
                d.value_num,
                d.value_txt,
                reason,
                applied,
                now_ms(),
            ],
        )?;

        Ok(verdict)
    }

    pub fn rejected_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM ledger WHERE applied = 0", [], |r| {
                r.get(0)
            })?)
    }

    /// Deactivate every applied entry after `through_chapter`. Returns how many
    /// rows changed, so a second call on an already-rewound ledger returns 0.
    pub fn rewind(&self, through_chapter: u32) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE ledger SET applied = 0, reason = 'rewound'
             WHERE chapter > ?1 AND applied = 1",
            params![through_chapter],
        )?)
    }
}
```

Add to `crates/litrpg-store/src/lib.rs`, after `pub mod migrations;`:

```rust
pub mod ledger;
```

And make the connection reachable from the sibling module by changing the field declaration:

```rust
pub struct Store {
    pub(crate) conn: Connection,
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-store
```

Expected: PASS — 3 schema tests + 8 ledger tests.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-store/src/ledger.rs crates/litrpg-store/src/lib.rs crates/litrpg-store/tests/ledger.rs
git commit -m "feat(store): ledger append with validation gate, snapshot, and rewind"
```

---

### Task 8: Chapter persistence

**Files:**
- Create: `crates/litrpg-store/src/chapters.rs`
- Modify: `crates/litrpg-store/src/lib.rs`
- Test: `crates/litrpg-store/tests/chapters.rs`

- [ ] **Step 1: Write the failing tests**

`crates/litrpg-store/tests/chapters.rs`:

```rust
use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use litrpg_store::{NewChapter, Store};

fn manifest(chapter: u32) -> Manifest {
    Manifest::new(
        chapter,
        vec![Segment {
            idx: 0,
            speaker: "narrator".into(),
            kind: SpeakerKind::Narrator,
            voice_ref: "sherpa:piper-en_GB-cori:0".into(),
            text: "The vale smelled of iron and wet ash.".into(),
            start_ms: 0,
            end_ms: 4120,
        }],
    )
}

fn new_chapter(number: u32) -> NewChapter {
    NewChapter {
        number,
        title: format!("Chapter {number}"),
        text_md: "[narrator] The vale smelled of iron and wet ash.".into(),
        prompt_hash: "abc123".into(),
        state_dirty: false,
    }
}

#[test]
fn inserts_and_reads_back_a_chapter() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();

    let ch = store.chapter(1).unwrap();
    assert_eq!(ch.number, 1);
    assert_eq!(ch.title, "Chapter 1");
    assert!(!ch.has_audio);
    assert_eq!(ch.duration_ms, 0);
}

#[test]
fn missing_chapter_is_an_error_not_a_panic() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.chapter(99).is_err());
}

#[test]
fn attaching_audio_persists_manifest_segments_and_duration() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    store
        .attach_audio(1, &manifest(1), "media/0001.pcm", "media/0001.mp3")
        .unwrap();

    let ch = store.chapter(1).unwrap();
    assert!(ch.has_audio);
    assert_eq!(ch.duration_ms, 4120);

    let segments = store.segments(1).unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].voice_ref, "sherpa:piper-en_GB-cori:0");
    assert_eq!(segments[0].start_byte(), 0);
    assert_eq!(segments[0].end_byte(), 131_840);
}

#[test]
fn attaching_audio_twice_replaces_rather_than_duplicates_segments() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.attach_audio(1, &manifest(1), "a.pcm", "a.mp3").unwrap();
    store.attach_audio(1, &manifest(1), "b.pcm", "b.mp3").unwrap();
    assert_eq!(store.segments(1).unwrap().len(), 1);
}

#[test]
fn chapters_since_returns_ascending_numbers_only_after_the_cursor() {
    let store = Store::open_in_memory().unwrap();
    for n in 1..=4 {
        store.insert_chapter(&new_chapter(n)).unwrap();
    }
    let numbers: Vec<u32> = store
        .chapters_since(2)
        .unwrap()
        .iter()
        .map(|c| c.number)
        .collect();
    assert_eq!(numbers, vec![3, 4]);
}

#[test]
fn latest_number_tracks_the_highest_chapter() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.latest_number().unwrap(), 0);
    store.insert_chapter(&new_chapter(1)).unwrap();
    store.insert_chapter(&new_chapter(2)).unwrap();
    assert_eq!(store.latest_number().unwrap(), 2);
}

#[test]
fn state_dirty_chapters_can_be_listed_for_re_extraction() {
    let store = Store::open_in_memory().unwrap();
    store.insert_chapter(&new_chapter(1)).unwrap();
    let mut dirty = new_chapter(2);
    dirty.state_dirty = true;
    store.insert_chapter(&dirty).unwrap();

    assert_eq!(store.dirty_chapters().unwrap(), vec![2]);
}

#[test]
fn on_disk_store_round_trips(){
    let dir = std::env::temp_dir().join(format!("litrpg-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("story.db");

    {
        let store = Store::open(&path).unwrap();
        store.insert_chapter(&new_chapter(7)).unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.chapter(7).unwrap().title, "Chapter 7");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p litrpg-store --test chapters
```

Expected: FAIL — `unresolved import litrpg_store::NewChapter`.

- [ ] **Step 3: Implement**

`crates/litrpg-store/src/chapters.rs`:

```rust
//! Chapter and segment persistence.

use std::time::{SystemTime, UNIX_EPOCH};

use litrpg_core::manifest::{Manifest, Segment, SpeakerKind};
use rusqlite::params;

use crate::{Result, Store, StoreError};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn kind_str(k: SpeakerKind) -> &'static str {
    match k {
        SpeakerKind::Narrator => "narrator",
        SpeakerKind::Character => "character",
        SpeakerKind::System => "system",
    }
}

fn kind_from_str(s: &str) -> SpeakerKind {
    match s {
        "character" => SpeakerKind::Character,
        "system" => SpeakerKind::System,
        _ => SpeakerKind::Narrator,
    }
}

/// Input for inserting a chapter. Audio is attached separately, because text
/// ships even when rendering fails (spec §10).
#[derive(Debug, Clone)]
pub struct NewChapter {
    pub number: u32,
    pub title: String,
    pub text_md: String,
    pub prompt_hash: String,
    pub state_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct ChapterRow {
    pub number: u32,
    pub title: String,
    pub text_md: String,
    pub prompt_hash: String,
    pub pcm_path: Option<String>,
    pub mp3_path: Option<String>,
    pub duration_ms: u32,
    pub has_audio: bool,
    pub state_dirty: bool,
}

impl Store {
    pub fn insert_chapter(&self, ch: &NewChapter) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chapters (number, title, text_md, prompt_hash, state_dirty, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                ch.number,
                ch.title,
                ch.text_md,
                ch.prompt_hash,
                ch.state_dirty as i64,
                now_ms()
            ],
        )?;
        Ok(())
    }

    fn row_to_chapter(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChapterRow> {
        Ok(ChapterRow {
            number: r.get::<_, i64>(0)? as u32,
            title: r.get(1)?,
            text_md: r.get(2)?,
            prompt_hash: r.get(3)?,
            pcm_path: r.get(4)?,
            mp3_path: r.get(5)?,
            duration_ms: r.get::<_, i64>(6)? as u32,
            has_audio: r.get::<_, i64>(7)? != 0,
            state_dirty: r.get::<_, i64>(8)? != 0,
        })
    }

    const CHAPTER_COLUMNS: &'static str =
        "number, title, text_md, prompt_hash, pcm_path, mp3_path, duration_ms, has_audio, state_dirty";

    pub fn chapter(&self, number: u32) -> Result<ChapterRow> {
        let sql = format!(
            "SELECT {} FROM chapters WHERE number = ?1",
            Self::CHAPTER_COLUMNS
        );
        self.conn
            .query_row(&sql, params![number], Self::row_to_chapter)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StoreError::ChapterNotFound(number),
                other => StoreError::Sqlite(other),
            })
    }

    pub fn chapters_since(&self, after: u32) -> Result<Vec<ChapterRow>> {
        let sql = format!(
            "SELECT {} FROM chapters WHERE number > ?1 ORDER BY number",
            Self::CHAPTER_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![after], Self::row_to_chapter)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn latest_number(&self) -> Result<u32> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(number), 0) FROM chapters",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    pub fn dirty_chapters(&self) -> Result<Vec<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT number FROM chapters WHERE state_dirty = 1 ORDER BY number")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|n| n as u32)
            .collect())
    }

    /// Attach rendered audio: manifest JSON, per-segment rows, duration, paths.
    /// Replaces any previous segments so a re-render cannot duplicate them.
    pub fn attach_audio(
        &self,
        number: u32,
        manifest: &Manifest,
        pcm_path: &str,
        mp3_path: &str,
    ) -> Result<()> {
        let chapter_id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM chapters WHERE number = ?1",
                params![number],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => StoreError::ChapterNotFound(number),
                other => StoreError::Sqlite(other),
            })?;

        let json = serde_json::to_string(manifest)?;

        self.conn.execute(
            "UPDATE chapters
             SET manifest_json = ?1, pcm_path = ?2, mp3_path = ?3,
                 duration_ms = ?4, has_audio = 1
             WHERE id = ?5",
            params![json, pcm_path, mp3_path, manifest.duration_ms, chapter_id],
        )?;

        self.conn.execute(
            "DELETE FROM segments WHERE chapter_id = ?1",
            params![chapter_id],
        )?;

        for s in &manifest.segments {
            self.conn.execute(
                "INSERT INTO segments
                    (chapter_id, idx, speaker, kind, text, voice_ref, start_ms, end_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    chapter_id,
                    s.idx,
                    s.speaker,
                    kind_str(s.kind),
                    s.text,
                    s.voice_ref,
                    s.start_ms,
                    s.end_ms
                ],
            )?;
        }

        Ok(())
    }

    pub fn segments(&self, number: u32) -> Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.idx, s.speaker, s.kind, s.text, s.voice_ref, s.start_ms, s.end_ms
             FROM segments s
             JOIN chapters c ON c.id = s.chapter_id
             WHERE c.number = ?1
             ORDER BY s.idx",
        )?;
        let rows = stmt.query_map(params![number], |r| {
            Ok(Segment {
                idx: r.get::<_, i64>(0)? as u32,
                speaker: r.get(1)?,
                kind: kind_from_str(&r.get::<_, String>(2)?),
                text: r.get(3)?,
                voice_ref: r.get(4)?,
                start_ms: r.get::<_, i64>(5)? as u32,
                end_ms: r.get::<_, i64>(6)? as u32,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

Add to `crates/litrpg-store/src/lib.rs` after `pub mod ledger;`:

```rust
pub mod chapters;

pub use chapters::{ChapterRow, NewChapter};
```

Also enable WAL for on-disk databases only — replace `configure` in `lib.rs`:

```rust
    fn configure(&self, on_disk: bool) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        if on_disk {
            // WAL is invalid for :memory: databases.
            self.conn
                .pragma_update(None, "journal_mode", "WAL")?;
        }
        Ok(())
    }
```

and update the two call sites: `store.configure(true)?` in `open`, `store.configure(false)?` in `open_in_memory`.

- [ ] **Step 4: Run the whole suite**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test
```

Expected: PASS — all tests in both crates.

- [ ] **Step 5: Commit**

```bash
git add crates/litrpg-store/src/chapters.rs crates/litrpg-store/src/lib.rs crates/litrpg-store/tests/chapters.rs
git commit -m "feat(store): chapter and segment persistence with audio attachment"
```

---

### Task 9: Lint clean and document the crates

**Files:**
- Modify: `crates/litrpg-core/src/lib.rs`
- Modify: `crates/litrpg-store/src/lib.rs`

- [ ] **Step 1: Run clippy and formatting**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo fmt --all && cargo clippy --all-targets -- -D warnings
```

Expected: no warnings. Fix anything reported — do not silence with `#[allow]` unless the lint is genuinely wrong for the case.

- [ ] **Step 2: Verify `litrpg-core` really is `no_std`**

The crate declares `#![no_std]`, so any accidental `std` use is a compile error already. Confirm nothing crept in through a dependency:

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo tree -p litrpg-core -e normal
```

Expected: only `serde` (and its `serde_derive`/`serde_core` internals). If `std` appears as an enabled feature on serde, the workspace dependency's `default-features = false` was lost.

- [ ] **Step 3: Run the full suite one final time**

```bash
export PATH="$HOME/.cargo/bin:$PATH" && cargo test
```

Expected: PASS, everything green.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "chore: clippy clean and rustfmt across core and store"
```

---

## What Phase 1a delivers

Two library crates with the foundation everything else builds on:

- Wire types the watch firmware can depend on verbatim, with byte-offset arithmetic proven by property test.
- A voice-reference parser that handles Azure's embedded colon — the bug the spec called out.
- An append-only ledger whose fold is order-independent and whose `rewind` is a filter.
- A validation gate with a test per rule, so stat drift cannot become canon.
- SQLite persistence with versioned migrations, storing rejections for audit rather than discarding them.

## Follow-on plans

| Plan | Scope |
|---|---|
| **1b** | `litrpg-ember` — client, prompt assembly, two-pass contract, tagged-prose parser |
| **1c** | `litrpg-tts` — `TtsBackend` registry, sherpa plugin (process pool), azure plugin (multi-voice SSML) |
| **1d** | `litrpg-engine` + `litrpg-daemon` + `litrpg-cli` — the loop, HTTP/RSS, realm-sigil, CLI |
| **2** | Watch Story app (`esp32c6-watch`) — playback, chapter reader, stats screen, character/equipment screen (spec §9.4.1) |
| **3** | `source-endless` Candela module |
