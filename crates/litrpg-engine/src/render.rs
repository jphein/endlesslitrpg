//! Segments → one PCM stream + the manifest.
//!
//! # Why timings are measured, never predicted
//!
//! The manifest's byte offsets are what every client — watch, Candela, the daemon's
//! `Range` handler — uses to seek. They are valid only because
//! `duration_ms × 32 == len` holds *exactly*, on the final bytes.
//!
//! So the manifest is built from the **measured length of each rendered buffer**, after
//! post-processing. Predicting from text length or from a pre-`loudnorm` duration would
//! be wrong: `loudnorm` changes stream length, and a manifest that disagrees with the
//! audio by even one sample makes every subsequent segment's `Range` request land in the
//! middle of a word — with nothing failing, anywhere, to say so.
//!
//! Each buffer is padded to a whole millisecond *before* its offsets are taken
//! ([`Pcm16k::padded_to_whole_ms`]), because 16 kHz × 2 bytes gives 32 bytes/ms and a
//! buffer ending mid-millisecond would push every later offset off by a fraction that
//! accumulates.

use litrpg_core::{BYTES_PER_MS, Manifest, Segment, SpeakerKind};
use litrpg_tts::{Pcm16k, RenderRequest, TtsError};

use crate::error::EngineError;

/// A segment with its voice assigned but no audio yet.
///
/// The stage between [`litrpg_ember::ParsedSegment`] (no voice) and
/// [`litrpg_core::Segment`] (voice *and* timings). Keeping it distinct is what stops a
/// zeroed `start_ms` being mistaken for a real offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSegment {
    pub idx: u32,
    pub speaker: String,
    pub kind: SpeakerKind,
    pub voice_ref: String,
    pub text: String,
}

impl PlannedSegment {
    pub fn to_request(&self) -> Result<RenderRequest, TtsError> {
        RenderRequest::parse(self.idx, &self.voice_ref, self.text.clone(), self.kind)
    }
}

/// A rendered chapter: one continuous stream plus the manifest that addresses it.
#[derive(Debug, Clone)]
pub struct RenderedChapter {
    pub pcm: Pcm16k,
    pub manifest: Manifest,
}

/// Target length for one manifest entry, in characters.
///
/// Passed to [`litrpg_tts::azure::split_for_requests`], whose contract is "split preferring
/// sentence boundaries, capped at this many characters". ~200 chars is a long sentence, so in
/// practice each entry is one sentence.
pub const SENTENCE_TARGET_CHARS: usize = 200;

/// Split each speaker turn into per-sentence segments, for §9.4 sentence highlighting.
///
/// # Why this is the whole feature
///
/// The renderer synthesises one TTS call per [`PlannedSegment`], and [`assemble`] derives each
/// entry's timings from the **measured** length of the buffer that came back. So splitting the
/// planned segments finer is sufficient on its own: nothing downstream changes, and contiguity
/// and `duration_ms × 32 == len` continue to hold *by construction* rather than by a second
/// implementation that has to agree with the first.
///
/// Measured before this existed: live chapter 1 was 7 segments, mean 64.7 s, longest `text`
/// 3 665 chars. A highlight sitting still for three minutes on prose that cannot fit the panel
/// reads as broken, which is worse than no highlighting at all.
///
/// # What it guarantees
///
/// * Speaker, kind and voice are **inherited** from the turn, so the cast is untouched and one
///   turn never becomes two voices.
/// * `idx` is re-numbered densely from zero, so the manifest can still be built straight from it.
/// * **No text is dropped.** A splitter returning nothing for a non-empty turn keeps the turn
///   whole rather than losing a sentence — the same rule as the tagged-prose parser.
/// * Idempotent: re-splitting already-split segments returns them unchanged, which is what makes
///   the resume path safe.
pub fn split_by_sentence(
    planned: Vec<PlannedSegment>,
    split: impl Fn(&str) -> Vec<String>,
) -> Vec<PlannedSegment> {
    let mut out: Vec<PlannedSegment> = Vec::with_capacity(planned.len());

    for turn in planned {
        let pieces: Vec<String> = split(&turn.text)
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();

        if pieces.is_empty() {
            // Either the turn was blank, or the splitter misbehaved. Keeping it is the
            // conservative choice; dropping it would silently lose a line of the chapter.
            out.push(PlannedSegment {
                idx: out.len() as u32,
                ..turn
            });
            continue;
        }

        for piece in pieces {
            out.push(PlannedSegment {
                idx: out.len() as u32,
                speaker: turn.speaker.clone(),
                kind: turn.kind,
                voice_ref: turn.voice_ref.clone(),
                text: piece,
            });
        }
    }

    out
}

