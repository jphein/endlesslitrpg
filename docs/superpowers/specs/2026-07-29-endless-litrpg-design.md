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

**A1 — `cori` — RESOLVED 2026-07-29.** Available directly from the sherpa-onnx `tts-models` bucket;
no HuggingFace fallback and no substitution needed. `vits-piper-en_GB-cori-medium` (67 MB) and
`-high` (116 MB) both return HTTP 200; `-low` does not exist. Preferred over the raw upstream Piper
voice because the sherpa tarball bundles `tokens.txt` and `espeak-ng-data`, so it drops into the
existing VITS config with no assembly.

| | |
|---|---|
| Language / voice | **en_GB — UK English female** |
| Speakers | **1** (`sid 0` only) |
| Native rate | **22,050 Hz** (both variants) |
| Dataset | LibriVox, ~24 h, public domain — audiobook provenance |
| RTF @ 8 threads | medium **25.0×** · **high 7.55×** |

**Narrator default is `cori-high`.** At 7.55× it is *faster than Kokoro* (5.28×), so the largest
share of any chapter can use the better variant without becoming the bottleneck: ~103 s for a
13-minute narration, against a 3-minute budget.

Voices remain **config, not code**, so recasting the narrator needs no code change.

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

### 5.2 What the live model actually does (measured 2026-07-29, build `b950-555881e`)

Three results from pinning the two-pass contract against `familiar:8091` rather than assuming:

1. **`response_format: {"type":"json_schema", …}` is supported and genuinely enforced.** GBNF
   `grammar` is not required. A schema the server cannot compile fails loudly with **HTTP 400** on the
   first call rather than degrading quietly.
2. **Qwen3.6 reasons by default, and the reasoning is *not* grammar-constrained.** So a
   schema-constrained call with thinking enabled spends its budget reasoning and returns
   `content: ""` with `finish_reason: "length"` — structured output that presents as the model having
   said nothing at all. Thinking is therefore **disabled by default** on the extraction pass. No
   amount of reading documentation would have surfaced this interaction.
3. **Pass 1's real output is *block* shaped**, not one tag per line: the speaker tag alone on its own
   line, content on the lines beneath, a blank line closing the block. The parser accepts **both**
   shapes, since the line-per-tag form is what the prompt asks for and the block form is what the
   model tends to produce.

Consequence for §10: an empty `content` is *not* interchangeable with a network failure. The error
type distinguishes them, because "the model reasoned itself out of budget" wants a retry with
thinking off, while a network failure wants backoff.

### 5.3 The prompt instructs strictly; the parser accepts leniently. Do not "fix" this.

`prompt.rs` tells the model to emit `[narrator]`. `parse.rs` accepts `narrator` *and* `narration`,
case-insensitively, tolerant of whitespace and of a typo within a bounded edit distance. Those two
look like a duplicated contract that has drifted. **They are not, and making them agree would
re-introduce two bugs we have already shipped.**

The evidence is on the record: a live chapter emitted `[narration]` for most of its length, and
another emitted `narraraor` — the model mistyping its own tag. Both were classified as *characters*,
which minted permanent `cast` rows and gave a chapter's narration a character's voice. A parser that
matched the instruction exactly would reject or mis-attribute both.

So the asymmetry is the design: **strict on what we ask for, lenient on what we accept.** That is
the correct shape for a contract with a model that will not comply exactly, and it is why rejecting a
typo'd narrator tag would be the wrong repair — it loses the prose rather than saving it. Anything an
LLM emits that becomes durable identity needs *canonicalisation*, not validation.

### 5.4 Ranking duplicated values by whether a mismatch is silent

Five instances of one shape have now been fixed — config paths, the prompt hash, the prompt's
legal-field list, the chapter filename convention, and `SpeakerKind`'s string form. The useful
question when auditing for a sixth is **not how many copies exist**:

> Do the copies sit on opposite sides of a trust boundary that nothing checks?

The worst instance found crossed a crate boundary *and* a language boundary — a Rust value written as
a SQL literal — which is precisely why no compiler and no test caught it. The healthiest apparent
instance (§5.3) has two copies inside one crate, differs *on purpose*, and is tested.

Counting copies would have ranked the healthy one highest and the dangerous one nowhere. Note also
what the technique cannot find: grepping for a duplicated **literal** catches `{:04}` and
`"narrator"`, and would never have found the prompt-hash instance, because "config path resolution"
is not a string.

