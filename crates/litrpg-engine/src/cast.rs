//! Voice assignment (spec §5.1 step 4, §7.3).
//!
//! Voices are drawn on a character's **first appearance** and persisted, which is what
//! makes a cast feel like continuity rather than a lottery. Because it is persisted, the
//! drawing must be a pure function of `(existing cast, order of first appearance)` — no
//! randomness, no hash-iteration order, no wall-clock. A shuffling cast is only
//! *audible*, so the bug would surface forty chapters in, with forty chapters of audio
//! already published in the wrong voices.
//!
//! # The pool
//!
//! Characters draw from Kokoro's English speakers, whose sid map (§4.4) is
//! `0–10` Am-F, `11–19` Am-M, `20–23` Br-F, `24–27` Br-M. The pool **interleaves the
//! four groups round-robin** rather than walking them in order, so a growing cast
//! alternates gender and accent instead of exhausting eleven American women before it
//! reaches its first man. A four-person cast spans all four groups.
//!
//! The narrator uses `config.narrator_voice` (D7: Piper `cori`) and `SYSTEM` uses one
//! reserved voice; **both are excluded from the character pool** so a person can never
//! draw the robot's voice or the narrator's.

use litrpg_core::SpeakerKind;

/// A distinct speaker seen in a chapter, in order of first appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSpeaker {
    pub speaker: String,
    pub kind: SpeakerKind,
}

/// One new `cast` row to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastAssignment {
    pub speaker: String,
    pub kind: SpeakerKind,
    pub voice_ref: String,
}

/// The Kokoro multi-speaker model, per spec §7.3.
pub const KOKORO_MODEL: &str = "sherpa:kokoro-multi-lang-v1_0";

/// Reserved for `SYSTEM`. A neutral speaker; the robotic character comes from the
/// post-render ffmpeg pass in §7.4, not from the voice.
pub const SYSTEM_VOICE: &str = "sherpa:kokoro-multi-lang-v1_0:24";

/// Used when no narrator voice is configured. D7 names Piper `cori`; §4.4's Assumption
/// A1 keeps `bf_emma` (21) as the en-GB fallback if `cori` cannot be sourced, and voices
/// are config rather than code precisely so this choice is cheap to change.
pub const NARRATOR_FALLBACK_VOICE: &str = "sherpa:kokoro-multi-lang-v1_0:21";

/// `sid` ranges from spec §4.4, in the order they are interleaved.
const SID_GROUPS: [&[u32]; 4] = [
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],   // Am-F
    &[11, 12, 13, 14, 15, 16, 17, 18, 19], // Am-M
    &[20, 21, 22, 23],                     // Br-F
    &[24, 25, 26, 27],                     // Br-M
];

/// Size of the character pool: all 28 English sids, minus the two reserved voices.
pub const CHARACTER_POOL_LEN: usize = 26;

/// Build a `voice_ref` for a Kokoro speaker id.
pub fn kokoro_voice_ref(sid: u32) -> String {
    format!("{KOKORO_MODEL}:{sid}")
}

/// The character voice pool: the four sid groups interleaved round-robin, with the
/// narrator's and `SYSTEM`'s reserved voices removed.
///
/// Deterministic and allocation-cheap; recomputed rather than cached because it is
/// called once per cycle and a `static` would need a lock or a lazy cell for no gain.
pub fn character_pool() -> Vec<String> {
    // Reserved voices are removed from each group *before* interleaving, not after.
    // Filtering afterwards would leave a hole in the round-robin, and the first four
    // voices — the ones a small cast actually gets — would no longer span all four
    // gender/accent groups.
    let groups: Vec<Vec<String>> = SID_GROUPS
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|sid| kokoro_voice_ref(*sid))
                .filter(|v| v != SYSTEM_VOICE && v != NARRATOR_FALLBACK_VOICE)
                .collect()
        })
        .collect();

    let longest = groups.iter().map(Vec::len).max().unwrap_or(0);
    let mut pool = Vec::with_capacity(CHARACTER_POOL_LEN);
    for i in 0..longest {
        for group in &groups {
            if let Some(v) = group.get(i) {
                pool.push(v.clone());
            }
        }
    }

    pool
}

/// Draws voices for speakers that are not yet in the cast.
#[derive(Debug, Clone)]
pub struct VoiceAssigner {
    narrator_voice: String,
    system_voice: String,
    pool: Vec<String>,
}

impl VoiceAssigner {
    /// The sherpa/Kokoro defaults.
    pub fn new(narrator_voice: String) -> Self {
        Self::with_voices(narrator_voice, SYSTEM_VOICE.to_string(), character_pool())
    }