/// The production splitter: aurora's sentence-preferring splitter from `litrpg-tts`.
///
/// Reused rather than reimplemented. Two documented divergences from true sentence splitting,
/// both consequences of its being budget-driven:
///
/// * A turn already shorter than [`SENTENCE_TARGET_CHARS`] is returned whole. Harmless here —
///   such a turn is a few seconds of audio, which is already fine highlighting granularity.
/// * A single sentence *longer* than the target is broken on a word boundary. That is a real
///   mid-clause seam, both audibly and for a highlight. Rare, and preferable to the alternative
///   of an unbounded request.
pub fn sentence_pieces(text: &str) -> Vec<String> {
    litrpg_tts::azure::split_for_requests(text, SENTENCE_TARGET_CHARS)
}

/// Build render requests for a whole chapter, failing before any synthesis happens if a
/// `voice_ref` is malformed (§7.3: fail at assignment time, not at render time).
pub fn plan_requests(planned: &[PlannedSegment]) -> Result<Vec<RenderRequest>, EngineError> {
    planned
        .iter()
        .map(|p| p.to_request().map_err(EngineError::Tts))
        .collect()
}

/// Stitch rendered buffers into one stream and derive the manifest from their measured
/// lengths.
///
/// Returns [`EngineError::Artifact`] if the parts do not correspond 1:1 with the planned
/// segments, or if the assembled stream violates `duration_ms × 32 == len`. The latter is
/// a belt-and-braces check on the whole point of the format: if it ever fires, publishing
/// the chapter would ship a manifest that lies.
pub fn assemble(
    chapter: u32,
    planned: &[PlannedSegment],
    parts: Vec<Pcm16k>,
) -> Result<RenderedChapter, EngineError> {
    if parts.len() != planned.len() {
        return Err(EngineError::Artifact {
            detail: format!(
                "renderer returned {} buffers for {} segments",
                parts.len(),
                planned.len()
            ),
        });
    }

    let padded: Vec<Pcm16k> = parts.into_iter().map(Pcm16k::padded_to_whole_ms).collect();

    let mut segments = Vec::with_capacity(planned.len());
    let mut cursor_ms: u32 = 0;

    for (p, pcm) in planned.iter().zip(&padded) {
        let dur = pcm.duration_ms();
        segments.push(Segment {
            idx: p.idx,
            speaker: p.speaker.clone(),
            kind: p.kind,
            voice_ref: p.voice_ref.clone(),
            text: p.text.clone(),
            start_ms: cursor_ms,
            end_ms: cursor_ms + dur,
        });
        cursor_ms += dur;
    }

    let pcm = Pcm16k::concat(&padded);
    let manifest = Manifest::new(chapter, segments);

    // The invariant the watch's whole playback path rests on.
    let expected = manifest.duration_ms as u64 * BYTES_PER_MS as u64;
    if pcm.len() as u64 != expected {
        return Err(EngineError::Artifact {
            detail: format!(
                "manifest/audio mismatch: duration_ms {} implies {expected} bytes but the \
                 stream is {} bytes",
                manifest.duration_ms,
                pcm.len()
            ),
        });
    }

    if !manifest.is_contiguous() {
        return Err(EngineError::Artifact {
            detail: "assembled manifest is not contiguous".to_string(),
        });
    }

    Ok(RenderedChapter { pcm, manifest })
}

