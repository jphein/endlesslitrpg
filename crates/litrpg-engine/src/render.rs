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
