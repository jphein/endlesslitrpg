# Endless LitRPG — Design Spec

**Date:** 2026-07-29
**Status:** design — awaiting JP's review
**Repo:** `~/Projects/endlesslitrpg` (new, `main`)
**Language:** Rust (`no_std`-compatible core, so the watch firmware shares wire types)

---

## 0. TL;DR

A Rust daemon on `familiar` drives the local Qwen model (**Ember**) to write an endless LitRPG
serial. Each chapter is persisted as **text + audio**, narrated by a **multi-voice cast** (one voice
per character, plus a robotic `SYSTEM` voice for RPG stat blocks). The story's initial prompt is a
git-tracked file JP edits in `$EDITOR`.

Four things make this tractable rather than sprawling:

1. **The engine, not the model, owns state.** Ember *proposes* stat deltas; a validation gate accepts
   or rejects them. Current state is a fold over an append-only ledger — which is what makes
   `rewind 40` free.
2. **TTS is a plugin layer with two first-class backends** (sherpa-onnx local, Azure DragonHD), both
   normalized to one output format at the boundary: 16 kHz mono s16le raw PCM.
3. **That format is not arbitrary** — it is byte-for-byte what the watch's existing
   `audio_out::play_pcm` already consumes, so the watch needs no decoder, no resampler, and no
   seek index.
4. **A podcast RSS feed is the day-one client.** Fancy clients (watch, Candela) become polish rather
   than blockers.

---

## 1. Goals

- An **endless** LitRPG serial that generates continuously without JP's involvement.
- Every chapter archived as **markdown + audio**, permanently.
- **Multi-voice narration** — per-character voices from a real catalogue, assigned automatically and
  persisted.
- JP can **edit the story prompt** at any time; changes take effect at the next chapter boundary.
- JP can **steer without blocking** the engine, via director notes that land at chapter boundaries.
- **Numeric consistency** — levels, HP, XP, inventory and quests must not drift.
- A **Rust watch client** on the ESP32-C6 that reads and plays chapters.

## 2. Non-goals (explicit YAGNI)

- **No interactive branching.** JP chose passive-plus-nudges; the engine never blocks on input.
  (Section 6.4 keeps the door open at the schema level, but no branch machinery is built.)
- **No LLM or TTS on the watch.** 512 KB SRAM, no PSRAM. The watch is a thin client, permanently.
- **No multi-user, no accounts, no cloud.** Single-tenant, LAN-only, plain HTTP.
- **No web UI in Phase 1.** The RSS feed plus the CLI is the interface.
- **No adopting a writing app.** Manuskript / novelWriter / bibisco / Plottr are GUI tools with no
  machine API. The lorebook pattern is ~200 lines of Rust; it is not a dependency.
- **No mempalace as primary store.** See §4.5.

---

## 3. Decisions made (and by whom)

| # | Decision | Rationale |
|---|---|---|
| D1 | **Rust**, not Go | Shared `no_std` wire types with the ESP32-C6 firmware; one language across daemon and watch |
| D2 | **Passive + director nudges** | Engine never blocks; notes land at chapter boundaries. Superset of pure-passive, far simpler than interactive |
| D3 | **Speaker-tagged, stitched cast** | Per-character voices + a distinct `SYSTEM` voice. Chosen over narrator-only and over Azure `en-Multitalker` |
| D4 | **Always-on daemon on `familiar`** | Sits next to Ember (no network hop for large prompts); GPUs and uptime are local |
| D5 | **Daemon + Candela source module** | Reuses Candela's reader, playback, highlighting and 36-source plugin system rather than rebuilding them |
| D6 | **sherpa-onnx and Azure are both first-class plugins** | Neither is a fallback. Registry, not trait-with-a-favourite |
| D7 | **Narrator = Piper `cori`** | JP's pick. en_GB, which preserves Ember's accent signature (§4.4) |
| D8 | **Watch on the `admin` SSID** | Puts it on `10.0.6.0/24` alongside `familiar` — no firewall rule needed at all |
| D9 | **Playback stays fully visible** | Screen on with live highlighting. Battery is untested, so measure it rather than design around it (§9.5) |

---

## 4. What already exists (inventory before building)

### 4.1 Ember — the local model