    /// Explicit voices, for a cast that is not sherpa-backed.
    ///
    /// This exists because a `voice_ref` names its backend (§7.3), so an Azure-only
    /// deployment cannot use the Kokoro pool at all — the registry would reject every
    /// character voice at render time and the chapter would ship silent. Voices are config,
    /// not code (§4.4), and this is the seam that makes that true in practice.
    ///
    /// The narrator's and `SYSTEM`'s voices are filtered out of the pool, so a character
    /// can never draw one of them however the caller ordered the list.
    pub fn with_voices(narrator_voice: String, system_voice: String, pool: Vec<String>) -> Self {
        let pool = pool
            .into_iter()
            .filter(|v| *v != narrator_voice && *v != system_voice)
            .collect();
        Self {
            narrator_voice,
            system_voice,
            pool,
        }
    }

    pub fn narrator_voice(&self) -> &str {
        &self.narrator_voice
    }

    pub fn system_voice(&self) -> &str {
        &self.system_voice
    }

    /// The character pool actually in use, after reserved voices were removed.
    pub fn pool(&self) -> &[String] {
        &self.pool
    }

    /// Voice for a speaker that already has one, or `None`.
    fn existing<'a>(existing_cast: &'a [(String, String)], speaker: &str) -> Option<&'a str> {
        existing_cast
            .iter()
            .find(|(s, _)| s.eq_ignore_ascii_case(speaker))
            .map(|(_, v)| v.as_str())
    }

    /// Assign voices to every speaker in `speakers` that `existing_cast` does not
    /// already cover, in order of first appearance.
    ///
    /// `existing_cast` is `(speaker, voice_ref)` pairs from the `cast` table. Matching
    /// is case-insensitive, mirroring the parser's canonicalisation, so `[Kaelen]` and
    /// `[KAELEN]` cannot end up as two rows drawing two voices.
    ///
    /// Returns only the **new** rows, so the caller upserts exactly what changed.
    pub fn assign(
        &self,
        speakers: &[ParsedSpeaker],
        existing_cast: &[(String, String)],
    ) -> Vec<CastAssignment> {
        let mut taken: Vec<String> = existing_cast.iter().map(|(_, v)| v.clone()).collect();
        let mut out: Vec<CastAssignment> = Vec::new();

        for sp in speakers {
            let already_cast = Self::existing(existing_cast, &sp.speaker).is_some()
                || out
                    .iter()
                    .any(|a| a.speaker.eq_ignore_ascii_case(&sp.speaker));
            if already_cast {
                continue;
            }

            let voice_ref = match sp.kind {
                SpeakerKind::Narrator => self.narrator_voice.clone(),
                SpeakerKind::System => self.system_voice.clone(),
                SpeakerKind::Character => self.draw_character_voice(&taken),
            };

            taken.push(voice_ref.clone());
            out.push(CastAssignment {
                speaker: sp.speaker.clone(),
                kind: sp.kind,
                voice_ref,
            });
        }

        out
    }

    /// First unused pool entry; if the pool is exhausted, wrap by the count of drawn
    /// character voices.
    ///
    /// Wrapping rather than erroring is deliberate: voices are a finite resource and
    /// §10's rule is that a bookkeeping limit must never cost a chapter. Two minor
    /// characters sharing a voice is a cosmetic flaw; a chapter that never ships is not.
    fn draw_character_voice(&self, taken: &[String]) -> String {
        if let Some(free) = self.pool.iter().find(|v| !taken.contains(v)) {
            return free.clone();
        }

        let drawn = taken.iter().filter(|v| self.pool.contains(v)).count();
        self.pool[drawn % self.pool.len()].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_excludes_both_reserved_voices() {
        let pool = character_pool();
        assert!(!pool.contains(&SYSTEM_VOICE.to_string()));
        assert!(!pool.contains(&NARRATOR_FALLBACK_VOICE.to_string()));
        // 28 English sids minus the two reserved.
        assert_eq!(pool.len(), 26);
    }

    #[test]
    fn every_english_sid_is_either_pooled_or_reserved() {
        let pool = character_pool();
        for sid in 0..=27u32 {
            let v = kokoro_voice_ref(sid);
            assert!(
                pool.contains(&v) || v == SYSTEM_VOICE || v == NARRATOR_FALLBACK_VOICE,
                "sid {sid} is neither in the pool nor reserved"
            );
        }
    }

    #[test]
    fn narrator_uses_the_configured_voice_not_the_fallback() {
        let a = VoiceAssigner::new("sherpa:piper-en_GB-cori:0".to_string());
        assert_eq!(a.narrator_voice(), "sherpa:piper-en_GB-cori:0");
    }
}
