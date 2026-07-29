# endlesslitrpg

An endless LitRPG serial, written by a local model and narrated by a cast of voices.
Every chapter is saved as text and audio, permanently.

Ember — the Qwen3.6-35B-A3B lane on `familiar:8091` — writes the prose. A validation
gate decides what becomes canon. A TTS plugin layer renders it with one voice per
character plus a distinct `SYSTEM` voice for stat blocks. The result is a podcast
feed, a Range-capable stream for an ESP32-C6 watch, and files on disk.

## Quick start

```bash
litrpg init --title "Your Title" --protagonist "Their Name"
litrpg prompt          # write the premise — this file is the story
litrpg-engine --once   # generate one chapter
litrpg read            # read the latest
litrpg play 1          # hear it
```

Config lives in `litrpg.toml`. Paths in it are resolved relative to the config file,
so this folder is self-contained: copy it elsewhere and it still works.

## The pieces

| Crate | Does |
|---|---|
| `litrpg-core` | `no_std` wire types shared with the watch firmware: ledger, manifest, voice refs, the validation gate |
| `litrpg-store` | SQLite. The only crate that writes state |
| `litrpg-ember` | The two-pass generation contract against Ember |
| `litrpg-tts` | Plugin registry: sherpa-onnx (local) and Azure DragonHD, both first-class |
| `litrpg-engine` | The chapter loop, and the binary that runs it |
| `litrpg-daemon` | HTTP: chapter API, Range-capable PCM, RSS feed, `/api/version` |
| `litrpg-config` | One definition of where things live |
| `litrpg-cli` | `litrpg` — init, prompt, note, status, state, cast, read, play, listened, rewind |

## Three ideas hold it together

**State is a fold, not a table.** There is no `characters.hp` column. Current state is
computed from an append-only ledger, which is what makes `litrpg rewind 40` free —
deactivate the rows past chapter 40 and the snapshot is simply correct again.

**The engine, not the model, owns state.** Ember only *proposes* stat changes. A
validation gate accepts or rejects each one — HP within bounds, levels and XP that
never regress, equipment slots whitelisted. Rejections are stored, not discarded: a
rising rejection rate is the early warning that the prompt is drifting.

**Every plugin returns 16 kHz mono s16le raw PCM.** Exactly 32 bytes per millisecond,
so a timestamp converts to a byte offset in closed form and no seek index is needed.
That is why the watch — 512 KB of RAM, no PSRAM — can stream a 25 MB chapter with no
decoder, no resampler, and no frame table.

## Running it continuously

See `deploy/` for systemd units. The engine idles when `buffer_target` chapters are
rendered ahead of the playback cursor, so tell it how far you have listened:

```bash
litrpg listened 3
```

## Development

```bash
cargo test --workspace                  # ~750 tests, no network, no GPU, no ffmpeg
cargo clippy --workspace --all-targets -- -D warnings
```

Tests that cost money or GPU time are `#[ignore]`d:

```bash
cargo test -p litrpg-engine --test live_end_to_end -- --ignored --nocapture
cargo test -p litrpg-tts --features sherpa --test sherpa_live -- --ignored
```

`cargo` is at `~/.cargo/bin` and may not be on a non-interactive `PATH`.

### Local TTS

The `sherpa` feature links native libraries and is **off by default**, so a plain
`cargo test` never builds them. With it enabled the binary needs the shared objects
on its library path:

```bash
cargo build -p litrpg-engine --bin litrpg-engine --features sherpa
export LD_LIBRARY_PATH="$PWD/target/debug:$LD_LIBRARY_PATH"
```

Models go under `$LITRPG_SHERPA_MODELS`, else `~/.local/share/litrpg/models`:
`vits-piper-en_GB-cori-high` (the narrator) and `kokoro-multi-lang-v1_0` (the cast),
~511 MB together. `SherpaConfig::preflight()` refuses to load an incomplete model,
because sherpa-onnx calls `exit()` on a missing asset rather than returning an error.

## Documentation

- `docs/superpowers/specs/2026-07-29-endless-litrpg-design.md` — the design, including
  what was measured rather than assumed
- `docs/superpowers/plans/` — implementation plans
- `docs/samples/` — a chapter generated early on, kept as a sample