The sixth instance was `voice_ref`. It is derived state owned by the `cast` table, with a copy on
every `segments` row and nothing forcing them to match — and the resumed-render path read the copy.
It crossed no crate boundary and no language boundary; both copies were plain Rust in one function.
What hid it was that **the copies agreed for every chapter rendered correctly**, so the disagreement
only existed in the chapters that needed repair. Add to the heuristic: a duplicated value whose
copies diverge *only in the failure case* is invisible to every test that starts from a healthy
fixture.

### 5.5 What a passing test cannot see

Four separate wording and advice defects reached a green suite: a stale-heartbeat line reading
"longer than the 15 min **ago** it is allowed" (an age formatter reused for a duration), and three
others of the same kind. Every assertion passed. The sentences were still wrong.

> Assertions catch **missing** output. Only reading the output catches **wrong** output.

This is the same family as §5.3 — both are about what a test can and cannot observe. An assertion
encodes a property someone thought to check; prose that is grammatical, well-formed, present in the
right field, and *misleading* satisfies every such property. So a command whose output is advice
gets read by a human at least once, out loud if necessary, however green the suite is.

The corollary bit us the same day, one level up. Re-rendering chapters 1, 2 and 5 restored the
correct narrator and "proved" the recovery path worked; chapters 3 and 4, the only two that were
broken, reproduced the substitution. **A test of a recovery path must start from the broken state.**
Exercising it from a healthy fixture passes trivially and proves nothing — and it will keep passing
while the recovery recovers nothing.

One more, learned by writing the same sentence wrong twice in an afternoon: **assert the claim
positively, not only negatively.** A negative assertion pins the mistake that has already been made
and nothing else. `!out.contains("restoring the original")` was added the first time that phrase was
wrong, and it sat there untroubled while the text was inverted past correct in the other direction.
The positive form — `contains("re-derives voices from the cast")`, and better, the arrow
`azure:… -> sherpa:…`, which pins the *direction* — fails on a rewrite that drops or reverses the
claim. Negatives are a supplement; they cannot be the guard.

### 5.6 A claim about someone else's behaviour has no owner

The defects in §5.5 were not false when written. Every one was accurate, and then a *different
component* changed and nothing failed.

> A doc comment describing another module's behaviour is a copy of that behaviour with nothing
> forcing it to track. Only a test that exercises both sides can fail when they diverge.

This is §5.4 one step out: not two copies of a **value** but a copy of a **behaviour**. It is worse
in one respect, because a duplicated value can at least be grepped, while a paraphrase of what
another crate does is invisible to every tool. Three specimens, all found on the same day:

- `render`'s help described the resume path's voice handling. The resume path changed; the help
  became a confident lie in both directions in turn.
- Three comments cited `cycle.rs:896` — a line number is another copy of a location, and they were
  stale the moment the function they described was edited. Replaced with the function's *name*,
  which survives an edit.
- A divergence check compared speaker names by exact equality while the engine compared them with
  `eq_ignore_ascii_case`. `SYSTEM` against a cast row spelled `system` would have reported "nothing
  will change" and then changed anyway. Two places comparing the same names by different rules,
  found by reading the other side — not by any test on either side.

The mitigation that works is to **assert across the boundary rather than describe it**: one fixture
exercising both rules, so it fails if they diverge again.

### 5.7 Resolving a contradiction between agents or sessions

Twice in one afternoon a measurement contradicted a claim, and on both occasions the answer was that
the two parties were reading different snapshots — once of the engine's source, once of the CLI's
own output. Neither party was wrong.

> A contradiction that **neither side can explain** is a version mismatch, not a disagreement.
>
> So: quote the artifact with its identity attached — a commit hash, or literal current output —
> rather than re-asserting the conclusion.

Re-asserting escalates and settles nothing, because both sides have honest evidence. Quoting the
artifact resolves it in one exchange. The rule has to be applied symmetrically to work: the second
occurrence ran in the opposite direction from the first, and the tell was identical.

---

## 6. Data model

