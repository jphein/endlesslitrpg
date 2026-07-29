//! One turn of the chapter loop (spec §5.1).
//!
//! ```text
//! 0  resume        a chapter with prose but no audio? render it, stop
//! 1  buffer check  rendered-ahead >= target? idle
//! 2  drain notes   read pending; consumed only at step 10
//! 3  pass 1        transport -> backoff; malformed -> 2 jittered retries; 400 -> stop
//! 4  parse + cast  new speakers get a deterministic voice, persisted
//! 5  pass 2        failure -> chapter still ships, state_dirty = 1
//! 6  new_lore      BEFORE the deltas
//! 7  deltas        rejections stored, never fatal
//! 8  render        measured lengths -> manifest
//! 9  publish       md, pcm, mp3, json, attach_audio
//! 10 notes         mark consumed, once the chapter is durable
//! ```
//!
//! # The two orderings that are not stylistic
//!
//! **Step 6 before step 7.** `known_subjects` is `cast ∪ applied ledger subjects ∪ lore
//! rows of kind `character``. A character introduced *this* chapter is therefore only a
//! known subject once their `new_lore` row exists. Append the deltas first and every new
//! character's opening stats are rejected as `UnknownSubject` — a bug that costs nothing
//! visible, just a protagonist whose HP never initialises.
//!
//! **Step 4 before step 5 and 7.** Persisting the cast makes the chapter's speakers known
//! subjects too, so dialogue-only characters can carry stats without a lore row.
//!
//! # Failures that must not cost a chapter (§10)
//!
//! Pass 2 failing, a delta being rejected, TTS failing, an artifact write failing — all of
//! these leave the prose published. Only a pass-1 failure abandons the cycle, and it does
//! so **before anything is written**, so there is no partial chapter to clean up.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use litrpg_core::speaker::{self, same_speaker};
use litrpg_core::{SpeakerKind, content_hash};
use litrpg_ember::prompt::{ChapterSummary, LoreEntry, Pass1Input, render_state_snapshot};
use litrpg_ember::{Extraction, ParsedSegment, match_lore, parse_tagged_prose};
use litrpg_store::{NewChapter, Store};
use tracing::{debug, info, warn};

use crate::canon::{SubjectResolution, resolve_subject};
use crate::cast::{CastAssignment, ParsedSpeaker, VoiceAssigner};
use crate::error::{CycleOutcome, EngineError};
use crate::ports::{Artifacts, Generator, Library, Renderer};
use crate::render::{PlannedSegment, assemble, chapter_markdown, plan_requests, sentence_pieces};

/// Spec §6.3 — the last five chapter summaries.
pub const SUMMARY_WINDOW: usize = 5;

/// Pass 1: one attempt plus two jittered retries (§10).
pub const PASS1_TEMPERATURES: &[f64] = &[0.9, 0.95, 1.0];

/// Pass 2 wants determinism, so it starts at 0. The jitter exists only to shake the model
/// off a malformed generation it would otherwise reproduce exactly.
pub const PASS2_TEMPERATURES: &[f64] = &[0.0, 0.15, 0.3];

/// Longest derived chapter title before truncation.
const TITLE_MAX: usize = 60;

/// How many times one chapter's render is retried before the loop stops picking it up.
///
/// The count is **in-memory**, so a daemon restart clears it. That is deliberate: a
/// restart is usually what happens *after* someone fixes the missing model file or the
/// expired key, and it is the natural moment to try again.
pub const MAX_RESUME_ATTEMPTS: u32 = 3;

/// Where the rendered-ahead buffer measures from (§5.1 step 1).
///
/// A type rather than a bare `u32` so that "read it fresh every cycle" is structural. The whole
/// point of a settable cursor is that a long-running daemon notices when someone listens, and a
/// value passed in at startup would make it inert in exactly the process that matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferCursor {
    /// Read `story.consumed_through` from the store, on every cycle. The normal mode.
    #[default]
    Stored,
    /// An explicit override, for debugging.
    At(u32),
    /// Ignore the cursor: treat the buffer as always empty so generation never idles.
    ///
    /// Not a stand-in for anything now that a real cursor exists — it deliberately overrides
    /// one, for backfilling a story or filling a buffer ahead of a listener.
    Drain,
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub buffer_target: u32,
    pub target_words: u32,
    pub narrator_voice: String,
    /// Voice for `[SYSTEM]` blocks. The robotic character comes from the post-render
    /// ffmpeg pass (§7.4), not from the voice itself.
    pub system_voice: String,
    /// Pool characters draw from, in preference order. Must belong to a registered
    /// backend: a `voice_ref` names its backend, so a sherpa pool on an Azure-only
    /// deployment fails at render time and the chapter ships silent.
    pub character_voices: Vec<String>,
    /// `voice_ref` → advertised gender, from the TTS registry. Populated by the caller, since
    /// the engine holds a [`Renderer`] rather than a registry. Empty disables gendered casting
    /// entirely — the feature is additive.
    pub voice_genders: BTreeMap<String, litrpg_tts::Gender>,
    /// Backend ids the TTS registry **actually has loaded**, e.g. `["azure"]`.
    ///
    /// Populated from `TtsRegistry::availability()`, never from the config file, and named to make
    /// that unmissable: config says what was *asked for*, the registry says what this process can
    /// *serve*, and the gap between the two is the bug that let a `sherpa:` cast render in an Azure
    /// voice for four chapters with nothing to notice it. A config-sourced value here would make
    /// the heartbeat report `sherpa` on a binary that does not have it — an instrument that lies in
    /// exactly the case it exists for.
    ///
    /// Empty means "unknown", and then no substitution happens.
    pub registered_backends: Vec<String>,
    /// Emit one manifest entry per **sentence** rather than per speaker turn (§9.4).
    ///
    /// Turn-level entries cannot drive sentence highlighting: measured on live chapter 1, the
    /// mean entry was 64.7 s and the longest 203 s, with 3 665 chars of prose — a highlight that
    /// sits still for three minutes reads as broken rather than as absent.
    ///
    /// The flag exists to make this revertible: turn-level highlighting is still serviceable, so
    /// if per-sentence synthesis ever costs too much wall clock or hurts the joins, this reverts
    /// without touching the pipeline.
    pub sentence_manifest: bool,
    pub summary_window: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            buffer_target: litrpg_config::MIN_BUFFER_TARGET + 1,
            target_words: litrpg_ember::DEFAULT_TARGET_WORDS,
            narrator_voice: crate::cast::NARRATOR_FALLBACK_VOICE.to_string(),
            system_voice: crate::cast::SYSTEM_VOICE.to_string(),
            character_voices: crate::cast::character_pool(),
            voice_genders: BTreeMap::new(),
            registered_backends: Vec::new(),
            sentence_manifest: true,
            summary_window: SUMMARY_WINDOW,
        }
    }
}