| | |
|---|---|
| Service | `qwen3-coder.service` — llama.cpp, Qwen3.6-35B-A3B MoE, both GPUs |
| Endpoint | `http://familiar:8091` — OpenAI-compatible (`/v1/models`, `/v1/chat/completions`) |
| Model id | `qwen36-coder` |
| Context | ~131 k |
| Roster voice | `en-GB-Ada:DragonHDLatestNeural` — **reserved**; never a Claude agent (`agent-orchestration.md:140`) |

**GPU headroom is zero.** Both P102-100s sit at ~9.7/10 GB, held by Ember. Any local TTS runs on
**CPU**, and the renderer must never contend with this service.

### 4.2 Azure Speech (via speech-to-cli)

- Key + region live in `~/.config/speech-to-cli/config.json` (`tts_region` = `eastus`, DragonHD is
  region-limited; `region` = `westus` for STT). The Azure plugin **reads this same config** rather
  than defining parallel key custody — so a voice change there propagates here.
- 769 voices available, **103 in the DragonHD family**, including `en-Multitalker`.
- **Multi-voice SSML in a single request works** (`speech-to-cli/speech_tts.py:191`) — one `<voice
  name=…>` element per segment, one audio stream back. This is what the Azure plugin's
  `render_batch` exploits.
- **`raw-16khz-16bit-mono-pcm` returns 200 OK for DragonHD** — measured by Morpheus 2026-07-27.

### 4.3 The watch — `~/Projects/esp32c6-watch`

100% Rust `no_std`, esp-hal + Embassy, edition 2024, target `riscv32imac-unknown-none-elf`.

| | |
|---|---|
| Board | Waveshare ESP32-C6-Touch-AMOLED-2.06 |
| MCU | ESP32-C6, single-core RISC-V @ 160 MHz |
| Memory | **512 KB SRAM, no PSRAM** |
| Display | CO5300 AMOLED 410×502, QSPI + DMA, Slint `no_std` renderer |
| Audio out | **ES8311 mono speaker codec**, shared I²S bus |
| Flash | 16 MB, A/B OTA (two 6 MB slots) |
| Radios | WiFi 6, BLE 5, 802.15.4 |

Already built and directly reusable:

- `src/peripherals/audio_out.rs:181` — `pub fn play_pcm(pcm: &[u8]) -> usize`
- `src/net/voice_tts.rs` — `POST /tts` → streams raw 16 kHz PCM through a **512 B window** with
  backpressure from an 8-slot playback channel; ≈2.4 KB peak cost; `cancel()`; zero new Embassy tasks
- `src/net/voice_stt.rs` — push-to-talk mic capture → LAN STT gateway (the director-note input path)
- `src/apps/mod.rs` — app registry; each launcher app is a single registration
- `src/apps/session.rs` — app switcher with suspend/resume
- Volume system (0–15 + mute), per-subsystem current estimation, battery monitoring

**Prior art that must be read before implementing Phase 2:**
`esp32c6-watch/docs/superpowers/specs/2026-07-27-tts-notify-readaloud-design.md`. It establishes the
no-decode PCM path and documents an **amp-gate deadlock** (its §6.2) that, unhandled, ships *silent
while logging complete success*.

**One divergence from that spec:** it routes through a LAN bridge (`watch_bridge.py`, active on
`ubox0:8090`) because the bridge holds the Azure key. Story chapters are **pre-rendered and hold no
secret**, so the watch talks to `litrpg-daemon` directly. One less hop, one less service to keep
alive.

### 4.4 sherpa-onnx — measured on `familiar` 2026-07-29 (Reverie)

Verdict: **GO**, CPU-only, GPUs verified untouched before and after.

| Model | Speakers | RTF @ 8 threads | 13 min of audio |
|---|---|---|---|
| Piper `libritts_r-medium` (94 MB) | **904**, unlabeled | 29.2× | ~27 s |
| Kokoro `multi-lang-v1_0` (384 MB) | **53**, named/labeled | 5.28× | ~148 s (~100 s with 4 workers) |

- **Speaker selection is an integer `sid` at synthesis time, no model reload.** The cast is a
  first-class primitive.
- **Process pool beats thread pool**: 4 workers × 4 threads yields **+48%** (Kokoro) / **+86%**
  (Piper) over one 8-thread process. **12 threads regresses** — contention with llama.cpp. Never set
  threads to core count.
- **`sherpa-rs` 0.6.8** is a near-1:1 port of the Python API; `download-binaries` avoids the C++
  build. `cargo` is **not yet installed on familiar** — not a blocker.
