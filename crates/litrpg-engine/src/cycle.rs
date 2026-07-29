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

use std::sync::Mutex;

use litrpg_ember::prompt::{ChapterSummary, LoreEntry, Pass1Input, render_state_snapshot};
use litrpg_ember::{Extraction, ParsedSegment, match_lore, parse_tagged_prose};
use litrpg_store::{NewChapter, Store};
use tracing::{debug, info, warn};

use crate::cast::{CastAssignment, ParsedSpeaker, VoiceAssigner};
use crate::error::{CycleOutcome, EngineError};
use crate::ports::{Artifacts, Generator, Library, Renderer};
use crate::render::{PlannedSegment, assemble, chapter_markdown, plan_requests};

/// Spec §6.3 — the last five chapter summaries.
pub const SUMMARY_WINDOW: usize = 5;

/// Pass 1: one attempt plus two jittered retries (§10).
pub const PASS1_TEMPERATURES: &[f64] = &[0.9, 0.95, 1.0];

/// Pass 2 wants determinism, so it starts at 0. The jitter exists only to shake the model
/// off a malformed generation it would otherwise reproduce exactly.
pub const PASS2_TEMPERATURES: &[f64] = &[0.0, 0.15, 0.3];

/// Longest derived chapter title before truncation.
const TITLE_MAX: usize = 60;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub buffer_target: u32,
    pub target_words: u32,
    pub narrator_voice: String,
    pub summary_window: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            buffer_target: litrpg_config::MIN_BUFFER_TARGET + 1,
            target_words: litrpg_ember::DEFAULT_TARGET_WORDS,
            narrator_voice: crate::cast::NARRATOR_FALLBACK_VOICE.to_string(),
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
    store: Mutex<Store>,
    generator: G,
    renderer: R,
    library: L,
    artifacts: A,
    config: EngineConfig,
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
        Self {
            store: Mutex::new(store),
            generator,
            renderer,
            library,
            artifacts,
            config,
        }
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

    /// Hand the store back, so a caller can hold the engine and still run CLI queries
    /// (`status`, `cast`, `rewind`) against the same database.
    pub fn into_store(self) -> Store {
        self.store.into_inner().unwrap_or_else(|e| e.into_inner())
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

    /// One turn of the loop.
    ///
    /// `consumed_through` is the highest chapter the listener has finished, used for the
    /// buffer depth in step 1. Pass `0` when nothing is known — the schema has no
    /// playback cursor yet (see the notes in `morpheus-engine.md`).
    pub async fn run_cycle(&self, consumed_through: u32) -> Result<CycleOutcome, EngineError> {
        // ---- 0. Resume ------------------------------------------------------
        // A chapter with prose but no audio means either a crash between publish
        // stages or an earlier TTS failure. Re-render it; never regenerate prose that
        // has already shipped.
        if let Some(number) = self.with_store(unrendered_chapter)? {
            info!(
                chapter = number,
                "resuming render for a chapter with no audio"
            );
            let planned = self.replan_from_store(number)?;
            let has_audio = self.render_and_publish(number, &planned).await;
            return Ok(CycleOutcome::ResumedRender {
                chapter: number,
                has_audio,
            });
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

        let prompt_hash = stable_hash(&story.prompt_md);

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

        let assigner = VoiceAssigner::new(self.config.narrator_voice.clone());
        let new_cast = assigner.assign(&speakers, &existing_cast);
        for a in &new_cast {
            info!(speaker = %a.speaker, voice = %a.voice_ref, "casting new speaker");
            self.with_store(|s| s.upsert_cast(&a.speaker, &a.voice_ref, kind_str(a.kind), number))?;
        }

        let planned = plan_segments(
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
            .collect();
        let extraction = self.pass2(&plain_text, &known).await;
        let state_dirty = extraction.is_none();
        if state_dirty {
            warn!(
                chapter = number,
                "pass 2 failed; chapter ships with state_dirty = 1 and can be re-extracted"
            );
        }

        let title = derive_title(number, extraction.as_ref().map(|e| e.summary.as_str()));
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

        // ---- 6 + 7. new_lore BEFORE deltas ---------------------------------
        let mut applied = 0usize;
        let mut rejected = 0usize;
        if let Some(e) = &extraction {
            for l in &e.new_lore {
                // `always_on` is false: the model does not get to decide that an entry is
                // injected into every future chapter.
                self.with_store(|s| {
                    s.upsert_lore(
                        &l.name,
                        &l.kind,
                        &l.keywords,
                        &l.body_md,
                        l.priority as i64,
                        false,
                        number,
                    )
                })?;
            }

            for pd in &e.deltas {
                let delta = match pd.to_delta() {
                    Ok(d) => d,
                    Err(err) => {
                        warn!(%err, "discarding a delta with an illegal op");
                        rejected += 1;
                        continue;
                    }
                };
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

        // ---- 8 + 9. Render and publish --------------------------------------
        let has_audio = self.render_and_publish(number, &planned).await;

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
        let requests = plan_requests(planned)?;
        let parts = self.renderer.render_all(&requests).await?;
        let rendered = assemble(number, planned, parts)?;

        let pcm_path = self.artifacts.write_pcm(number, &rendered.pcm).await?;
        let mp3_path = self.artifacts.encode_mp3(number, &pcm_path).await?;
        self.artifacts
            .write_manifest(number, &rendered.manifest)
            .await?;

        self.with_store(|s| s.attach_audio(number, &rendered.manifest, &pcm_path, &mp3_path))?;
        Ok(())
    }

    /// Rebuild the planned segments for an already-written chapter, for a resumed render.
    ///
    /// Prefers the persisted `segments` rows; falls back to re-parsing `text_md` when a
    /// crash landed between `insert_chapter` and `attach_audio`, which is precisely when
    /// no segment rows exist yet.
    fn replan_from_store(&self, number: u32) -> Result<Vec<PlannedSegment>, EngineError> {
        let segments = self.with_store(|s| s.segments(number))?;
        if !segments.is_empty() {
            return Ok(segments
                .into_iter()
                .map(|s| PlannedSegment {
                    idx: s.idx,
                    speaker: s.speaker,
                    kind: s.kind,
                    voice_ref: s.voice_ref,
                    text: s.text,
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
        let assigner = VoiceAssigner::new(self.config.narrator_voice.clone());
        let new_cast = assigner.assign(&distinct_speakers(&parsed), &existing_cast);
        for a in &new_cast {
            self.with_store(|s| s.upsert_cast(&a.speaker, &a.voice_ref, kind_str(a.kind), number))?;
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
        if !out
            .iter()
            .any(|p| p.speaker.eq_ignore_ascii_case(&seg.speaker))
        {
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
            .find(|a| a.speaker.eq_ignore_ascii_case(speaker))
            .map(|a| a.voice_ref.clone())
            .or_else(|| {
                existing_cast
                    .iter()
                    .find(|(s, _)| s.eq_ignore_ascii_case(speaker))
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

fn kind_str(kind: litrpg_core::SpeakerKind) -> &'static str {
    match kind {
        litrpg_core::SpeakerKind::Narrator => "narrator",
        litrpg_core::SpeakerKind::Character => "character",
        litrpg_core::SpeakerKind::System => "system",
    }
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

/// A chapter title derived from the extraction summary.
///
/// Deterministic and free. Asking the model for a title would be another round trip and
/// another thing to fail; falling back to `Chapter N` keeps a `state_dirty` chapter
/// perfectly serviceable.
pub fn derive_title(number: u32, summary: Option<&str>) -> String {
    let fallback = format!("Chapter {number}");
    let Some(s) = summary else { return fallback };

    let first = s
        .trim()
        .split_terminator(['.', '!', '?', '\n'])
        .next()
        .unwrap_or("")
        .trim();
    if first.is_empty() {
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

/// FNV-1a, hex. A stable, dependency-free digest for `chapters.prompt_hash`, whose job is
/// provenance (§9.3: "tell drift from your own edit"), not security.
pub fn stable_hash(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The lowest-numbered chapter that has prose but no audio.
///
/// Scans every chapter row: there is no `chapters_missing_audio()` query in the store yet,
/// and at one scan per multi-minute cycle the cost is irrelevant next to a GPU pass.
fn unrendered_chapter(store: &Store) -> litrpg_store::Result<Option<u32>> {
    Ok(store
        .chapters_since(0)?
        .into_iter()
        .find(|c| !c.has_audio)
        .map(|c| c.number))
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
    fn stable_hash_is_stable_and_sensitive() {
        assert_eq!(stable_hash("abc"), stable_hash("abc"));
        assert_ne!(stable_hash("abc"), stable_hash("abd"));
        assert_eq!(stable_hash("abc").len(), 16);
    }

    #[test]
    fn derive_title_uses_the_first_sentence_of_the_summary() {
        assert_eq!(
            derive_title(4, Some("Kaelen broke the first seal. Sera watched.")),
            "Kaelen broke the first seal"
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