```sql
story    (id, title, protagonist, prompt_path, prompt_hash, arc_outline_md,
          target_words, updated_at)
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
| numeric fields | `hp`, `max_hp`, `level`, `xp`, `gold` |
| text fields | `location`, `status` |
| `inv:<item>` | inventory counts, numeric, non-negative. Free-form item names |
| `equip:<slot>` | equipped item name, text, **Set-only**. Slot is whitelisted (see below); empty string means the slot is empty |
| `appear:<trait>` | appearance descriptor, text, **Set-only**. Whitelisted: `hair`, `eyes`, `skin`, `build`, `height`, `notable` |
| equipment slots | `head`, `chest`, `legs`, `feet`, `hands`, `cloak`, `main_hand`, `off_hand`, `amulet`, `ring1`, `ring2` |
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
- **Equipment slot whitelisted** — `equip:third_arm` is rejected. Each slot maps to a row on the
  character screen (§9.4.1), so an invented slot would silently break the renderer.
- **Appearance trait whitelisted** — same reasoning: the screen layout stays stable.
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
- **sherpa** overrides it to **fan out across a process pool at 4 threads per worker**, sharded by
  **work unit (segment)** — *not* by model. Measured: one process holds cori and Kokoro
  simultaneously with **zero reload penalty**, so routing segments by model would add complexity for
  nothing.

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

sherpa models emit 22.05 kHz (cori and Piper) / 24 kHz (Kokoro); Azure emits 16 kHz natively. The
sherpa plugin resamples internally to satisfy §7.1. **This must be in the design or the watch gets
chipmunks.** Verified invocation — headerless, since `-f s16le` writes a bare sample stream with no
44-byte WAV header:

```bash
ffmpeg -y -i INPUT.wav -af "loudnorm=I=-20:TP=-2:LRA=7" \
  -ar 16000 -ac 1 -f s16le -acodec pcm_s16le OUTPUT.pcm
```

Byte-exact against the 32,000 B/s contract, and ~850× realtime — resampling is never a bottleneck.
The SYSTEM colouring chain (§7.4) composes into the *same* single ffmpeg pass; no intermediate file.

**`loudnorm` is not optional.** The backends land 4.1 LU apart — cori −25.1 LUFS, Kokoro −24.2,
Kokoro+SYSTEM FX −21.0 (the `acompressor` drives that one hot) — which is plainly audible as a level
jump where segments abut. One stage brings the spread to **0.7 LU**, below the ~1 LU just-noticeable
difference, with the whole chapter at −20.0 LUFS: inside the ACX audiobook window. This was the real
integration hazard, not the resample.

**Consequence that must not be missed: `loudnorm` has filter delay and changes stream length
slightly** (a measured segment came out 104 bytes shorter). Therefore:

1. Byte offsets and durations are **measured from the final, fully-processed PCM** — never predicted
   beforehand.
2. Every segment's final PCM is **zero-padded up to a 32-byte (whole-millisecond) boundary**. At most
   30 bytes, under 1 ms, inaudible — and it keeps `duration_ms × 32 == len` exactly true, which is
   the invariant the watch's Range requests and sentence highlighting rest on. Without this, an
   arbitrary even byte count silently breaks the manifest's arithmetic.

**Joins need no silence padding.** Measured join deltas were 6 and 1 against a p99.9 signal delta of
~3,595 — three orders of magnitude below normal movement, because sherpa output already begins and
ends at ~zero amplitude. Inter-segment silence remains available as a **narrative pacing** control
(a beat before a SYSTEM alert), defaulting to zero. It is a creative dial, not artifact suppression.

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
      "kind": "narrator",
      "start_ms": 0, "end_ms": 4120 }
  ]
}
```

**Correction to an earlier draft of this section.** It claimed byte offsets were "precomputed on
purpose so the watch does zero arithmetic". They are **not serialized** — `litrpg-core` exposes
`start_byte()`/`end_byte()` as *methods* computing `ms × 32`, and the JSON carries only
`start_ms`/`end_ms`. The original rationale was also overstated: `ms × 32` is a bit-shift, and
avoiding one shift was never the point.

**The actual point is that no frame table is needed.** At a constant 32 bytes/ms, any timestamp
converts to a byte offset in closed form. Compressed audio would require a seek index mapping time to
frame boundaries — an extra artifact to generate, ship, keep in sync, and parse on a 512 KB device.
That is what constant bitrate buys, and it holds whether or not the offsets are written down.

What makes the conversion trustworthy:

1. Durations are measured from the **final rendered PCM** — after resampling, SYSTEM colouring and
   `loudnorm` — never from predicted durations, because `loudnorm` alters stream length (§7.5).
2. Each segment is zero-padded to a **32-byte boundary** before offsets are computed. Note
   `duration_ms × 32 == len` cannot hold for an arbitrary buffer: 32 bytes is 1 ms, so a legal
   34-byte buffer is 1.0625 ms, and real renders land there routinely (a measured cori render is
   1,955,108 bytes = 61,097.125 ms). `duration_ms()` therefore **floors**, and padding is the
   mechanism that makes the identity exact where it matters. Unpadded, segment *N*'s offset drifts by
   the accumulated remainders of segments `0..N` — a silent, cumulative desync of highlighting and
   Range requests that grows across a chapter.