impl EngineConfig {
    pub fn from_config(c: &litrpg_config::Config) -> Self {
        Self {
            buffer_target: c.buffer_target,
            target_words: c.target_words,
            narrator_voice: c.narrator_voice.clone(),
            system_voice: c.system_voice.clone(),
            character_voices: c.character_voices.clone(),
            voice_genders: BTreeMap::new(),
            registered_backends: Vec::new(),
            sentence_manifest: true,
            summary_window: SUMMARY_WINDOW,
        }
    }
}

/// The loop, parameterised over its four ports so the whole pipeline is testable with no
/// network, no GPU and no ffmpeg.
pub struct Engine<G, R, L, A> {
    /// `Mutex` rather than a bare `Store`: `rusqlite::Connection` is `Send` but not
    /// `Sync`, and without this the cycle future would not be `Send` and could not be
    /// spawned. Guards are always scoped inside [`Engine::with_store`] so one is never
    /// held across an await.
    store: Arc<Mutex<Store>>,
    generator: G,
    renderer: R,
    library: L,
    artifacts: A,
    config: EngineConfig,
    /// Per-chapter count of failed resume renders, so a chapter that can never render
    /// stops being picked up. See [`MAX_RESUME_ATTEMPTS`].
    resume_failures: Mutex<BTreeMap<u32, u32>>,
}