- **No model has a robotic voice.** `SYSTEM` requires a post-render ffmpeg pass (verified chain in
  §7.4).
- Kokoro sid map (recovered from JP's own `VoxSherpa-TTS`): 0–10 Am-F, 11–19 Am-M, 20–23 Br-F,
  24–27 Br-M, 28–52 other languages.
- Steady-state disk: ~533 MB (models + venv), at `/home/jp/tts-spike/models/`.

**Assumption A1 — `cori`.** D7 names Piper `cori`, a single-speaker en_GB model distinct from
`libritts_r`'s 904 sids. Sourcing and benchmarking is in flight. If it cannot be obtained as a
sherpa-loadable VITS model, the narrator default falls back to Kokoro `bf_emma` (21) or `bm_george`
(26), preserving the en-GB signature. **Nothing else in this design changes** — voices are config,
not code.

**Why en-GB matters:** `agent-orchestration.md:144` reserves en-GB as Ember's accent signature, the
one JP classifies by before parsing content. A British narrator keeps that rule true while the local
model literally narrates, instead of quietly violating it.

### 4.5 mempalace — secondary, not primary

The CLI offers `mine`, `search`, `cypher`, `wake-up`, `tunnels` — but **no arbitrary-write API**;
writes happen by mining files. It is also a network hop and cannot hold numbers. Asking a vector
store for a character's exact HP is a category error.

**Use:** mine finished chapter markdown into the existing `endlesslitrpg` wing, async and
best-effort, for long-range semantic recall ("what did we establish about the Ashen Vale?"). Never in
the generation critical path; a mempalace outage must not stall a chapter.

### 4.6 Candela — the reader that already exists

`~/Projects/candela`, Kotlin/Android, Play package `org.techempower.candela`, namespace
`in.jphe.storyvox`. **36 `source-*` modules** behind a KSP-generated plugin registry, plus
`core-llm`, `core-playback`, `core-sync`, `core-ui`, `core-source-testkit`, `core-voice-testkit`.

`core-data/.../source/FictionSource.kt` is described as a "read-side abstraction over a
fiction-hosting site": `popular`, `latestUpdates`, `byGenre`, `search`, `fictionDetail`,
`chapterContent`, with a `FictionResult` error model. A daemon serving endless generated chapters *is*
a fiction-hosting site — `source-endless` becomes the 37th sibling with no impedance mismatch.

---

## 5. Architecture

```
endlesslitrpg/
├── crates/
│   ├── litrpg-core/     # no_std + serde: Chapter, Segment, VoiceRef, Manifest, StateSnapshot
│   ├── litrpg-store/    # SQLite via rusqlite; migrations; the ledger fold
│   ├── litrpg-ember/    # OpenAI-compatible client, prompt assembly, the two-pass contract
│   ├── litrpg-tts/      # TtsBackend trait + registry; sherpa + azure plugins behind features
│   ├── litrpg-engine/   # the loop: schedule → generate → validate → render → publish
│   └── litrpg-daemon/   # axum: HTTP + RSS + realm-sigil /api/version
└── litrpg-cli/          # prompt, note, status, cast, render, rewind
```

**`litrpg-core` is the crate that earns D1.** It is `no_std`-compatible with zero engine
dependencies, so the watch firmware depends on *exactly* the types the daemon serves. No parallel
struct definitions, no drift between server and firmware.

### 5.1 The chapter pipeline

1. **Schedule** — wake; if the rendered-ahead buffer is below target (2–3 chapters), proceed;
   otherwise idle. Production outruns consumption by ~5× (a ~13-minute chapter takes ~1–3 minutes to
   produce), so a small buffer suffices — no overnight batches needed.
2. **Assemble prompt** — story prompt + arc outline + *derived* state snapshot + keyword-matched lore
   + last N chapter summaries + pending director notes.
3. **Ember pass 1 (creative)** — unconstrained, temp ~0.9. Prose with inline speaker tags.
4. **Parse → segments** — deterministic. Unknown speakers mint a `cast` row and draw a voice.
5. **Ember pass 2 (extraction)** — `json_schema`-constrained, temp 0, over the finished chapter.
6. **Validate → ledger** — accepted deltas appended; rejected ones stored with `applied=0` + reason.
7. **Render** — `render_batch` over the segments → one 16 kHz PCM stream + manifest with offsets.
8. **Publish** — `chapters/NNNN.{md,pcm,mp3,json}`; update RSS.
9. **Mine** (optional, async) — chapter markdown → mempalace `endlesslitrpg` wing.

Every stage is **idempotent by chapter number**; a crash resumes from the last completed stage.

---

## 6. Data model

```sql
story    (id, title, prompt_path, prompt_hash, arc_outline_md, target_words, updated_at)
chapters (id, number, title, text_md, prompt_hash, pcm_path, mp3_path,
          manifest_json, duration_ms, has_audio, state_dirty, created_at)
segments (id, chapter_id, idx, speaker, text, voice_ref, start_ms, end_ms)
cast     (id, speaker, voice_ref, kind, first_chapter)   -- narrator | character | system
lore     (id, name, kind, keywords, body_md, priority, always_on, updated_chapter)
ledger   (id, chapter_id, seq, subject, field, op, value_num, value_txt,
          reason, applied, created_at)
summaries(id, level, from_ch, to_ch, body_md)            -- 0=chapter 1=arc 2=book
notes    (id, body, source, created_at, consumed_chapter)
```

### 6.0 Defaults and enumerations

Stated explicitly so implementation has nothing to guess:

| Field | Values / default |
|---|---|
| `story.target_words` | **2000** — ≈13 min of audio at ~150 wpm narration. Tunable per story |
| `ledger.op` | `set` \| `add` \| `sub` (`set` writes `value_num`/`value_txt` absolutely; `add`/`sub` are relative and numeric only) |
| `cast.kind` | `narrator` \| `character` \| `system` |
| `lore.kind` | `character` \| `place` \| `item` \| `faction` \| `rule` |
| `summaries.level` | `0` chapter · `1` arc · `2` book |
| `notes.source` | `cli` \| `watch` \| `candela` |
| `chapters.state_dirty` | `0` normally; `1` when pass 2 failed and deltas were never extracted |

Buffer target is **3** rendered-ahead chapters (minimum 2), and `max_hp` is itself a ledger field, so
the clamp in §6.2 reads the folded snapshot rather than a constant.

### 6.1 There is no `characters.hp` column

Current state is a **fold over `ledger WHERE applied = 1 ORDER BY seq`**. This is the load-bearing
decision in the whole data model:

- `rewind N` deactivates ledger rows past chapter N and the snapshot is simply correct again — no
  compensating updates, no unwinding.
- Regenerating forward from chapter 40 cannot corrupt state.
- A mutable state table would need a full audit log to support any of this, so the ledger is not
  extra work — it is the same work, arranged so it composes.

### 6.2 The validation gate

Applied before anything is written:

- Field **whitelist** — unknown fields rejected.
- `hp` clamped to `0..=max_hp`.
- `level` monotonic non-decreasing.
- `xp` non-decreasing.
- Inventory counts non-negative.
- Unknown `subject` rejected (prevents the model inventing a character sheet by typo).

Rejections are **stored, not discarded**. A rising `applied=0` rate is an early warning that the
prompt or state format is drifting — measurable, instead of discovering at chapter 60 that HP went
negative at chapter 31.

### 6.3 Lorebook retrieval

The SillyTavern/KoboldAI pattern: entries carry `keywords`; an entry is injected when a keyword
appears in the recent-context scan window (last chapter's text + arc outline), plus all `always_on`
entries, ordered by `priority`.

**Context budget** — Ember has ~131 k and we use roughly 6 k: state snapshot ~1 k, matched lore ~2 k,
last five chapter summaries ~1 k, arc summary, notes, story prompt. Enormous headroom, so retrieval
can be generous.

**Previous chapters are never fed back verbatim.** This is not only a token optimization: raw prose in
context makes the model mimic its own recent phrasing and the story collapses into a loop. Summaries
carry facts without carrying cadence.

### 6.4 Director notes

`notes` rows are consumed at chapter boundaries and stamped with `consumed_chapter`. Sources: `cli`,
`watch` (push-to-talk → STT), `candela`. The engine never waits for one.

This is also where interactivity would attach later if D2 is ever revisited — a choice is a note with
a stronger contract. No branch machinery is built now.

---

## 7. The TTS plugin layer

Both backends are **first class** (D6), registered at startup, each behind a cargo feature.

```rust
trait TtsBackend {
    fn id(&self) -> &str;                          // "sherpa" | "azure"
    fn available(&self) -> Availability;           // models present? key present?
    fn voices(&self) -> Vec<VoiceDesc>;            // id, label, lang, gender, cost_class
    async fn render(&self, seg: &Segment) -> Result<Pcm16k>;
    async fn render_batch(&self, segs: &[Segment]) -> Result<Vec<Pcm16k>> { /* default: loop */ }
}
```

### 7.1 The output contract

**Every plugin returns 16 kHz mono s16le raw PCM** — headerless. Exactly `32,000 B/s` = `32 B/ms`.

Normalizing at the plugin boundary is what makes mixing backends *within one chapter* safe. If
plugins returned native rates, every consumer would need a resampler and the joins would be a
rate-mismatch minefield. One rate at the boundary makes concatenation `Vec::extend`.

### 7.2 `render_batch` is the point of the design

It lets each plugin play to its strength without leaking into the engine:

- **Azure** overrides it to emit **one multi-voice SSML request** for the whole chapter (§4.2).
- **sherpa** overrides it to **fan out across a process pool, sharded by model** — 4 threads per
  worker, one model per worker (cori and Kokoro are different models).

The engine calls `render_batch` and does not know which happened.

### 7.3 Voice references

Fully qualified, so the cast table can mix freely:

```
sherpa:piper-en_GB-cori:0             → narrator (D7 default)
sherpa:kokoro-multi-lang-v1_0:18      → am_puck, protagonist
sherpa:kokoro-multi-lang-v1_0:27      → bm_lewis, antagonist
azure:en-GB-Ada:DragonHDLatestNeural  → guest / hero moments
```

**Parsing rule — this bites if left implicit.** A `voice_ref` is `backend_id` + `:` +
backend-specific remainder, split on the **first colon only** (`split_once(':')`). The remainder is
opaque to the engine and parsed by the owning plugin. This matters because Azure voice names
*contain* a colon (`en-GB-Ada:DragonHDLatestNeural`), so a naive `split(':')` into three parts is
correct for sherpa and wrong for Azure. The engine never inspects the remainder; an unknown
`backend_id`, or a remainder the plugin rejects, fails validation at cast-assignment time rather than
at render time.

Voices are **auto-assigned on a character's first appearance** and persisted. That is what makes a
cast feel like continuity rather than a lottery — and it is story state, so it lives in SQLite next to
the ledger, not in a config file.

### 7.4 The `SYSTEM` voice

No sherpa model ships a robotic voice, so `SYSTEM` is a neutral speaker plus a post-render ffmpeg
pass (chain verified by Reverie, output confirmed as real modulated audio):

```bash
ffmpeg -y -i in.wav -af "asetrate=24000*0.92,aresample=24000,atempo=1/0.92,\
tremolo=f=60:d=0.7,highpass=f=180,lowpass=f=5200,acompressor" out/system.wav
```

Cost is negligible. It is a distinct pipeline stage, not a voice.

### 7.5 Resampling

sherpa models emit 22.05 kHz (Piper) / 24 kHz (Kokoro); Azure emits 16 kHz natively. The sherpa
plugin resamples internally to satisfy §7.1. **This must be in the design or the watch gets
chipmunks.** Reverie is verifying the exact ffmpeg invocation, the per-join click behaviour, and that
output size equals `32000 × seconds` exactly.

### 7.6 Secrets

The Azure plugin reads `~/.config/speech-to-cli/config.json` (or `AZURE_SPEECH_KEY`). No key is
committed, no parallel custody is invented, and a voice/region change there propagates here — the same
rationale as Morpheus's bridge. Provisioning on familiar comes from Vaultwarden (`bw get password`),
never pasted.

**The watch never sees a secret** for story playback, because chapters are pre-rendered server-side.

---

## 8. Chapter artifacts

| File | Purpose | Size (~13 min) |
|---|---|---|
| `NNNN.md` | canonical text, permanent | ~15 KB |
| `NNNN.json` | manifest: segments, voices, offsets | ~20 KB |
| `NNNN.mp3` | archive + podcast + Candela, permanent | ~6 MB |
| `NNNN.pcm` | watch playback, **buffered chapters only** | ~25 MB |

`.pcm` is the *source* (both plugins produce it); `.mp3` is derived via ffmpeg. Outside the buffer
window `.pcm` is pruned and regenerated on demand from `.mp3` if an older chapter is replayed.

### 8.1 Manifest

```json
{
  "chapter": 42,
  "sample_rate": 16000,
  "bytes_per_ms": 32,
  "duration_ms": 798400,
  "segments": [
    { "idx": 0, "speaker": "narrator",
      "voice_ref": "sherpa:piper-en_GB-cori:0",
      "text": "The vale smelled of iron and wet ash.",
      "start_ms": 0, "end_ms": 4120,
      "start_byte": 0, "end_byte": 131840 }
  ]
}
```

Byte offsets are derivable (`ms × 32`) but **precomputed on purpose**: the watch then does zero
arithmetic and can issue a `Range` request straight from the manifest. Constant bitrate is why this
works at all — compressed audio would need a frame table to answer "where does segment 40 start".

One manifest type in `litrpg-core`, **three consumers**: daemon, Candela highlighting, watch
highlighting.

---

## 9. Interfaces

### 9.1 HTTP (axum, plain HTTP, `10.0.6.107:8093`)

| Route | Consumer |
|---|---|
| `GET /api/version` | realm-sigil — mandated by CLAUDE.md for any HTTP presence |
| `GET /api/story` | story metadata, chapter count |
| `GET /api/chapters?since=N` | index: number, title, words, duration, `has_audio` |
| `GET /api/chapters/{n}` | text + segments + manifest |
| `GET /media/{n}.pcm` | **Range-capable** — watch playback |
| `GET /media/{n}.mp3` | Candela + podcast |
| `GET /feed.xml` | RSS with MP3 enclosures |
| `POST /api/notes` | director notes (CLI, watch PTT, Candela) |
| `GET /api/voices` | aggregated across plugins — drives cast selection UI |
| `GET /api/state` | derived ledger snapshot, for a status pane |
| `GET /healthz` | liveness |

Plain HTTP with **no TLS and no DNS** is a hard requirement inherited from the watch, which is why
`familiar` needs a **pinned static DHCP lease** so `10.0.6.107` cannot drift.

Port 8093 verified free on familiar (8085 mempalace, 8090 bridge, 8091 Ember, 8384 syncthing, 11435
ollama-embed, 19999 netdata all in use).

### 9.2 CLI

| Command | Behaviour |
|---|---|
| `litrpg prompt` | opens `story/prompt.md` in `$EDITOR` (nano); validates; effective next chapter |
| `litrpg note "introduce a rival"` | queues a director note |
| `litrpg status` | buffer depth, last chapter, applied/rejected delta counts |
| `litrpg cast` | list / override speaker → voice |
| `litrpg render N` | re-render audio only (e.g. after a cast change) |
| `litrpg rewind N` | deactivate ledger past N, drop chapters > N — **destructive, confirms** |

### 9.3 Prompt editing

`story/prompt.md` on disk is the source of truth, **git-tracked**. Each chapter records the prompt's
hash, so provenance is one column: six months in, when chapter 200 reads nothing like chapter 40, you
can tell drift from your own edit. Reload happens at **chapter boundaries only**, never mid-chapter.

### 9.4 Phase 2 — the watch Story app

One registration in `src/apps/mod.rs`, per the existing plugin pattern. Talks to `10.0.6.107:8093`
directly (D8 puts it on the same `/24`, so no gatekeeper rule).

- **Chapter list** from `/api/chapters` — small JSON, no large allocations.
- **Playback** via `Range` requests on `/media/{n}.pcm`, reusing the `voice_tts.rs` pattern: 512 B
  window, backpressure from the 8-slot playback channel, ≈2.4 KB peak. Nothing is buffered whole —
  16 MB of flash split into two 6 MB OTA slots leaves no room to cache, so streaming is mandatory.
- **Resume** — byte offset from the manifest; exact by construction.
- **Sentence highlighting on glass**, driven by the same manifest offsets Candela uses.
- **Director notes by voice** — push-to-talk → existing STT gateway → `POST /api/notes`.
- **Must handle the amp-gate deadlock** documented in the read-aloud spec's §6.2, or this ships silent
  while logging success.

### 9.5 Battery instrumentation (D9)

Playback keeps the screen on with live highlighting. Since battery life under sustained playback is
untested, the Story app **records drain across a full chapter** using the existing per-subsystem
current estimation and battery monitoring, and reports it. This converts "not fully tested" into a
number. Screen-off playback becomes an option only if the measurement says it must.

### 9.6 Phase 3 — the Candela source module

`source-endless`, implementing `FictionSource` against the daemon:

| `FictionSource` | Mapping |
|---|---|
| `popular` / `latestUpdates` | the running serial(s) |
| `search` | chapter title/text search |
| `fictionDetail` | story metadata + chapter list |
| `chapterContent` | chapter markdown |

Phases 2 and 3 are **order-swappable** — both consume the same API, and neither blocks the other.

---

## 10. Failure modes

Each has a defined degradation. The rule is: **a bookkeeping failure must never cost a chapter.**

| Failure | Behaviour |
|---|---|
| Ember unreachable | exponential backoff; buffer drains; nothing corrupts |
| Pass 1 malformed/empty | 2 retries with temperature jitter, then skip the cycle. **No partial chapters** |
| Pass 2 schema failure | chapter **ships anyway**, `state_dirty=1`, re-extract later |
| Delta rejected | stored `applied=0` with reason; generation continues |
| TTS plugin fails | text ships, `has_audio=false`, render queue retries. Text and audio are separate artifacts precisely so one cannot take down the other |
| Azure metered/unavailable | that voice falls back to its sherpa equivalent; chapter still renders |
| mempalace down | mining is best-effort and async; never in the critical path |
| Disk pressure | prune `.pcm` outside the buffer window; `.mp3` is permanent |
| Crash mid-pipeline | every stage idempotent by chapter number; resume from last completed stage |

---

## 11. Testing

| Unit | Approach |
|---|---|
| `litrpg-core` | property tests: serde round-trip; `start_byte == start_ms × 32` |
| Tagged-prose parser | table-driven fixtures: malformed tags, nested quotes, unknown speakers, untagged leading prose |
| Ledger fold | property tests: `fold(rewind(N)) == snapshot_at(N)`; fold is order-independent given `seq` |
| Validation gate | one unit test per rule (negative HP, level decrease, unknown subject, bad field) |
| `litrpg-ember` | recorded fixtures; one `#[ignore]` live test against `familiar:8091` |
| `litrpg-tts` | mocked backend; one opt-in render test asserting `bytes == 32000 × seconds` |
| Mixed-model stitch | opt-in: cori + Kokoro + SYSTEM in one chapter, assert continuity and exact byte count |
| `litrpg-daemon` | axum harness, including **`Range` correctness** — the watch's whole playback path rests on it |
| Watch app | `core-source-testkit`-equivalent is Android-side; firmware gets the project's existing `ui_test.py` console transport |

CLAUDE.md's rule applies: this project has tests, so they run after every change.

---

## 12. Open questions

1. **Project naming.** CLAUDE.md says new projects with HTTP presence adopt `name.realm.watch` and
   register in `status.realm.watch/checks.json`. Should this become e.g. `story.realm.watch`, or stay
   `endlesslitrpg` like `candela`, `smol`, and `tonemask`? Registration in `checks.json` should happen
   either way once `/api/version` exists.
2. **Assumption A1** — `cori` sourcing (§4.4), in flight with Reverie.
3. **Static DHCP lease** for `familiar` at `10.0.6.107` — needs confirming on gatekeeper; the watch has
   no DNS.
4. **`cargo` on familiar** — install via rustup or mise when implementation starts.
5. **Story premise.** The initial `prompt.md` content is JP's to write; the engine ships with a
   placeholder-free default seed that JP is expected to replace.

## 13. Drive-by finding (not in scope)

`candela`'s `KokoroVoiceHelper.java` still passes `dict_dir`, which sherpa-onnx **deprecated in
≥1.12.15** (warns to stderr and ignores it). Harmless today, worth a ticket in that repo.

---

## 14. Build order

**Phase 1 — the daemon.** `litrpg-core` → `litrpg-store` (+ ledger fold property tests) →
`litrpg-ember` (two-pass + validation gate) → `litrpg-tts` (registry, sherpa plugin, azure plugin) →
`litrpg-engine` → `litrpg-daemon` (+ RSS) → `litrpg-cli`. **Listenable in any podcast app at the end
of this phase**, with no client work.

**Phase 2 — the watch Story app.** Read the read-aloud spec first; handle the amp-gate deadlock;
instrument battery.

**Phase 3 — `source-endless` for Candela.** Swappable with Phase 2.

Each phase gets its own implementation plan.