/// Render the chapter markdown that ships as `NNNN.md` (§8).
///
/// Speaker tags are kept: they are how a reader — and Candela's highlighter — tells
/// dialogue from narration from a stat block, and the file is the canonical permanent
/// artifact, so throwing that away would be lossy.
pub fn chapter_markdown(number: u32, title: &str, planned: &[PlannedSegment]) -> String {
    let mut out = format!("# Chapter {number}: {title}\n\n");
    for p in planned {
        out.push_str(&format!("[{}] {}\n\n", p.speaker, p.text));
    }
    out
}

/// Word count of the chapter body, for `/api/chapters`.
pub fn word_count(planned: &[PlannedSegment]) -> usize {
    planned
        .iter()
        .map(|p| p.text.split_whitespace().count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(n: u32) -> Vec<PlannedSegment> {
        (0..n)
            .map(|i| PlannedSegment {
                idx: i,
                speaker: format!("S{i}"),
                kind: SpeakerKind::Character,
                voice_ref: "sherpa:kokoro-multi-lang-v1_0:1".to_string(),
                text: format!("line {i}"),
            })
            .collect()
    }

    #[test]
    fn offsets_come_from_measured_lengths_and_are_contiguous() {
        let p = planned(3);
        let parts = vec![
            Pcm16k::silence_ms(100),
            Pcm16k::silence_ms(250),
            Pcm16k::silence_ms(40),
        ];
        let r = assemble(7, &p, parts).unwrap();

        assert_eq!(r.manifest.segments[0].start_ms, 0);
        assert_eq!(r.manifest.segments[0].end_ms, 100);
        assert_eq!(r.manifest.segments[1].start_ms, 100);
        assert_eq!(r.manifest.segments[1].end_ms, 350);
        assert_eq!(r.manifest.segments[2].end_ms, 390);
        assert_eq!(r.manifest.duration_ms, 390);
        assert!(r.manifest.is_contiguous());
        assert_eq!(r.pcm.len() as u32, 390 * BYTES_PER_MS);
    }

    #[test]
    fn byte_offsets_derive_exactly_from_millisecond_offsets() {
        let p = planned(2);
        let parts = vec![Pcm16k::silence_ms(31), Pcm16k::silence_ms(17)];
        let r = assemble(1, &p, parts).unwrap();
        for s in &r.manifest.segments {
            assert_eq!(s.start_byte(), s.start_ms as u64 * 32);
            assert_eq!(s.end_byte(), s.end_ms as u64 * 32);
        }
        assert_eq!(r.manifest.total_bytes(), r.pcm.len() as u64);
    }

    #[test]
    fn a_sub_millisecond_buffer_is_padded_so_offsets_cannot_drift() {
        // 33 bytes is one millisecond plus one sample: without padding, the next
        // segment's start_ms would round and every later offset would be wrong.
        let odd = Pcm16k::from_slice(&[0u8; 34]).unwrap();
        assert!(!odd.is_whole_ms());

        let p = planned(2);
        let r = assemble(1, &p, vec![odd, Pcm16k::silence_ms(10)]).unwrap();
        assert_eq!(r.pcm.len() as u32, r.manifest.duration_ms * BYTES_PER_MS);
        assert!(r.manifest.is_contiguous());
    }

    #[test]
    fn a_count_mismatch_is_an_error_not_a_silently_short_chapter() {
        let err = assemble(1, &planned(3), vec![Pcm16k::silence_ms(10)]).unwrap_err();
        assert!(format!("{err}").contains("1 buffers for 3 segments"));
    }

    #[test]
    fn an_empty_chapter_assembles_to_an_empty_manifest() {
        let r = assemble(1, &[], vec![]).unwrap();
        assert_eq!(r.manifest.duration_ms, 0);
        assert!(r.pcm.is_empty());
        assert!(r.manifest.is_contiguous());
    }

    #[test]
    fn markdown_keeps_the_speaker_tags() {
        let md = chapter_markdown(4, "The Ashen Ledger", &planned(2));
        assert!(md.starts_with("# Chapter 4: The Ashen Ledger"));
        assert!(md.contains("[S0] line 0"));
        assert!(md.contains("[S1] line 1"));
    }

    #[test]
    fn word_count_sums_segment_bodies() {
        assert_eq!(word_count(&planned(3)), 6);
    }

    // -----------------------------------------------------------------------
    // Per-sentence manifest entries (§9.4, issue #10)
    // -----------------------------------------------------------------------

    /// A deterministic splitter, so the plumbing is tested independently of the splitter's
    /// heuristics. The real one is covered by `sentence_pieces_*` below.
    fn on_pipe(text: &str) -> Vec<String> {
        text.split('|').map(|s| s.trim().to_string()).collect()
    }

    fn turn(idx: u32, speaker: &str, kind: SpeakerKind, voice: &str, text: &str) -> PlannedSegment {
        PlannedSegment {
            idx,
            speaker: speaker.to_string(),
            kind,
            voice_ref: voice.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn splitting_inherits_speaker_kind_and_voice() {
        let out = split_by_sentence(
            vec![turn(
                0,
                "Kaelen",
                SpeakerKind::Character,
                "azure:m1",
                "One.|Two.|Three.",
            )],
            on_pipe,
        );
        assert_eq!(out.len(), 3);
        for (i, s) in out.iter().enumerate() {
            assert_eq!(s.idx, i as u32, "indices must be dense and zero-based");
            assert_eq!(
                s.speaker, "Kaelen",
                "one turn must never become two speakers"
            );
            assert_eq!(s.kind, SpeakerKind::Character);
            assert_eq!(
                s.voice_ref, "azure:m1",
                "one turn must never become two voices"
            );
        }
        assert_eq!(
            out.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec!["One.", "Two.", "Three."]
        );
    }

    #[test]
    fn indices_stay_dense_across_several_turns() {
        let out = split_by_sentence(
            vec![
                turn(0, "narrator", SpeakerKind::Narrator, "v0", "A.|B."),
                turn(1, "Kaelen", SpeakerKind::Character, "v1", "C."),
                turn(2, "SYSTEM", SpeakerKind::System, "v2", "D.|E.|F."),
            ],
            on_pipe,
        );
        assert_eq!(out.len(), 6);
        assert_eq!(
            out.iter().map(|s| s.idx).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert_eq!(out[2].speaker, "Kaelen");
        assert_eq!(out[3].kind, SpeakerKind::System);
    }

    #[test]
    fn no_text_is_dropped_by_splitting() {
        let turns = vec![
            turn(
                0,
                "narrator",
                SpeakerKind::Narrator,
                "v",
                "The vale.|Ash fell.",
            ),
            turn(1, "Kaelen", SpeakerKind::Character, "v", "\"Pay up.\""),
        ];
        let joined_before: String = turns
            .iter()
            .flat_map(|t| t.text.split('|'))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ");

        let out = split_by_sentence(turns, on_pipe);
        let joined_after = out
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined_after, joined_before);
    }

    #[test]
    fn a_splitter_returning_nothing_keeps_the_turn_whole() {
        // Losing a line of the chapter to a splitter bug would be silent, so the conservative
        // branch has to be the one that keeps text.
        let out = split_by_sentence(
            vec![turn(
                0,
                "Kaelen",
                SpeakerKind::Character,
                "v",
                "Something was said.",
            )],
            |_| Vec::new(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Something was said.");
    }

    #[test]
    fn blank_pieces_are_discarded() {
        let out = split_by_sentence(
            vec![turn(
                0,
                "Kaelen",
                SpeakerKind::Character,
                "v",
                "One.||  |Two.",
            )],
            on_pipe,
        );
        assert_eq!(
            out.len(),
            2,
            "empty pieces would render as silence: {out:?}"
        );
    }

    #[test]
    fn splitting_is_idempotent_so_a_resume_is_safe() {
        let once = split_by_sentence(
            vec![turn(0, "narrator", SpeakerKind::Narrator, "v", "A.|B.|C.")],
            on_pipe,
        );
        let twice = split_by_sentence(once.clone(), on_pipe);
        assert_eq!(
            once, twice,
            "re-splitting stored segments must change nothing"
        );
    }

    #[test]
    fn an_empty_chapter_splits_to_nothing() {
        assert!(split_by_sentence(Vec::new(), on_pipe).is_empty());
    }

    /// The measured problem, end to end: a long narration turn becomes many entries, and the
    /// assembled manifest is still contiguous with exact byte arithmetic.
    #[test]
    fn a_split_chapter_still_satisfies_the_manifest_invariant() {
        let long = (0..12)
            .map(|i| format!("Sentence number {i}."))
            .collect::<Vec<_>>()
            .join(" ");
        let turns = vec![turn(0, "narrator", SpeakerKind::Narrator, "v", &long)];

        let split = split_by_sentence(turns, sentence_pieces);
        assert!(
            split.len() > 1,
            "a 12-sentence turn must not stay one entry"
        );

        let parts: Vec<Pcm16k> = split
            .iter()
            .enumerate()
            .map(|(i, _)| Pcm16k::silence_ms(40 + i as u32))
            .collect();
        let r = assemble(1, &split, parts).unwrap();

        assert!(r.manifest.is_contiguous());
        assert_eq!(r.pcm.len() as u32, r.manifest.duration_ms * BYTES_PER_MS);
        assert_eq!(r.manifest.segments.len(), split.len());
        // Every entry is short enough to highlight usefully.
        for s in &r.manifest.segments {
            assert!(
                s.duration_ms() < 1_000,
                "entry {} is too long to highlight",
                s.idx
            );
        }
    }

    // The real splitter's behaviour on the shapes prose actually contains.
    #[test]
    fn sentence_pieces_does_not_shatter_dialogue() {
        let text = "\"Pay up,\" he said. \"Or I take the ledger.\" Kaelen did not move.";
        let pieces = sentence_pieces(&text.repeat(4));
        assert!(
            pieces.iter().all(|p| !p.trim().is_empty()),
            "no empty pieces: {pieces:?}"
        );
        // The comma inside the quotation must not end a sentence.
        assert!(
            !pieces.iter().any(|p| p.trim() == "\"Pay up,\""),
            "dialogue was shattered at the comma: {pieces:?}"
        );
    }

    #[test]
    fn sentence_pieces_keeps_a_decimal_intact() {
        // "4.200" must not split, which is why the splitter requires whitespace after a
        // terminator.
        let text = "He owed 4.200 marks and a debt of honour. ".repeat(12);
        let pieces = sentence_pieces(&text);
        assert!(
            !pieces.iter().any(|p| p.trim().ends_with("4.")),
            "a decimal was split: {pieces:?}"
        );
    }

    #[test]
    fn sentence_pieces_never_loses_characters() {
        let text = "One. Two! Three? \"Four.\" Five... Six. ".repeat(10);
        let pieces = sentence_pieces(&text);
        let rejoined: String = pieces
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let original: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined, original, "the splitter must be lossless");
    }

    #[test]
    fn a_short_turn_is_left_whole() {
        // Documented divergence from true sentence splitting: harmless, because a turn under the
        // target is a few seconds of audio and already fine to highlight.
        let pieces = sentence_pieces("Short. Two sentences.");
        assert_eq!(pieces, vec!["Short. Two sentences."]);
    }

    #[test]
    fn a_malformed_voice_ref_fails_before_any_synthesis() {
        let bad = vec![PlannedSegment {
            idx: 0,
            speaker: "X".into(),
            kind: SpeakerKind::Character,
            voice_ref: "no-colon-here".into(),
            text: "hi".into(),
        }];
        assert!(plan_requests(&bad).is_err());
    }
}