impl<G, R, L, A> Engine<G, R, L, A>
where
    G: Generator,
    R: Renderer,
    L: Library,
    A: Artifacts,
{
    pub fn new(
        store: Store,
        generator: G,
        renderer: R,
        library: L,
        artifacts: A,
        config: EngineConfig,
    ) -> Self {
        Self::with_shared_store(
            Arc::new(Mutex::new(store)),
            generator,
            renderer,
            library,
            artifacts,
            config,
        )
    }

    /// Build over a store handle shared with something else — normally
    /// [`StoreLibrary`](crate::StoreLibrary), which needs the *same* connection rather
    /// than a second one to the same file.
    pub fn with_shared_store(
        store: Arc<Mutex<Store>>,
        generator: G,
        renderer: R,
        library: L,
        artifacts: A,
        config: EngineConfig,
    ) -> Self {
        Self {
            store,
            generator,
            renderer,
            library,
            artifacts,
            config,
            resume_failures: Mutex::new(BTreeMap::new()),
        }
    }

    /// How many times chapter `number`'s render has failed since this process started.
    ///
    /// **In-process only, so the CLI can never read this.** `litrpg status` runs as a
    /// separate process against the same database and would always see zero. This belongs in
    /// the daemon's `GET /api/state`, which owns the running engine. The operator-visible,
    /// persisted half of the same question is `Store::chapters_missing_audio()` — *which*
    /// chapters lack audio is in SQLite; *how many times we have tried* is not, deliberately,
    /// because a restart should retry a chapter whose missing model or expired key was just
    /// fixed.
    pub fn resume_attempts(&self, number: u32) -> u32 {
        self.resume_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&number)
            .copied()
            .unwrap_or(0)
    }

    /// Chapters with prose and no audio that this process has given up on.
    ///
    /// Same caveat as [`Engine::resume_attempts`]: the attempt counts are in memory, so this
    /// is a **daemon** view (`GET /api/state`), not something `litrpg status` can compute.
    /// A CLI wanting "what has no audio" should call `Store::chapters_missing_audio()`.
    pub fn stuck_chapters(&self) -> Result<Vec<u32>, EngineError> {
        let unrendered = self.with_store(unrendered_chapters)?;
        Ok(unrendered
            .into_iter()
            .filter(|n| self.resume_attempts(*n) >= MAX_RESUME_ATTEMPTS)
            .collect())
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn generator(&self) -> &G {
        &self.generator
    }

    pub fn renderer(&self) -> &R {
        &self.renderer
    }

    pub fn library(&self) -> &L {
        &self.library
    }

    pub fn artifacts(&self) -> &A {
        &self.artifacts
    }

    /// A handle on the same connection, for building a [`StoreLibrary`](crate::StoreLibrary)
    /// or running CLI queries (`status`, `cast`, `rewind`) against the same database.
    pub fn store_handle(&self) -> Arc<Mutex<Store>> {
        Arc::clone(&self.store)
    }

    /// Hand the store back when the engine is done with it.
    pub fn into_shared_store(self) -> Arc<Mutex<Store>> {
        self.store
    }

    /// Run store work under a scoped lock. Poison-tolerant: a panic elsewhere must not
    /// permanently wedge the loop, and the connection itself is not left inconsistent
    /// because every multi-statement write in the store is transactional.
    pub fn with_store<T>(
        &self,
        f: impl FnOnce(&Store) -> litrpg_store::Result<T>,
    ) -> Result<T, EngineError> {
        let guard = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard).map_err(EngineError::Store)
    }

    /// Record that this process is alive, and what it can actually render.
    ///
    /// `pid` and `version` are evaluated **here**, in the engine crate: `litrpg-store` is linked
    /// into the CLI and the daemon too, so reading them there would describe whichever process
    /// happened to call and report the store's version rather than the engine's.
    ///
    /// Never fails a cycle. This is an observability write, so §10's rule — a bookkeeping failure
    /// must not cost a chapter — applies with more force here, not less: losing a chapter to a
    /// failed liveness stamp would be the instrument causing the outage it exists to report.
    ///
    /// # Called at phase boundaries, not only at the top
    ///
    /// A once-per-cycle stamp cannot distinguish "crashed during TTS" from "still rendering" for a
    /// whole cycle, which for a chapter is minutes. Stamping after each phase bounds staleness by
    /// the **longest single phase** rather than by the whole cycle.
    ///
    /// What that does and does not buy, precisely: it does **not** detect a mid-TTS crash promptly,
    /// because nothing stamps *inside* the render. It bounds the stale window to the longest phase —
    /// measured on the live serial, generation ≈ 60 s and render ≈ 140 s — so a threshold of around
    /// five minutes catches a crash instead of fifteen. Seconds-level detection would need the
    /// stamp inside the TTS batch loop, which is `litrpg-tts`'s call, not the engine's.
    fn stamp_heartbeat(&self) {
        let backends = self.config.registered_backends.clone();
        if let Err(e) = self.with_store(|s| {
            s.stamp_engine_heartbeat(std::process::id(), env!("CARGO_PKG_VERSION"), &backends)
        }) {
            warn!(error = %e, "could not stamp the engine heartbeat; carrying on");
        }
    }

    /// Resolve the buffer baseline for this cycle.
    ///
    /// `Stored` hits the database every time it is called, which is the behaviour that makes a
    /// settable cursor mean anything.
    fn resolve_cursor(&self, cursor: BufferCursor) -> Result<u32, EngineError> {
        match cursor {
            // `consumed_through()` answers 0 rather than erroring when there is no story row,
            // so this needs no guard of its own.
            BufferCursor::Stored => self.with_store(|s| s.consumed_through()),
            BufferCursor::At(n) => Ok(n),
            BufferCursor::Drain => self.with_store(|s| s.latest_number()),
        }
    }

    /// One turn of the loop.
    ///
    /// `cursor` fixes where the rendered-ahead buffer measures from; see [`BufferCursor`].
    pub async fn run_cycle(&self, cursor: BufferCursor) -> Result<CycleOutcome, EngineError> {
        let consumed_through = self.resolve_cursor(cursor)?;
        // Unconditionally, and before any branch: a heartbeat that only fires when work happens
        // goes stale on a healthy engine that has caught up, and then every caller has to
        // distinguish "idle" from "dead" — the one question this row exists to answer.
        self.stamp_heartbeat();

        // ---- 0. Resume ------------------------------------------------------
        // A chapter with prose but no audio means either a crash between publish stages
        // or an earlier TTS failure. Re-render it; never regenerate prose that has
        // already shipped.
        //
        // A failed resume **falls through** to normal generation rather than returning.
        // Returning here would mean a chapter that can never render — a `voice_ref` the
        // registry rejects, a manifest `attach_audio` refuses, a backend down for a week —
        // gets picked up first every single cycle and no new chapter is ever produced.
        // §10 says a bookkeeping failure must not cost a chapter; it must equally not cost
        // every chapter after it.
        if let Some(number) = self.next_resumable()? {
            info!(
                chapter = number,
                "resuming render for a chapter with no audio"
            );

            let resumed = match self.replan_from_store(number) {
                Ok(mut planned) => {
                    // Same hazard as a fresh chapter: the persisted segments carry whatever
                    // `voice_ref` was written at the time, which may name a backend this
                    // process did not load.
                    for (speaker, voice) in crate::voices::substitute_unrenderable(
                        &mut planned,
                        &self.config.registered_backends,
                        &self.config.narrator_voice,
                        &self.config.system_voice,
                        &self.config.character_voices,
                    ) {
                        warn!(%speaker, %voice, "substituting an unrenderable voice to resume");
                    }
                    self.render_and_publish(number, &planned).await
                }
                Err(e) => {
                    warn!(chapter = number, error = %e, "could not rebuild segments to resume");
                    false
                }
            };

            if resumed {
                self.clear_resume_failures(number);
                return Ok(CycleOutcome::ResumedRender {
                    chapter: number,
                    has_audio: true,
                });
            }

            let attempts = self.note_resume_failure(number);
            warn!(
                chapter = number,
                attempts,
                max = MAX_RESUME_ATTEMPTS,
                "resume render failed; carrying on with a new chapter so the serial keeps moving"
            );
        }

        // ---- 1. Buffer check ------------------------------------------------
        let buffer_depth = self.with_store(|s| buffer_depth(s, consumed_through))?;
        if buffer_depth >= self.config.buffer_target {
            debug!(
                buffer_depth,
                target = self.config.buffer_target,
                "buffer full; idling"
            );
            return Ok(CycleOutcome::Idle { buffer_depth });
        }

        let number = self.with_store(|s| s.latest_number())? + 1;

        // ---- 2. Drain notes (read only; consumed at step 10) ----------------
        let notes = self.with_store(|s| s.pending_notes())?;
        let note_bodies: Vec<String> = notes.iter().map(|n| n.body.clone()).collect();

        // ---- Assemble ------------------------------------------------------
        let story = self.library.story()?;
        let snapshot = self.with_store(|s| s.snapshot())?;
        let state_rendered = render_state_snapshot(&snapshot);
        let summaries: Vec<ChapterSummary> =
            self.library.recent_summaries(self.config.summary_window)?;
        let all_lore: Vec<LoreEntry> = self.library.lore()?;

        // The retrieval scan window is "last chapter's text + arc outline" (§6.3). The
        // prose is used *only* for keyword matching and never enters the prompt — §6.3's
        // no-verbatim-chapters rule is about what the model reads, not about what the
        // retriever may look at.
        let last_text = self.with_store(last_chapter_text)?;
        let scan = format!("{last_text}\n{}", story.arc_outline_md);
        let lore_hits: Vec<&LoreEntry> = match_lore(&all_lore, &scan);

        let target_words = if story.target_words > 0 {
            story.target_words
        } else {
            self.config.target_words
        };

        let input = Pass1Input {
            chapter_number: number,
            story_prompt: &story.prompt_md,
            arc_outline: &story.arc_outline_md,
            state_snapshot: &state_rendered,
            lore: &lore_hits,
            recent_summaries: &summaries,
            director_notes: &note_bodies,
            target_words,
        };

        let prompt_hash = content_hash(&story.prompt_md);

        // ---- 3. Pass 1 ------------------------------------------------------
        let prose = match self.pass1(&input).await {
            Ok(p) => p,
            Err((reason, backoff)) => {
                warn!(chapter = number, %reason, backoff, "abandoning cycle; no partial chapter");
                return Ok(CycleOutcome::Abandoned {
                    chapter: number,
                    reason,
                    backoff,
                });
            }
        };

        // Pass 1 is the longest Ember phase; stamping here bounds the stale window to it rather
        // than to the whole cycle.
        self.stamp_heartbeat();

        // ---- 4. Parse + assign voices ---------------------------------------
        let parsed = parse_tagged_prose(&prose);
        if parsed.is_empty() {
            return Ok(CycleOutcome::Abandoned {
                chapter: number,
                reason: "pass 1 produced no parseable segments".to_string(),
                backoff: false,
            });
        }

        let speakers = distinct_speakers(&parsed);
        let existing_cast: Vec<(String, String)> = self
            .with_store(|s| s.cast())?
            .into_iter()
            .map(|c| (c.speaker, c.voice_ref))
            .collect();

        let assigner = self.assigner();
        let new_cast = assigner.assign(&speakers, &existing_cast);
        for a in &new_cast {
            info!(speaker = %a.speaker, voice = %a.voice_ref, "casting new speaker");
            self.with_store(|s| s.upsert_cast(&a.speaker, &a.voice_ref, a.kind.as_str(), number))?;
        }

        // Mutable because a gender hint from pass 2 can re-voice a speaker cast this cycle,
        // and that correction has to reach the render.
        let mut planned = plan_segments(
            &parsed,
            &existing_cast,
            &new_cast,
            assigner.narrator_voice(),
        );

        // ---- 5. Pass 2 ------------------------------------------------------
        let plain_text = plain_chapter_text(&planned);
        let known: Vec<String> = self
            .with_store(|s| s.known_subjects())?
            .into_iter()
            // A *naming* decision — which names to offer the model — so `is_reserved` is right
            // here. The stats guard below asks `kind` instead.
            .filter(|s| !speaker::is_reserved(s))
            .collect();
        let extraction = self.pass2(&plain_text, &known).await;
        let state_dirty = extraction.is_none();
        if state_dirty {
            warn!(
                chapter = number,
                "pass 2 failed; chapter ships with state_dirty = 1 and can be re-extracted"
            );
        }

        let title = derive_title(number, extraction.as_ref().map(|e| e.title.as_str()));
        let text_md = chapter_markdown(number, &title, &planned);

        self.with_store(|s| {
            s.insert_chapter(&NewChapter {
                number,
                title: title.clone(),
                text_md: text_md.clone(),
                prompt_hash: prompt_hash.clone(),
                state_dirty,
            })
        })?;

        // The premise is now *in effect*: a chapter exists that was written from it. Stamped here
        // rather than where the prompt was read, because a cycle abandoned in pass 1 produced no
        // chapter and must leave the pending-edit warning standing. Uses the same value written to
        // `chapters.prompt_hash` rather than re-hashing the file, so it can never record a hash no
        // chapter actually used.
        if let Err(e) = self.library.set_prompt_hash(&prompt_hash) {
            // Bookkeeping, so it must not cost the chapter (§10). The cost of failure is a stale
            // pending-edit warning, which the next chapter clears.
            warn!(chapter = number, error = %e, "could not record the prompt hash now in effect");
        }

        // The canonical permanent artifact (§8). Written here rather than in the render
        // stage because the text ships even when audio fails, and a failure to write it must
        // not cost the chapter either — the prose is already durable in `chapters.text_md`.
        if let Err(e) = self.artifacts.write_text(number, &text_md).await {
            warn!(chapter = number, error = %e, "could not write the chapter markdown");
        }

        // ---- 6 + 7. new_lore BEFORE deltas ---------------------------------
        let mut applied = 0usize;
        let mut rejected = 0usize;
        if let Some(e) = &extraction {
            // Canonicalise every name before it becomes durable identity (issue #11). Without
            // this, `litrpg init --protagonist "Kaelen"` against a prompt saying "Kaelen Vord"
            // gives one character two ledger keys, with his stats split so neither view is
            // complete — and an append-only ledger cannot be un-split afterwards.
            let known_before = self.with_store(|s| s.known_subjects())?;
            let canon = |proposed: &str| -> String {
                let r = resolve_subject(proposed, &known_before, &story.protagonist);
                match &r {
                    SubjectResolution::Aliased { from, to } => {
                        info!(%from, %to, "resolved a character name onto the established one")
                    }
                    SubjectResolution::Ambiguous { name, candidates } => warn!(
                        %name,
                        ?candidates,
                        "character name matches several established ones; left as written rather \
                         than guessing, because a wrong merge cannot be undone"
                    ),
                    _ => {}
                }
                r.name().to_string()
            };

            for l in &e.new_lore {
                // `always_on` is false: the model does not get to decide that an entry is
                // injected into every future chapter.
                // Only characters carry identity; a place called "Ashen Vale" must not be
                // resolved against a person.
                let lore_name = if l.kind.eq_ignore_ascii_case("character") {
                    canon(&l.name)
                } else {
                    l.name.clone()
                };
                self.with_store(|s| {
                    s.upsert_lore(
                        &lore_name,
                        &l.kind,
                        &l.keywords,
                        &l.body_md,
                        l.priority as i64,
                        false,
                        number,
                    )
                })?;
            }

            // Gender hints arrive with `new_lore`, i.e. after step 4 already cast this
            // chapter's speakers. Correct those rows now: nothing has been synthesised yet, so
            // the character's very first audio is already in a matching voice. Established
            // cast members are never touched.
            // `speakers` is the general source: it covers everyone who spoke, including the
            // protagonist, so a hint arrives on every chapter rather than only on one that
            // introduces someone. `new_lore` is the specific case and wins on a conflict,
            // since a genuinely new character is described there in more detail.
            //
            // Keyed lowercase so two spellings of one name cannot both sit in the map.
            let mut wanted: BTreeMap<String, String> = BTreeMap::new();
            for sp in &e.speakers {
                if let Some(g) = sp.gender_hint() {
                    wanted.insert(canon(&sp.name).to_lowercase(), g.to_string());
                }
            }
            for l in &e.new_lore {
                if let Some(g) = l.gender_hint() {
                    wanted.insert(canon(&l.name).to_lowercase(), g.to_string());
                }
            }
            if !wanted.is_empty() && !new_cast.is_empty() {
                let all_voices: Vec<String> = existing_cast
                    .iter()
                    .map(|(_, v)| v.clone())
                    .chain(new_cast.iter().map(|a| a.voice_ref.clone()))
                    .collect();

                for fixed in assigner.regender(&new_cast, &wanted, &all_voices) {
                    info!(
                        speaker = %fixed.speaker,
                        voice = %fixed.voice_ref,
                        "re-cast to a gender-matched voice"
                    );
                    self.with_store(|s| {
                        s.upsert_cast(
                            &fixed.speaker,
                            &fixed.voice_ref,
                            fixed.kind.as_str(),
                            number,
                        )
                    })?;
                    for seg in planned
                        .iter_mut()
                        .filter(|p| same_speaker(&p.speaker, &fixed.speaker))
                    {
                        seg.voice_ref = fixed.voice_ref.clone();
                    }
                }
            }

            // Fetched after the cast upserts and after `new_lore`, so a character introduced this
            // chapter is judged by their own row rather than by absence.
            let cast_kinds: Vec<(String, String)> = self
                .with_store(|s| s.cast())?
                .into_iter()
                .map(|c| (c.speaker, c.kind))
                .collect();

            for pd in &e.deltas {
                let delta = match pd.to_delta() {
                    Ok(mut d) => {
                        // Resolved against the subjects known *after* `new_lore` landed, so a
                        // character this chapter introduced anchors its own deltas.
                        d.subject = {
                            let known_now = self.with_store(|s| s.known_subjects())?;
                            let r = resolve_subject(&d.subject, &known_now, &story.protagonist);
                            if let SubjectResolution::Aliased { from, to } = &r {
                                info!(%from, %to, "resolved a delta subject onto the established name");
                            }
                            r.name().to_string()
                        };
                        d
                    }
                    Err(err) => {
                        warn!(%err, "discarding a delta with an illegal op");
                        rejected += 1;
                        continue;
                    }
                };

                // The gate would *accept* these, because `narrator` and `SYSTEM` are cast
                // rows and therefore known subjects. Stopping them here keeps a stat block's
                // numbers from accruing to a voice instead of to the character it describes.
                if is_placeholder_value(&delta) {
                    warn!(
                        subject = %delta.subject,
                        field = %delta.field,
                        value = ?delta.value_txt,
                        "refusing a placeholder value; the gate would record it as fact"
                    );
                    rejected += 1;
                    continue;
                }

                if subject_is_a_role(&delta.subject, &cast_kinds) {
                    warn!(
                        subject = %delta.subject,
                        field = %delta.field,
                        "refusing a delta whose subject is a role row, not a character"
                    );
                    rejected += 1;
                    continue;
                }
                match self.with_store(|s| s.append_delta(number, &delta))? {
                    Ok(()) => applied += 1,
                    Err(reason) => {
                        // Stored with applied = 0, not discarded. A rising rejection rate
                        // is §6.2's early warning that the prompt or state format is
                        // drifting -- measurable, rather than discovered at chapter 60.
                        warn!(
                            subject = %delta.subject,
                            field = %delta.field,
                            code = reason.code(),
                            "delta rejected by the gate"
                        );
                        rejected += 1;
                    }
                }
            }

            self.library.put_summary(number, &e.summary)?;
        }

        // Immediately before the longest phase, so a crash inside TTS shows as staleness measured
        // from the render's start rather than from the cycle's.
        self.stamp_heartbeat();

        // ---- 8 + 9. Render and publish --------------------------------------
        // A `cast` row can name a backend this process did not load. Substitute before
        // rendering, or one such row costs the whole chapter's audio.
        for (speaker, voice) in crate::voices::substitute_unrenderable(
            &mut planned,
            &self.config.registered_backends,
            &self.config.narrator_voice,
            &self.config.system_voice,
            &self.config.character_voices,
        ) {
            warn!(
                %speaker,
                %voice,
                "cast voice is not renderable by the loaded backends; substituting for this \
                 run only (the cast row is left alone, so a build with that backend restores it)"
            );
        }

        let has_audio = self.render_and_publish(number, &planned).await;

        self.stamp_heartbeat();

        // ---- 10. Notes are consumed once the chapter is durable --------------
        // Unconditional on the render: the notes were honoured by the prose, which is
        // written. Leaving them pending would re-apply them to the next chapter too.
        let consumed = self.with_store(|s| s.mark_notes_consumed(number))?;
        debug!(chapter = number, consumed, "notes marked consumed");

        info!(
            chapter = number,
            has_audio, state_dirty, applied, rejected, "chapter published"
        );
        Ok(CycleOutcome::Produced {
            chapter: number,
            has_audio,
            state_dirty,
            applied,
            rejected,
        })
    }

    /// Pass 1 with jittered retries. `Err((reason, backoff))` abandons the cycle.
    async fn pass1(&self, input: &Pass1Input<'_>) -> Result<String, (String, bool)> {
        let mut last = "pass 1 was never attempted".to_string();

        for (attempt, temp) in PASS1_TEMPERATURES.iter().enumerate() {
            match self.generator.pass1(input, *temp).await {
                Ok(prose) if !prose.trim().is_empty() => return Ok(prose),
                Ok(_) => last = "pass 1 returned empty prose".to_string(),
                Err(e) => {
                    // A network failure wants backoff, not a re-prompt: hammering an
                    // unreachable Ember just drains the buffer faster.
                    if e.is_transport() {
                        return Err((e.to_string(), true));
                    }
                    // A 400 will be a 400 every time.
                    if !e.is_retryable() {
                        return Err((e.to_string(), false));
                    }
                    last = e.to_string();
                }
            }
            warn!(attempt, temperature = temp, %last, "pass 1 attempt failed; jittering");
        }

        Err((last, false))
    }

    /// Pass 2 with jittered retries. `None` means the chapter ships `state_dirty`.
    async fn pass2(&self, chapter_text: &str, known: &[String]) -> Option<Extraction> {
        for (attempt, temp) in PASS2_TEMPERATURES.iter().enumerate() {
            debug!(attempt, temperature = temp, "extraction attempt");
            match self.generator.pass2(chapter_text, known).await {
                Ok(e) => return Some(e),
                Err(e) => {
                    if e.is_transport() || !e.is_retryable() {
                        warn!(error = %e, "extraction gave up early");
                        return None;
                    }
                    warn!(attempt, error = %e, "extraction attempt failed");
                }
            }
        }
        None
    }

    /// Render, write artifacts, attach audio. Returns whether audio ended up attached.
    ///
    /// Never propagates: §10 says the text ships with `has_audio = false` and the render
    /// is retried later, so a TTS or filesystem failure here is logged, not raised.
    async fn render_and_publish(&self, number: u32, planned: &[PlannedSegment]) -> bool {
        match self.try_render_and_publish(number, planned).await {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    chapter = number,
                    error = %e,
                    "render failed; text ships with has_audio = false and will be retried"
                );
                false
            }
        }
    }

    async fn try_render_and_publish(
        &self,
        number: u32,
        planned: &[PlannedSegment],
    ) -> Result<(), EngineError> {
        // Split here and nowhere else: the markdown and the pass-2 text are already built from
        // whole turns, and doing it at the last moment covers the fresh and resume paths with one
        // call. Idempotent, so re-splitting stored segments on a resume is a no-op.
        let planned: Vec<PlannedSegment> = if self.config.sentence_manifest {
            let split = crate::render::split_by_sentence(planned.to_vec(), sentence_pieces);
            debug!(
                turns = planned.len(),
                entries = split.len(),
                "split speaker turns into per-sentence manifest entries"
            );
            split
        } else {
            planned.to_vec()
        };
        let planned = planned.as_slice();

        let requests = plan_requests(planned)?;
        let parts = self.renderer.render_all(&requests).await?;
        let rendered = assemble(number, planned, parts)?;

        // The files still get written; the store simply no longer records where they went.
        // Chapter media lives at `media_dir/NNNN.{pcm,mp3}` by construction, so a stored path
        // could only restate the layout — and a restatement can disagree with it, which is
        // exactly how the old absolute paths came to point at a deleted directory.
        let pcm_path = self.artifacts.write_pcm(number, &rendered.pcm).await?;
        self.artifacts.encode_mp3(number, &pcm_path).await?;
        self.artifacts
            .write_manifest(number, &rendered.manifest)
            .await?;

        self.with_store(|s| s.attach_audio(number, &rendered.manifest))?;
        Ok(())
    }

    /// Voice assigner built from config, so a deployment can swap backends without code.
    fn assigner(&self) -> VoiceAssigner {
        VoiceAssigner::with_voices(
            self.config.narrator_voice.clone(),
            self.config.system_voice.clone(),
            self.config.character_voices.clone(),
        )
        .with_genders(self.config.voice_genders.clone())
    }

    /// The lowest-numbered chapter that has prose, no audio, and retries left.
    fn next_resumable(&self) -> Result<Option<u32>, EngineError> {
        let unrendered = self.with_store(unrendered_chapters)?;
        Ok(unrendered
            .into_iter()
            .find(|n| self.resume_attempts(*n) < MAX_RESUME_ATTEMPTS))
    }

    fn note_resume_failure(&self, number: u32) -> u32 {
        let mut map = self
            .resume_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let slot = map.entry(number).or_insert(0);
        *slot += 1;
        *slot
    }

    fn clear_resume_failures(&self, number: u32) {
        self.resume_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&number);
    }

    /// Rebuild the planned segments for an already-written chapter, exposed so a test can check
    /// that a re-render honours the cast rather than replaying stored voices.
    pub fn replan_for_test(&self, number: u32) -> Result<Vec<PlannedSegment>, EngineError> {
        self.replan_from_store(number)
    }

    /// Rebuild the planned segments for an already-written chapter, for a resumed render.
    ///
    /// Prefers the persisted `segments` rows; falls back to re-parsing `text_md` when a
    /// crash landed between `insert_chapter` and `attach_audio`, which is precisely when
    /// no segment rows exist yet.
    fn replan_from_store(&self, number: u32) -> Result<Vec<PlannedSegment>, EngineError> {
        let segments = self.with_store(|s| s.segments(number))?;
        if !segments.is_empty() {
            // The stored rows supply the chapter's *content* — idx, speaker, kind, text — because
            // that has already been published and must not change. The **voice is re-derived from
            // the cast**, which is the authoritative identity, rather than reused from the row.
            //
            // A segment's `voice_ref` records what was *rendered*, not what the character's voice
            // *is*. Reusing it makes a re-render faithfully reproduce whatever the last render
            // happened to use, which defeats the entire purpose of §9.2's `litrpg render N`
            // ("re-render audio only, e.g. after a cast change") — a cast override could never take
            // effect. It is also what made a substituted voice permanent: chapters rendered by an
            // Azure-only build stayed Azure even when re-rendered by a build that had sherpa,
            // because the rows kept saying Azure while the cast said cori.
            let cast: Vec<(String, String)> = self
                .with_store(|s| s.cast())?
                .into_iter()
                .map(|c| (c.speaker, c.voice_ref))
                .collect();

            return Ok(segments
                .into_iter()
                .map(|s| {
                    let voice_ref = match cast
                        .iter()
                        .find(|(sp, _)| same_speaker(sp, &s.speaker))
                        .map(|(_, v)| v.clone())
                    {
                        Some(v) => v,
                        None => {
                            // Should be impossible — and that is exactly why it warns. A rendered
                            // segment naming a speaker the cast has forgotten would otherwise
                            // happen quietly and stay that way for months.
                            warn!(
                                speaker = %s.speaker,
                                chapter = number,
                                "segment references a speaker with no cast row; reusing the stored voice"
                            );
                            s.voice_ref
                        }
                    };
                    PlannedSegment {
                        idx: s.idx,
                        speaker: s.speaker,
                        kind: s.kind,
                        voice_ref,
                        text: s.text,
                    }
                })
                .collect());
        }

        let row = self.with_store(|s| s.chapter(number))?;
        let parsed = parse_tagged_prose(&strip_title(&row.text_md));
        let existing_cast: Vec<(String, String)> = self
            .with_store(|s| s.cast())?
            .into_iter()
            .map(|c| (c.speaker, c.voice_ref))
            .collect();

        // Any speaker missing from the cast gets one now, deterministically, and it is
        // persisted so a second resume produces identical audio.
        let assigner = self.assigner();
        let new_cast = assigner.assign(&distinct_speakers(&parsed), &existing_cast);
        for a in &new_cast {
            self.with_store(|s| s.upsert_cast(&a.speaker, &a.voice_ref, a.kind.as_str(), number))?;
        }

        Ok(plan_segments(
            &parsed,
            &existing_cast,
            &new_cast,
            assigner.narrator_voice(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Distinct speakers in order of first appearance, deduplicated case-insensitively to
/// match the parser's canonicalisation.
pub fn distinct_speakers(parsed: &[ParsedSegment]) -> Vec<ParsedSpeaker> {
    let mut out: Vec<ParsedSpeaker> = Vec::new();
    for seg in parsed {
        if !out.iter().any(|p| same_speaker(&p.speaker, &seg.speaker)) {
            out.push(ParsedSpeaker {
                speaker: seg.speaker.clone(),
                kind: seg.kind,
            });
        }
    }
    out
}

/// Attach a `voice_ref` to every parsed segment.
///
/// Falls back to the narrator voice if a speaker somehow has no assignment; that would be
/// a bug, but rendering a segment in the wrong voice beats dropping it from the chapter.
pub fn plan_segments(
    parsed: &[ParsedSegment],
    existing_cast: &[(String, String)],
    new_cast: &[CastAssignment],
    narrator_voice: &str,
) -> Vec<PlannedSegment> {
    let lookup = |speaker: &str| -> String {
        new_cast
            .iter()
            .find(|a| same_speaker(&a.speaker, speaker))
            .map(|a| a.voice_ref.clone())
            .or_else(|| {
                existing_cast
                    .iter()
                    .find(|(s, _)| same_speaker(s, speaker))
                    .map(|(_, v)| v.clone())
            })
            .unwrap_or_else(|| narrator_voice.to_string())
    };

    parsed
        .iter()
        .map(|seg| PlannedSegment {
            idx: seg.idx,
            speaker: seg.speaker.clone(),
            kind: seg.kind,
            voice_ref: lookup(&seg.speaker),
            text: seg.text.clone(),
        })
        .collect()
}

/// The chapter as continuous prose, for the extraction pass.
///
/// Tags are dropped but `SYSTEM` blocks are kept: a stat block is often the only place a
/// number is stated outright, so removing it would hide exactly what pass 2 is looking for.
pub fn plain_chapter_text(planned: &[PlannedSegment]) -> String {
    planned
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Drop the `# Chapter N: Title` heading that [`chapter_markdown`] adds, so re-parsing a
/// stored chapter does not turn its title into a narrator segment.
fn strip_title(text_md: &str) -> String {
    text_md
        .lines()
        .skip_while(|l| l.trim_start().starts_with('#') || l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The chapter title, from pass 2's `title` field.
///
/// Measured 2026-07-29: deriving this from the *summary* produced
/// `"No stat changes, inventory changes, or location changes are…"`, because a summary is
/// written for a bookkeeping audience and sometimes describes the extraction rather than
/// the story. A title is content, so the model that just read the chapter writes it.
/// Falling back to `Chapter N` keeps a `state_dirty` chapter perfectly serviceable.
pub fn derive_title(number: u32, summary: Option<&str>) -> String {
    let fallback = format!("Chapter {number}");
    let Some(s) = summary else { return fallback };

    let first = s
        .trim()
        .split_terminator(['.', '!', '?', '\n'])
        .next()
        .unwrap_or("")
        .trim();
    if first.is_empty() || is_placeholder_title(first) {
        return fallback;
    }
    if first.chars().count() <= TITLE_MAX {
        return first.to_string();
    }

    // Truncate on a word boundary so a title never ends mid-word.
    let mut out = String::new();
    for word in first.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > TITLE_MAX {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        fallback
    } else {
        format!("{out}…")
    }
}

/// Whether a model-supplied title is a template stub rather than a name.
///
/// Measured live: pass 2 returned `"Chapter [Insert Chapter Number]"`. Publishing that as an
/// RSS item title, a Candela chapter name and a line on the watch would look like a bug in
/// every client at once, so an obvious stub falls back to `Chapter N` — which is at least
/// honest.
fn is_placeholder_title(t: &str) -> bool {
    let lower = t.to_lowercase();
    t.contains('[')
        || t.contains(']')
        || t.contains('<')
        || lower.starts_with("chapter")
        || lower.starts_with("untitled")
        || lower.contains("insert")
}

/// Text values that assert nothing and must never be recorded as state.
///
/// Measured live: a chapter whose `[SYSTEM]` block printed a full character sheet produced
/// **52 applied deltas**, most of them `appear:eyes = "unknown"`, `appear:build = "unknown"`
/// and so on. Nothing was rejected, because the gate accepts any string for a text field —
/// correctly, since `""` legitimately means "slot is empty" (§6.0). So the character screen
/// would have shown "eyes: unknown" as established fact, forever, from a placeholder.
///
/// `""` is deliberately **not** here: an empty string is a real, documented value.
const PLACEHOLDER_VALUES: &[&str] = &[
    "unknown",
    "none",
    "n/a",
    "na",
    "tbd",
    "not specified",
    "unspecified",
    "not stated",
    "not set",
    "null",
    "-",
    "--",
    "?",
    "???",
];

/// Whether a delta's text value is a placeholder rather than a fact.
fn is_placeholder_value(d: &litrpg_core::Delta) -> bool {
    let Some(txt) = d.value_txt.as_deref() else {
        return false;
    };
    let t = txt.trim().to_ascii_lowercase();
    if t.is_empty() {
        return false; // "" means "slot is empty", which is information.
    }
    PLACEHOLDER_VALUES.contains(&t.as_str())
}

/// Whether a delta's subject names a **role row** rather than a person.
///
/// `cast` holds a row for `narrator` and `SYSTEM` because they need voices, and the store's
/// `known_subjects()` unions every cast speaker — so both are offered to pass 2 as legitimate
/// subjects and accepted by the gate. Measured live: pass 2 attributed a whole stat block to
/// `subject: "SYSTEM"`, so Kaelen's inventory landed under a pseudo-person while his own
/// character screen stayed empty.
///
/// # Decided by `kind`, not by name
///
/// This used to ask whether the *name* was `narrator` or `SYSTEM`, which is a different question
/// and can give a different answer: a character legitimately named `System` in the prose is
/// excluded by the name rule and included by the kind rule. `core::speaker::is_reserved` decides
/// what to **call** something; a row's `kind` is the only authority on whether it can **hold
/// stats**, so the stats guard asks `kind`.
///
/// The visible consequence: a character called `System` now keeps their stats, because their cast
/// row says `character`.
fn subject_is_a_role(subject: &str, cast: &[(String, String)]) -> bool {
    // `kind` is the authority. Look the subject up in the cast and ask what kind of row it is,
    // rather than asking what it is called.
    if let Some((speaker, kind)) = cast.iter().find(|(sp, _)| same_speaker(sp, subject)) {
        return match SpeakerKind::from_canonical(kind) {
            Some(SpeakerKind::Narrator | SpeakerKind::System) => true,
            Some(SpeakerKind::Character) => false,
            None => {
                // The row's kind is not a value we wrote, so the authority is unreadable. Fall
                // back to the *name*, which is the weaker rule, and say so — a corrupt kind on a
                // row called `narrator` is still almost certainly the narrator, and letting stats
                // accrue to it is the failure this guard exists to stop.
                let reserved = speaker::is_reserved(speaker);
                warn!(
                    %speaker,
                    kind = %kind,
                    reserved,
                    "cast row has an unreadable kind; falling back to the name to decide personhood"
                );
                reserved
            }
        };
    }

    // Not in the cast at all. Nothing to judge by kind, and the validation gate's
    // `UnknownSubject` check is the thing that catches an invented subject — so this guard
    // declines to answer rather than guessing from the name.
    false
}

/// Every chapter that has prose but no audio, oldest first.
fn unrendered_chapters(store: &Store) -> litrpg_store::Result<Vec<u32>> {
    store.chapters_missing_audio()
}

/// Chapters that are rendered and still ahead of the listener.
fn buffer_depth(store: &Store, consumed_through: u32) -> litrpg_store::Result<u32> {
    Ok(store
        .chapters_since(consumed_through)?
        .iter()
        .filter(|c| c.has_audio)
        .count() as u32)
}

fn last_chapter_text(store: &Store) -> litrpg_store::Result<String> {
    let latest = store.latest_number()?;
    if latest == 0 {
        return Ok(String::new());
    }
    Ok(store.chapter(latest)?.text_md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use litrpg_core::SpeakerKind;

    fn seg(idx: u32, speaker: &str, kind: SpeakerKind, text: &str) -> ParsedSegment {
        ParsedSegment {
            idx,
            speaker: speaker.to_string(),
            kind,
            text: text.to_string(),
        }
    }

    #[test]
    fn distinct_speakers_preserves_first_appearance_and_dedups() {
        let parsed = vec![
            seg(0, "narrator", SpeakerKind::Narrator, "a"),
            seg(1, "Kaelen", SpeakerKind::Character, "b"),
            seg(2, "narrator", SpeakerKind::Narrator, "c"),
            seg(3, "KAELEN", SpeakerKind::Character, "d"),
            seg(4, "SYSTEM", SpeakerKind::System, "e"),
        ];
        let speakers = distinct_speakers(&parsed);
        let names: Vec<&str> = speakers.iter().map(|p| p.speaker.as_str()).collect();
        assert_eq!(names, vec!["narrator", "Kaelen", "SYSTEM"]);
    }

    #[test]
    fn the_prompt_hash_is_cores_canonical_one_not_a_local_copy() {
        // `story.prompt_hash` is written by the CLI with `litrpg_core::content_hash`,
        // which renders as `fnv1a64:<16 hex>`. A bare-hex hash here would compare unequal
        // to it forever, and §9.3's whole point is asking "did chapter 40 come from the
        // prompt I have now?".
        assert_eq!(content_hash("abc"), litrpg_core::content_hash("abc"));
        assert!(content_hash("abc").starts_with("fnv1a64:"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
    }

    #[test]
    fn derive_title_uses_the_first_sentence_of_the_summary() {
        assert_eq!(
            derive_title(4, Some("Kaelen broke the first seal. Sera watched.")),
            "Kaelen broke the first seal"
        );
    }

    #[test]
    fn derive_title_rejects_the_placeholder_titles_the_model_really_emits() {
        // All measured or one edit away from measured.
        for junk in [
            "Chapter [Insert Chapter Number]",
            "Chapter 1",
            "chapter one: the vale",
            "[Chapter Title]",
            "Untitled",
            "Insert title here",
            "<title>",
        ] {
            assert_eq!(
                derive_title(7, Some(junk)),
                "Chapter 7",
                "{junk:?} is a stub and must not be published as a chapter name"
            );
        }
    }

    #[test]
    fn derive_title_keeps_a_real_title() {
        for good in [
            "The First Seal Breaks",
            "A Debt Collected in Ash",
            "Sera Names Her Price",
        ] {
            assert_eq!(derive_title(7, Some(good)), good);
        }
    }

    #[test]
    fn placeholder_text_values_are_refused_but_an_empty_slot_is_not() {
        let txt = |v: Option<&str>| litrpg_core::Delta {
            subject: "Kaelen".into(),
            field: "appear:eyes".into(),
            op: litrpg_core::Op::Set,
            value_num: None,
            value_txt: v.map(str::to_string),
        };

        for junk in [
            "unknown",
            "Unknown",
            "  UNKNOWN  ",
            "none",
            "N/A",
            "tbd",
            "not specified",
            "-",
            "???",
            "null",
        ] {
            assert!(
                is_placeholder_value(&txt(Some(junk))),
                "{junk:?} asserts nothing and must not become state"
            );
        }

        // Real values, including the documented empty slot.
        for real in [
            "",
            "grey",
            "a scar through the left brow",
            "0",
            "Blade of Unpaid Debts",
        ] {
            assert!(
                !is_placeholder_value(&txt(Some(real))),
                "{real:?} is a real value"
            );
        }

        // A numeric delta has no text value and is unaffected.
        assert!(!is_placeholder_value(&txt(None)));
    }

    #[test]
    fn a_role_row_cannot_hold_stats_but_a_character_named_system_can() {
        let cast = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter()
                .map(|(s, k)| (s.to_string(), k.to_string()))
                .collect()
        };

        let roles = cast(&[
            ("narrator", "narrator"),
            ("SYSTEM", "system"),
            ("Kaelen", "character"),
        ]);
        assert!(subject_is_a_role("narrator", &roles));
        assert!(
            subject_is_a_role("NARRATOR", &roles),
            "case-insensitive via same_speaker"
        );
        assert!(subject_is_a_role("SYSTEM", &roles));
        assert!(subject_is_a_role("system", &roles));
        assert!(!subject_is_a_role("Kaelen", &roles));

        // The behaviour that changed, and the reason the two rules must not be collapsed: a
        // character the prose calls `System` has a cast row of kind `character`, so their stats
        // are theirs. The old name-based rule silently discarded them.
        let person = cast(&[("System", "character")]);
        assert!(
            !subject_is_a_role("System", &person),
            "kind is the authority on personhood, not the name"
        );
        assert!(
            speaker::is_reserved("System"),
            "while the *name* is still reserved — the two questions differ, which is the point"
        );

        // A name merely containing a role word was never a role.
        assert!(!subject_is_a_role(
            "Systemsmith",
            &cast(&[("Systemsmith", "character")])
        ));

        // Absent from the cast: this guard declines to answer, and `UnknownSubject` catches it.
        assert!(!subject_is_a_role("Nobody", &roles));
    }

    #[test]
    fn an_unreadable_kind_falls_back_to_the_name() {
        // Defensive: if a row's kind is not a value we wrote, the authority is unreadable, so the
        // weaker name rule applies rather than letting stats accrue to the narrator.
        let corrupt = vec![("narrator".to_string(), "Narrator".to_string())];
        assert!(
            subject_is_a_role("narrator", &corrupt),
            "a corrupt kind on a row called narrator is still the narrator"
        );

        let corrupt_person = vec![("Kaelen".to_string(), "wizard".to_string())];
        assert!(
            !subject_is_a_role("Kaelen", &corrupt_person),
            "an unreadable kind on a non-reserved name stays a person"
        );
    }

    #[test]
    fn derive_title_falls_back_when_there_is_no_summary() {
        assert_eq!(derive_title(9, None), "Chapter 9");
        assert_eq!(derive_title(9, Some("   ")), "Chapter 9");
        assert_eq!(derive_title(9, Some(".")), "Chapter 9");
    }

    #[test]
    fn derive_title_truncates_on_a_word_boundary() {
        let long = "a ".repeat(80);
        let t = derive_title(1, Some(&long));
        assert!(t.chars().count() <= TITLE_MAX + 1, "got {t:?}");
        assert!(t.ends_with('…'));
        assert!(!t.contains("  "));
    }

    #[test]
    fn strip_title_removes_only_the_heading() {
        let md = "# Chapter 3: Ash\n\n[narrator] The vale.\n\n[Kaelen] \"Hi.\"\n";
        let stripped = strip_title(md);
        assert!(!stripped.contains("# Chapter"));
        assert!(stripped.contains("[narrator] The vale."));
        assert!(stripped.contains("[Kaelen] \"Hi.\""));
    }

    #[test]
    fn plan_segments_prefers_new_assignments_then_existing_cast() {
        let parsed = vec![
            seg(0, "narrator", SpeakerKind::Narrator, "a"),
            seg(1, "Kaelen", SpeakerKind::Character, "b"),
            seg(2, "Sera", SpeakerKind::Character, "c"),
        ];
        let existing = [("Kaelen".to_string(), "sherpa:x:1".to_string())];
        let new = [CastAssignment {
            speaker: "Sera".into(),
            kind: SpeakerKind::Character,
            voice_ref: "sherpa:x:2".into(),
        }];

        let planned = plan_segments(&parsed, &existing, &new, "narr:0");
        assert_eq!(planned[0].voice_ref, "narr:0");
        assert_eq!(planned[1].voice_ref, "sherpa:x:1");
        assert_eq!(planned[2].voice_ref, "sherpa:x:2");
    }

    #[test]
    fn plan_segments_never_drops_a_segment_with_an_unknown_speaker() {
        let parsed = vec![seg(0, "Ghost", SpeakerKind::Character, "boo")];
        let planned = plan_segments(&parsed, &[], &[], "narr:0");
        assert_eq!(planned.len(), 1, "a segment must never vanish");
        assert_eq!(planned[0].voice_ref, "narr:0");
    }

    #[test]
    fn plain_chapter_text_drops_tags_but_keeps_system_blocks() {
        let planned = vec![
            PlannedSegment {
                idx: 0,
                speaker: "SYSTEM".into(),
                kind: SpeakerKind::System,
                voice_ref: "v".into(),
                text: "XP gained: 150".into(),
            },
            PlannedSegment {
                idx: 1,
                speaker: "narrator".into(),
                kind: SpeakerKind::Narrator,
                voice_ref: "v".into(),
                text: "The vale.".into(),
            },
        ];
        let text = plain_chapter_text(&planned);
        assert!(text.contains("XP gained: 150"), "pass 2 needs the numbers");
        assert!(!text.contains("[SYSTEM]"));
    }
}
