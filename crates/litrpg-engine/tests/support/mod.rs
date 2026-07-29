//! Fakes for the four engine ports, so the whole cycle runs with no network, no GPU and
//! no ffmpeg.

use std::collections::VecDeque;
use std::sync::Mutex;

use litrpg_core::Manifest;
use litrpg_ember::prompt::{ChapterSummary, LoreEntry, pass1_messages};
use litrpg_ember::{EmberError, Extraction, Pass1Input, ProposedDelta, ProposedLore, QuestUpdate};
use litrpg_engine::{Artifacts, EngineError, Generator, Library, Renderer, StoryMeta};
use litrpg_store::Store;
use litrpg_tts::{Pcm16k, RenderRequest, TtsError, async_trait};

// ---------------------------------------------------------------------------
// Generator
// ---------------------------------------------------------------------------

/// Scripted generator. Each queue is popped per call; when a queue runs dry it falls back
/// to its default, so a test only has to script the calls it cares about.
pub struct FakeGenerator {
    pass1_queue: Mutex<VecDeque<Result<String, EmberError>>>,
    pass2_queue: Mutex<VecDeque<Result<Extraction, EmberError>>>,
    pass1_default: String,
    pass2_default: Mutex<Option<Extraction>>,
    /// (rendered prompt, temperature) for every pass-1 call.
    pub pass1_calls: Mutex<Vec<(String, f64)>>,
    /// (chapter text, known subjects) for every pass-2 call.
    pub pass2_calls: Mutex<Vec<(String, Vec<String>)>>,
}

pub const DEFAULT_PROSE: &str = "\
[narrator] The vale smelled of iron and wet ash.

[Kaelen] \"You brought a sword to a debt collection?\"

[SYSTEM] Quest updated — The Ashen Ledger: 1 of 3 seals broken.";

impl FakeGenerator {
    pub fn new() -> Self {
        Self {
            pass1_queue: Mutex::new(VecDeque::new()),
            pass2_queue: Mutex::new(VecDeque::new()),
            pass1_default: DEFAULT_PROSE.to_string(),
            pass2_default: Mutex::new(Some(extraction_with(vec![], vec![]))),
            pass1_calls: Mutex::new(Vec::new()),
            pass2_calls: Mutex::new(Vec::new()),
        }
    }

    pub fn with_prose(mut self, prose: &str) -> Self {
        self.pass1_default = prose.to_string();
        self
    }

    pub fn push_pass1(self, r: Result<String, EmberError>) -> Self {
        self.pass1_queue.lock().unwrap().push_back(r);
        self
    }

    pub fn push_pass2(self, r: Result<Extraction, EmberError>) -> Self {
        self.pass2_queue.lock().unwrap().push_back(r);
        self
    }

    pub fn with_extraction(self, e: Extraction) -> Self {
        *self.pass2_default.lock().unwrap() = Some(e);
        self
    }

    /// Make pass 2 always fail with a malformed-output error.
    pub fn with_pass2_always_malformed(self) -> Self {
        *self.pass2_default.lock().unwrap() = None;
        self
    }

    pub fn pass1_count(&self) -> usize {
        self.pass1_calls.lock().unwrap().len()
    }

    pub fn pass2_count(&self) -> usize {
        self.pass2_calls.lock().unwrap().len()
    }

    /// Every pass-1 prompt, concatenated.
    pub fn all_pass1_prompts(&self) -> String {
        self.pass1_calls
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn pass1_temperatures(&self) -> Vec<f64> {
        self.pass1_calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, t)| *t)
            .collect()
    }
}

impl Default for FakeGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Generator for FakeGenerator {
    async fn pass1(&self, input: &Pass1Input<'_>, temperature: f64) -> Result<String, EmberError> {
        let rendered = pass1_messages(input)
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.pass1_calls
            .lock()
            .unwrap()
            .push((rendered, temperature));

        match self.pass1_queue.lock().unwrap().pop_front() {
            Some(r) => r,
            None => Ok(self.pass1_default.clone()),
        }
    }