3. The render path **asserts** `manifest.duration_ms × 32 == pcm.len()` and
   `manifest.is_contiguous()` before publishing. `Manifest::new()` derives `duration_ms` from the last
   segment while `chapters.duration_ms` is stored separately; they agree by construction today, but
   an invariant this load-bearing is checked, not assumed.

Clients are given `bytes_per_ms` on `/api/story` and `total_bytes` on the chapter index so none of
them hardcodes 32. Constant bitrate is why this
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
| `GET /api/character/{subject}` | one subject's stats, equipment and appearance — watch screens (§9.4.1) |
| `GET /healthz` | liveness |

Plain HTTP with **no TLS and no DNS** is a hard requirement inherited from the watch, which is why
`familiar` needs a **pinned static DHCP lease** so `10.0.6.107` cannot drift.

**Authentication: none, accepted deliberately.** Every route is open on a LAN-reachable port. This
follows from the watch's constraints — it cannot do TLS, so there is no credential worth protecting in
transit — and from the fact that this is a single-tenant story generator on a trusted `/24`. The
consequence to be explicit about rather than leave implicit: `POST /api/notes` is the only mutating
route, so **anything on `10.0.6.0/24` can inject a director note** and thereby steer the story. The
body is bounded at 4096 bytes to limit the damage. Accepted; revisit if the daemon is ever exposed
beyond the admin VLAN.

**`/api/version` is hand-rolled, not mounted from realm-sigil.** That crate's `rust/` directory turns
out to be a `no_std` *name generator*; the one-line handler exists only in the Go/Python/JS bindings.
The endpoint matches the Go `Version` struct field-for-field so `status.realm.watch` and the
`<Sigil />` badge keep working. Taking the crate as a dependency was rejected because its own docs
flag that the Rust word tables were regenerated against a later lexicon and **disagree with the other
bindings** — hash `9e3779b1` is `Blazing Jewel` in Go and `Draconic Monolith` in Rust — so a project
consuming both would publish two different names for one commit. That divergence is a bug in
realm-sigil worth fixing there, not worked around here.

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

### 9.4.1 Watch character and stats screens

Two additional pages in the Story app, both fed by `GET /api/character/{subject}` with `subject`
defaulting to `story.protagonist`.

**Stats screen** — nearly free, because the ledger fold already produces exactly this data. Level, XP
(with progress to next), HP as a bar against `max_hp`, gold, `location`, `status`, and inventory
counts. It is a *rendering* of the derived snapshot; it holds no state of its own and cannot disagree
with the story.

**Character screen** — the protagonist's appearance and what they are wearing and wielding: the
whitelisted `appear:*` traits plus all eleven `equip:*` slots with their current item names.

**Render strategy — text-and-icon panel first.** The default is a slot list plus appearance lines
drawn with Slint / `embedded-graphics` primitives: zero image assets, zero new pipeline, and correct
the moment the ledger has data. This is what Phase 2 builds.

**A composited portrait is a deliberate Phase 2b, not a Phase 2 goal.** If it happens, it follows the
same principle as the audio path — *own both ends and ship the exact bytes the hardware wants*: the
daemon composites layered art into **RGB565** server-side and the watch blits it with no decode, no
resize, and no image library. The existing Slint renderer already streams two-line RGB565 strips
(~1.6 KB) straight to panel GRAM, so a portrait stream reuses a proven path rather than inventing one.
An endpoint shape is reserved (`GET /media/portrait/{subject}.rgb565`) so the door stays open.

Reasons it stays deferred rather than promised: 512 KB SRAM with no PSRAM means a full 410×502 RGB565
frame is ~412 KB and cannot be held whole, so it must stream in strips; the flash budget is already
committed to two 6 MB OTA slots; and layered character art does not exist yet. None of that is
blocking — it is simply a separate piece of work with its own art dependency.

**Why the whitelists in §6.2 matter here.** Each `equip:` slot is a row on this screen and each
`appear:` trait is a line. If Ember could invent `equip:third_arm`, the screen would either drop data
silently or need defensive layout code forever. Rejecting at the gate keeps the renderer simple and
makes the failure visible in the `applied=0` audit trail instead of on glass.

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
2. ~~**Assumption A1** — `cori` sourcing~~ — **resolved 2026-07-29** (§4.4). One thing worth a
   conscious decision rather than a default: **`cori` is UK English *female***. If a male narrator
   was intended, Kokoro `bm_george` (26) or `bm_lewis` (27) keep the en-GB signature. Config change,
   not a code change.
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