    async fn pass2(
        &self,
        chapter_text: &str,
        known_subjects: &[String],
    ) -> Result<Extraction, EmberError> {
        self.pass2_calls
            .lock()
            .unwrap()
            .push((chapter_text.to_string(), known_subjects.to_vec()));

        if let Some(r) = self.pass2_queue.lock().unwrap().pop_front() {
            return r;
        }
        match self.pass2_default.lock().unwrap().clone() {
            Some(e) => Ok(e),
            None => Err(EmberError::Malformed {
                body: "not json".to_string(),
                detail: "scripted failure".to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

pub struct FakeRenderer {
    fail: bool,
    /// Milliseconds of silence returned per segment.
    ms_per_segment: u32,
    /// Return one fewer buffer than requested, to exercise the mismatch guard.
    short_by_one: bool,
    pub calls: Mutex<Vec<Vec<RenderRequest>>>,
}

impl FakeRenderer {
    pub fn new() -> Self {
        Self {
            fail: false,
            ms_per_segment: 100,
            short_by_one: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn failing() -> Self {
        Self {
            fail: true,
            ..Self::new()
        }
    }

    pub fn short_by_one() -> Self {
        Self {
            short_by_one: true,
            ..Self::new()
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// The voice_refs of the most recent render call, in order.
    pub fn last_voices(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .last()
            .map(|reqs| reqs.iter().map(|r| r.voice.to_string()).collect())
            .unwrap_or_default()
    }
}

impl Default for FakeRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Renderer for FakeRenderer {
    async fn render_all(&self, reqs: &[RenderRequest]) -> Result<Vec<Pcm16k>, TtsError> {
        self.calls.lock().unwrap().push(reqs.to_vec());

        if self.fail {
            return Err(TtsError::Synthesis("scripted render failure".to_string()));
        }

        let n = if self.short_by_one {
            reqs.len().saturating_sub(1)
        } else {
            reqs.len()
        };
        // Vary the length per segment so a test asserting offsets cannot pass by
        // accident on uniform buffers.
        Ok((0..n)
            .map(|i| Pcm16k::silence_ms(self.ms_per_segment + i as u32))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

pub struct FakeLibrary {
    pub story: StoryMeta,
    pub lore: Mutex<Vec<LoreEntry>>,
    pub summaries: Mutex<Vec<ChapterSummary>>,
    pub puts: Mutex<Vec<(u32, String)>>,
}

impl FakeLibrary {
    pub fn new() -> Self {
        Self {
            story: StoryMeta {
                title: "The Ashen Ledger".to_string(),
                protagonist: "Kaelen".to_string(),
                prompt_md: "Kaelen is a debt-collector for a dead god.".to_string(),
                arc_outline_md: "Arc 1: break the three seals.".to_string(),
                target_words: 2000,
            },
            lore: Mutex::new(Vec::new()),
            summaries: Mutex::new(Vec::new()),
            puts: Mutex::new(Vec::new()),
        }
    }

    pub fn with_lore(self, entries: Vec<LoreEntry>) -> Self {
        *self.lore.lock().unwrap() = entries;
        self
    }

    pub fn with_summaries(self, s: Vec<ChapterSummary>) -> Self {
        *self.summaries.lock().unwrap() = s;
        self
    }

    pub fn summaries_written(&self) -> Vec<(u32, String)> {
        self.puts.lock().unwrap().clone()
    }
}

impl Default for FakeLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl Library for FakeLibrary {
    fn story(&self) -> Result<StoryMeta, EngineError> {
        Ok(self.story.clone())
    }

    fn lore(&self) -> Result<Vec<LoreEntry>, EngineError> {
        Ok(self.lore.lock().unwrap().clone())
    }

    fn recent_summaries(&self, limit: usize) -> Result<Vec<ChapterSummary>, EngineError> {
        let all = self.summaries.lock().unwrap().clone();
        let start = all.len().saturating_sub(limit);
        Ok(all[start..].to_vec())
    }

    fn put_summary(&self, chapter: u32, body_md: &str) -> Result<(), EngineError> {
        self.puts
            .lock()
            .unwrap()
            .push((chapter, body_md.to_string()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

pub struct FakeArtifacts {
    fail_on: Option<&'static str>,
    pub written: Mutex<Vec<(String, u32)>>,
}

impl FakeArtifacts {
    pub fn new() -> Self {
        Self {
            fail_on: None,
            written: Mutex::new(Vec::new()),
        }
    }

    /// Fail on one artifact kind: `"text"`, `"pcm"`, `"manifest"` or `"mp3"`.
    pub fn failing_on(kind: &'static str) -> Self {
        Self {
            fail_on: Some(kind),
            written: Mutex::new(Vec::new()),
        }
    }

    pub fn kinds(&self) -> Vec<String> {
        self.written
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn record(&self, kind: &'static str, chapter: u32) -> Result<String, EngineError> {
        if self.fail_on == Some(kind) {
            return Err(EngineError::Artifact {
                detail: format!("scripted {kind} failure"),
            });
        }
        self.written
            .lock()
            .unwrap()
            .push((kind.to_string(), chapter));
        Ok(format!("/fake/{chapter:04}.{kind}"))
    }
}

impl Default for FakeArtifacts {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Artifacts for FakeArtifacts {
    async fn write_text(&self, chapter: u32, _text_md: &str) -> Result<String, EngineError> {
        self.record("text", chapter)
    }

    async fn write_pcm(&self, chapter: u32, _pcm: &Pcm16k) -> Result<String, EngineError> {
        self.record("pcm", chapter)
    }

    async fn write_manifest(
        &self,
        chapter: u32,
        _manifest: &Manifest,
    ) -> Result<String, EngineError> {
        self.record("manifest", chapter)
    }

    async fn encode_mp3(&self, chapter: u32, _pcm_path: &str) -> Result<String, EngineError> {
        self.record("mp3", chapter)
    }
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

pub fn store() -> Store {
    let s = Store::open_in_memory().expect("in-memory store");
    s.migrate().expect("migrate");
    s
}

pub fn delta(subject: &str, field: &str, op: &str, num: Option<i64>) -> ProposedDelta {
    ProposedDelta {
        subject: subject.to_string(),
        field: field.to_string(),
        op: op.to_string(),
        value_num: num,
        value_txt: None,
    }
}

/// A text-valued delta.
pub fn delta_txt(subject: &str, field: &str, txt: &str) -> ProposedDelta {
    ProposedDelta {
        subject: subject.to_string(),
        field: field.to_string(),
        op: "set".to_string(),
        value_num: None,
        value_txt: Some(txt.to_string()),
    }
}

pub fn lore_row(name: &str, kind: &str, keywords: &str) -> ProposedLore {
    ProposedLore {
        name: name.to_string(),
        kind: kind.to_string(),
        keywords: keywords.to_string(),
        body_md: format!("About {name}."),
        priority: 0,
        gender: None,
    }
}

/// A lore row carrying a gender hint, for gender-matched casting.
pub fn gendered_lore(name: &str, gender: &str) -> ProposedLore {
    ProposedLore {
        gender: Some(gender.to_string()),
        ..lore_row(name, "character", &name.to_lowercase())
    }
}

pub fn extraction_with(deltas: Vec<ProposedDelta>, new_lore: Vec<ProposedLore>) -> Extraction {
    Extraction {
        title: "The First Seal".to_string(),
        summary: "Kaelen broke the first seal.".to_string(),
        deltas,
        new_lore,
        quest_updates: vec![QuestUpdate {
            name: "The Ashen Ledger".to_string(),
            status: "advanced".to_string(),
            detail: None,
        }],
    }
}

pub fn lore_entry(name: &str, keywords: &str, always_on: bool) -> LoreEntry {
    LoreEntry {
        name: name.to_string(),
        kind: "place".to_string(),
        keywords: keywords.to_string(),
        body_md: format!("Body of {name}."),
        priority: 0,
        always_on,
    }
}

// ---------------------------------------------------------------------------
// Test-only sugar over the engine's store, so assertions read as intent.
// ---------------------------------------------------------------------------

pub type FakeEngine =
    litrpg_engine::Engine<FakeGenerator, FakeRenderer, FakeLibrary, FakeArtifacts>;

pub trait EngineTestExt {
    fn latest_number(&self) -> u32;
    fn chapter_text(&self, n: u32) -> String;
    fn prompt_hash(&self, n: u32) -> String;
    fn has_audio(&self, n: u32) -> bool;
    fn duration_ms(&self, n: u32) -> u32;
    fn dirty_chapters(&self) -> Vec<u32>;
    fn segments(&self, n: u32) -> Vec<litrpg_core::Segment>;
    fn segment_count(&self, n: u32) -> usize;
    fn cast_pairs(&self) -> Vec<(String, String)>;
    fn snapshot_num(&self, subject: &str, field: &str) -> Option<i64>;
    fn rejected_count(&self) -> i64;
    fn insert_note(&self, body: &str, source: &str);
    fn pending_note_count(&self) -> usize;
    fn summaries_written(&self) -> Vec<(u32, String)>;
    fn artifacts_kinds(&self) -> Vec<String>;
}

impl EngineTestExt for FakeEngine {
    fn latest_number(&self) -> u32 {
        self.with_store(|s| s.latest_number()).unwrap()
    }

    fn chapter_text(&self, n: u32) -> String {
        self.with_store(|s| s.chapter(n)).unwrap().text_md
    }

    fn prompt_hash(&self, n: u32) -> String {
        self.with_store(|s| s.chapter(n)).unwrap().prompt_hash
    }

    fn has_audio(&self, n: u32) -> bool {
        self.with_store(|s| s.chapter(n)).unwrap().has_audio
    }

    fn duration_ms(&self, n: u32) -> u32 {
        self.with_store(|s| s.chapter(n)).unwrap().duration_ms
    }

    fn dirty_chapters(&self) -> Vec<u32> {
        self.with_store(|s| s.dirty_chapters()).unwrap()
    }

    fn segments(&self, n: u32) -> Vec<litrpg_core::Segment> {
        self.with_store(|s| s.segments(n)).unwrap()
    }

    fn segment_count(&self, n: u32) -> usize {
        self.segments(n).len()
    }

    fn cast_pairs(&self) -> Vec<(String, String)> {
        self.with_store(|s| s.cast())
            .unwrap()
            .into_iter()
            .map(|c| (c.speaker, c.voice_ref))
            .collect()
    }

    fn snapshot_num(&self, subject: &str, field: &str) -> Option<i64> {
        self.with_store(|s| s.snapshot())
            .unwrap()
            .num(subject, field)
    }

    fn rejected_count(&self) -> i64 {
        self.with_store(|s| s.rejected_count()).unwrap()
    }

    fn insert_note(&self, body: &str, source: &str) {
        self.with_store(|s| s.insert_note(body, source)).unwrap();
    }

    fn pending_note_count(&self) -> usize {
        self.with_store(|s| s.pending_notes()).unwrap().len()
    }

    fn summaries_written(&self) -> Vec<(u32, String)> {
        self.library().summaries_written()
    }

    fn artifacts_kinds(&self) -> Vec<String> {
        self.artifacts().kinds()
    }
}
