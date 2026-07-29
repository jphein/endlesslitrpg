//! Reconciling configured voices against the backends that are actually loaded.
//!
//! A `voice_ref` names its backend (§7.3), so a configured `sherpa:` voice on a deployment
//! where only Azure registered is unrenderable. Left alone, that failure surfaces once per
//! chapter as `has_audio = false` — the text ships, the render is queued, retried, and fails
//! identically forever, and the serial quietly becomes text-only.
//!
//! §7.3 already says the right thing: an unknown backend should fail **at cast-assignment
//! time, not at render time**. This module moves that check earlier still, to startup, where a
//! misconfiguration is one clear error instead of an endless stream of degraded chapters.
//!
//! Pure functions over plain inputs, so the interesting rules are unit tests rather than
//! something you only discover by deploying.

use litrpg_core::speaker::{self, same_speaker};
use litrpg_core::{SpeakerKind, VoiceRef};
use litrpg_tts::{Gender, VoiceDesc};

use crate::error::EngineError;
use crate::render::PlannedSegment;

/// The voices the engine will actually use, plus what had to be changed to get there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoicePlan {
    pub narrator: String,
    pub system: String,
    pub characters: Vec<String>,
    /// Human-readable notes about substitutions, for logging at startup. Empty when the
    /// configuration was used verbatim.
    pub notes: Vec<String>,
}

/// Whether `voice_ref` parses and names a registered backend.
pub fn is_usable(voice_ref: &str, registered_backends: &[String]) -> bool {
    VoiceRef::parse(voice_ref)
        .map(|v| registered_backends.contains(&v.backend))
        .unwrap_or(false)
}

/// Reconcile requested voices with what is loaded.
///
/// Requested voices are kept whenever they are usable. Anything unusable is replaced from the
/// registry's own catalogue — "use the voices you actually have" — and every substitution is
/// recorded in [`VoicePlan::notes`] so a startup log says exactly what happened rather than
/// leaving an operator to infer it from the audio.
///
/// Fails only when *nothing* is renderable, which is a genuine stop-and-fix.
pub fn plan_voices(
    narrator: &str,
    system: &str,
    characters: &[String],
    registered_backends: &[String],
    advertised: &[VoiceDesc],
) -> Result<VoicePlan, EngineError> {
    let usable_advertised: Vec<&VoiceDesc> = advertised
        .iter()
        .filter(|v| is_usable(&v.voice_ref, registered_backends))
        .collect();

    let mut notes = Vec::new();

    let narrator = if is_usable(narrator, registered_backends) {
        narrator.to_string()
    } else {
        let pick = usable_advertised
            .first()
            .ok_or_else(|| EngineError::Library {
                detail: format!(
                    "no usable TTS voice: narrator {narrator:?} names an unregistered backend \
                     and the registered backends {registered_backends:?} advertise nothing"
                ),
            })?
            .voice_ref
            .clone();
        notes.push(format!(
            "narrator voice {narrator:?} is not renderable by {registered_backends:?}; \
             substituted {pick:?}"
        ));
        pick
    };

    let system = if is_usable(system, registered_backends) {
        system.to_string()
    } else {
        // Prefer a voice distinct from the narrator so a stat block still sounds separate.
        let pick = usable_advertised
            .iter()
            .find(|v| v.voice_ref != narrator)
            .map(|v| v.voice_ref.clone())
            .unwrap_or_else(|| narrator.clone());
        notes.push(format!(
            "SYSTEM voice {system:?} is not renderable by {registered_backends:?}; \
             substituted {pick:?}"
        ));
        pick
    };

    let kept: Vec<String> = characters
        .iter()
        .filter(|v| is_usable(v, registered_backends))
        .filter(|v| **v != narrator && **v != system)
        .cloned()
        .collect();

    let characters = if kept.is_empty() {
        let derived = interleave_by_gender(
            &usable_advertised
                .iter()
                .filter(|v| v.voice_ref != narrator && v.voice_ref != system)
                .copied()
                .collect::<Vec<_>>(),
        );
        if derived.is_empty() {
            // Not fatal: narration and SYSTEM still render, and a character falls back to
            // the narrator's voice. Worth shouting about, not worth refusing to start.
            notes.push(
                "no character voices are renderable; every character will use the narrator's \
                 voice"
                    .to_string(),
            );
        } else {
            notes.push(format!(
                "no configured character voice is renderable; derived a pool of {} from the \
                 registry",
                derived.len()
            ));
        }
        derived
    } else {
        if kept.len() < characters.len() {
            notes.push(format!(
                "dropped {} configured character voice(s) that name unregistered backends",
                characters.len() - kept.len()
            ));
        }
        kept
    };

    Ok(VoicePlan {
        narrator,
        system,
        characters,
        notes,
    })
}

/// Replace voice references that the loaded backends cannot render.
///
/// [`plan_voices`] validates *configuration*, but the `cast` table is what actually supplies a
/// voice for an established speaker — and a cast row written under one set of backends is
/// unrenderable under another. Measured: chapter 1 was generated by a sherpa-enabled build, so
/// `narrator` and `SYSTEM` hold `sherpa:` rows; an Azure-only build then failed the whole
/// chapter's render with `no TTS backend registered with id 'sherpa'`, because the startup
/// preflight never sees the cast table.
///
/// **Non-destructive on purpose.** The cast rows are left untouched, so rebuilding with sherpa
/// restores the original voices — the substitution is a property of *this process*, not a
/// rewrite of the story's history. The trade is that audio for those chapters differs between
/// builds, which is fine: audio is regenerable and the manifest records what was actually used.
///
/// Returns `(speaker, substituted voice)` pairs, deduplicated, for logging.
pub fn substitute_unrenderable(
    planned: &mut [PlannedSegment],
    registered_backends: &[String],
    narrator: &str,
    system: &str,
    pool: &[String],
) -> Vec<(String, String)> {
    // Nothing known about the registry: leave everything alone rather than guessing.
    if registered_backends.is_empty() {
        return Vec::new();
    }

    // Voices already in use by segments that *can* render, so a substitution does not collide.
    let mut taken: Vec<String> = planned
        .iter()
        .filter(|p| is_usable(&p.voice_ref, registered_backends))
        .map(|p| p.voice_ref.clone())
        .collect();

    let mut decided: Vec<(String, String)> = Vec::new();

    for seg in planned.iter_mut() {
        if is_usable(&seg.voice_ref, registered_backends) {
            continue;
        }

        // One decision per speaker, so every segment of theirs gets the same voice.
        if let Some((_, v)) = decided
            .iter()
            .find(|(sp, _)| same_speaker(sp, &seg.speaker))
        {
            seg.voice_ref = v.clone();
            continue;
        }

        let replacement = match seg.kind {
            SpeakerKind::Narrator => narrator.to_string(),
            SpeakerKind::System => system.to_string(),
            SpeakerKind::Character => pool
                .iter()
                .find(|v| is_usable(v, registered_backends) && !taken.contains(v))
                .cloned()
                // Better a character in the narrator's voice than a silent chapter.
                .unwrap_or_else(|| narrator.to_string()),
        };

        taken.push(replacement.clone());
        decided.push((seg.speaker.clone(), replacement.clone()));
        seg.voice_ref = replacement;
    }

    decided
}

/// Report where `litrpg.toml` disagrees with the `cast` table about an established voice.
///
/// # Ownership: the cast owns a voice; config seeds it
///
/// Stated plainly because "it works out" is how this class of bug survives. Both places carry the
/// narrator and system voices, and the resolution is:
///
/// * **`cast` is the owner.** Every lookup — fresh parse and resume alike — hits the cast first, so
///   once a speaker has a row, that row decides what they sound like. This is what makes a voice
///   *permanent*, which is the property continuity depends on.
/// * **Config seeds a voice** at a speaker's first appearance, and never again. `plan_segments`'s
///   fallback to `narrator_voice` is unreachable in practice, because step 4 casts every speaker
///   before planning; it exists so an impossible state renders in the wrong voice rather than
///   dropping a segment.
///
/// The consequence, and the reason this function exists: **editing `narrator_voice` in the config of
/// a story that already has a cast row does nothing at all.** That is correct under the ownership
/// rule, and it is silent, which is not. `litrpg cast` is the way to change an established voice, so
/// a divergence is worth saying out loud at startup rather than leaving someone to wonder why their
/// edit had no effect.
pub fn config_cast_divergence(
    cast: &[(String, String)],
    narrator_voice: &str,
    system_voice: &str,
) -> Vec<String> {
    let mut notes = Vec::new();
    for (role, configured) in [
        (speaker::NARRATOR, narrator_voice),
        (speaker::SYSTEM, system_voice),
    ] {
        let Some((_, cast_voice)) = cast.iter().find(|(sp, _)| same_speaker(sp, role)) else {
            continue; // not cast yet, so config will seed it — no divergence
        };
        if cast_voice != configured {
            notes.push(format!(
                "config sets the {role} voice to {configured:?} but the cast row says \
                 {cast_voice:?}; the cast wins, so this chapter will sound like {cast_voice:?}. \
                 Use `litrpg cast` to change an established voice — editing the config only \
                 affects speakers cast from now on."
            ));
        }
    }
    notes
}

/// Round-robin the voices across gender groups.
///
/// Same reasoning as the Kokoro pool in [`crate::cast`]: a small cast should span the
/// available voices rather than draw four of the same kind, and that only happens if the pool
/// alternates instead of listing one group first.
fn interleave_by_gender(voices: &[&VoiceDesc]) -> Vec<String> {
    let group = |g: Gender| -> Vec<String> {
        voices
            .iter()
            .filter(|v| v.gender == g)
            .map(|v| v.voice_ref.clone())
            .collect()
    };

    let groups = [
        group(Gender::Female),
        group(Gender::Male),
        group(Gender::Neutral),
        group(Gender::Unknown),
    ];

    let longest = groups.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::with_capacity(voices.len());
    for i in 0..longest {
        for g in &groups {
            if let Some(v) = g.get(i) {
                out.push(v.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use litrpg_tts::CostClass;

    fn voice(id: &str, gender: Gender) -> VoiceDesc {
        VoiceDesc {
            voice_ref: id.to_string(),
            label: id.to_string(),
            lang: "en-US".to_string(),
            gender,
            cost_class: CostClass::Metered,
        }
    }

    fn azure_catalog() -> Vec<VoiceDesc> {
        vec![
            voice("azure:a-f1", Gender::Female),
            voice("azure:a-f2", Gender::Female),
            voice("azure:a-m1", Gender::Male),
            voice("azure:a-m2", Gender::Male),
        ]
    }

    fn azure() -> Vec<String> {
        vec!["azure".to_string()]
    }

    #[test]
    fn a_fully_usable_configuration_is_kept_verbatim_and_silent() {
        let chars = vec!["azure:a-m1".to_string(), "azure:a-f2".to_string()];
        let plan = plan_voices(
            "azure:a-f1",
            "azure:a-m2",
            &chars,
            &azure(),
            &azure_catalog(),
        )
        .unwrap();

        assert_eq!(plan.narrator, "azure:a-f1");
        assert_eq!(plan.system, "azure:a-m2");
        assert_eq!(plan.characters, chars);
        assert!(
            plan.notes.is_empty(),
            "no substitution, so nothing to report"
        );
    }

    /// The real misconfiguration: sherpa voices in config, only Azure loaded.
    #[test]
    fn sherpa_voices_with_only_azure_loaded_are_all_substituted() {
        let chars = vec![
            "sherpa:kokoro-multi-lang-v1_0:1".to_string(),
            "sherpa:kokoro-multi-lang-v1_0:11".to_string(),
        ];
        let plan = plan_voices(
            "sherpa:piper-en_GB-cori:0",
            "sherpa:kokoro-multi-lang-v1_0:24",
            &chars,
            &azure(),
            &azure_catalog(),
        )
        .unwrap();

        for v in std::iter::once(&plan.narrator)
            .chain(std::iter::once(&plan.system))
            .chain(plan.characters.iter())
        {
            assert!(v.starts_with("azure:"), "{v} is not renderable");
        }
        assert_ne!(plan.system, plan.narrator, "SYSTEM should stay distinct");
        assert!(!plan.characters.is_empty());
        assert_eq!(
            plan.notes.len(),
            3,
            "narrator, SYSTEM and the pool all changed"
        );
    }

    #[test]
    fn a_partially_usable_pool_keeps_the_usable_half_and_says_so() {
        let chars = vec![
            "azure:a-m1".to_string(),
            "sherpa:kokoro-multi-lang-v1_0:1".to_string(),
        ];
        let plan = plan_voices(
            "azure:a-f1",
            "azure:a-m2",
            &chars,
            &azure(),
            &azure_catalog(),
        )
        .unwrap();
        assert_eq!(plan.characters, vec!["azure:a-m1".to_string()]);
        assert_eq!(plan.notes.len(), 1);
        assert!(plan.notes[0].contains("dropped 1"));
    }

    #[test]
    fn the_derived_pool_excludes_the_narrator_and_system_voices() {
        let plan =
            plan_voices("azure:a-f1", "azure:a-m1", &[], &azure(), &azure_catalog()).unwrap();
        assert!(!plan.characters.contains(&"azure:a-f1".to_string()));
        assert!(!plan.characters.contains(&"azure:a-m1".to_string()));
        assert_eq!(plan.characters.len(), 2);
    }

    #[test]
    fn the_derived_pool_alternates_gender() {
        let catalog = vec![
            voice("azure:f1", Gender::Female),
            voice("azure:f2", Gender::Female),
            voice("azure:f3", Gender::Female),
            voice("azure:m1", Gender::Male),
            voice("azure:m2", Gender::Male),
        ];
        // Narrator and SYSTEM take voices outside the pool so all five remain.
        let plan = plan_voices("azure:x", "azure:y", &[], &azure(), &catalog).unwrap();
        assert_eq!(
            plan.characters,
            vec!["azure:f1", "azure:m1", "azure:f2", "azure:m2", "azure:f3"],
            "a small cast must span genders rather than drawing three women first"
        );
    }

    #[test]
    fn no_usable_voice_at_all_is_a_startup_error() {
        let err = plan_voices(
            "sherpa:piper-en_GB-cori:0",
            "sherpa:kokoro-multi-lang-v1_0:24",
            &[],
            &azure(),
            &[],
        )
        .unwrap_err();
        assert!(
            format!("{err}").contains("no usable TTS voice"),
            "got {err}"
        );
    }

    #[test]
    fn a_single_voice_backend_still_starts_with_a_warning() {
        // One voice: it becomes the narrator, SYSTEM falls back to it, and there is no
        // character pool. Degraded, but a chapter still renders -- so it must not refuse.
        let catalog = vec![voice("azure:only", Gender::Female)];
        let plan = plan_voices("sherpa:a:0", "sherpa:b:0", &[], &azure(), &catalog).unwrap();
        assert_eq!(plan.narrator, "azure:only");
        assert_eq!(plan.system, "azure:only");
        assert!(plan.characters.is_empty());
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("every character will use")),
            "the degradation must be stated: {:?}",
            plan.notes
        );
    }

    #[test]
    fn a_malformed_voice_ref_is_not_usable() {
        assert!(!is_usable("no-colon", &azure()));
        assert!(!is_usable("", &azure()));
        assert!(is_usable("azure:x", &azure()));
        // Azure names contain colons; only the first splits (§7.3).
        assert!(is_usable("azure:en-GB-Ada:DragonHDLatestNeural", &azure()));
    }

    fn seg(idx: u32, speaker: &str, kind: SpeakerKind, voice: &str) -> PlannedSegment {
        PlannedSegment {
            idx,
            speaker: speaker.to_string(),
            kind,
            voice_ref: voice.to_string(),
            text: format!("line {idx}"),
        }
    }

    /// The measured failure: chapter 1 was cast by a sherpa-enabled build, so `narrator` and
    /// `SYSTEM` hold `sherpa:` rows. An Azure-only build then lost the whole chapter's audio to
    /// `no TTS backend registered with id 'sherpa'`, because the startup preflight validates
    /// configuration and never looks at the cast table.
    #[test]
    fn a_config_edit_that_the_cast_overrides_is_reported() {
        let cast = vec![
            ("narrator".to_string(), "sherpa:cori:0".to_string()),
            ("SYSTEM".to_string(), "sherpa:kokoro:24".to_string()),
        ];

        // Agreement is silent.
        assert!(config_cast_divergence(&cast, "sherpa:cori:0", "sherpa:kokoro:24").is_empty());

        // A narrator edit that will not take effect says so, and names both sides.
        let notes = config_cast_divergence(&cast, "azure:en-GB-Ada", "sherpa:kokoro:24");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("narrator"));
        assert!(
            notes[0].contains("azure:en-GB-Ada"),
            "the ignored value: {}",
            notes[0]
        );
        assert!(
            notes[0].contains("sherpa:cori:0"),
            "the winning value: {}",
            notes[0]
        );
        assert!(
            notes[0].contains("litrpg cast"),
            "and how to actually change it"
        );

        // Both roles can diverge at once.
        assert_eq!(config_cast_divergence(&cast, "azure:a", "azure:b").len(), 2);
    }

    #[test]
    fn an_uncast_role_is_not_a_divergence() {
        // Nothing established yet, so config is about to seed it — that is the design, not a clash.
        assert!(config_cast_divergence(&[], "sherpa:cori:0", "sherpa:kokoro:24").is_empty());
    }

    #[test]
    fn role_matching_is_case_insensitive() {
        let cast = vec![("Narrator".to_string(), "sherpa:cori:0".to_string())];
        assert_eq!(
            config_cast_divergence(&cast, "azure:other", "unused").len(),
            1,
            "a case variant is the same role"
        );
    }

    #[test]
    fn a_cast_row_from_another_backend_is_substituted_not_fatal() {
        let mut planned = vec![
            seg(
                0,
                "narrator",
                SpeakerKind::Narrator,
                "sherpa:piper-en_GB-cori-high:0",
            ),
            seg(1, "Kaelen", SpeakerKind::Character, "azure:a-m1"),
            seg(
                2,
                "SYSTEM",
                SpeakerKind::System,
                "sherpa:kokoro-multi-lang-v1_0:24",
            ),
            seg(
                3,
                "narrator",
                SpeakerKind::Narrator,
                "sherpa:piper-en_GB-cori-high:0",
            ),
        ];

        let changed = substitute_unrenderable(
            &mut planned,
            &azure(),
            "azure:narr",
            "azure:sys",
            &["azure:a-f1".to_string(), "azure:a-f2".to_string()],
        );

        assert_eq!(planned[0].voice_ref, "azure:narr");
        assert_eq!(
            planned[1].voice_ref, "azure:a-m1",
            "a usable voice is untouched"
        );
        assert_eq!(planned[2].voice_ref, "azure:sys");
        assert_eq!(
            planned[3].voice_ref, "azure:narr",
            "every segment of one speaker gets the same substitute"
        );

        // Reported once per speaker, not once per segment.
        assert_eq!(changed.len(), 2);
        assert!(changed.iter().any(|(s, _)| s == "narrator"));
        assert!(changed.iter().any(|(s, _)| s == "SYSTEM"));
    }

    #[test]
    fn a_substituted_character_does_not_collide_with_a_renderable_one() {
        let mut planned = vec![
            seg(0, "Kaelen", SpeakerKind::Character, "azure:a-f1"),
            seg(1, "Sera", SpeakerKind::Character, "sherpa:x:1"),
        ];
        substitute_unrenderable(
            &mut planned,
            &azure(),
            "azure:narr",
            "azure:sys",
            &["azure:a-f1".to_string(), "azure:a-f2".to_string()],
        );
        assert_eq!(planned[0].voice_ref, "azure:a-f1");
        assert_eq!(
            planned[1].voice_ref, "azure:a-f2",
            "must not steal the voice Kaelen is already using"
        );
    }

    #[test]
    fn an_exhausted_pool_falls_back_to_the_narrator_rather_than_failing() {
        let mut planned = vec![
            seg(0, "Kaelen", SpeakerKind::Character, "sherpa:x:1"),
            seg(1, "Sera", SpeakerKind::Character, "sherpa:x:2"),
        ];
        substitute_unrenderable(
            &mut planned,
            &azure(),
            "azure:narr",
            "azure:sys",
            &["azure:a-f1".to_string()],
        );
        assert_eq!(planned[0].voice_ref, "azure:a-f1");
        assert_eq!(
            planned[1].voice_ref, "azure:narr",
            "a character in the narrator's voice beats a silent chapter"
        );
    }

    #[test]
    fn nothing_happens_when_every_voice_is_renderable() {
        let mut planned = vec![seg(0, "Kaelen", SpeakerKind::Character, "azure:a-m1")];
        let before = planned.clone();
        assert!(
            substitute_unrenderable(&mut planned, &azure(), "azure:narr", "azure:sys", &[])
                .is_empty()
        );
        assert_eq!(planned, before);
    }

    #[test]
    fn an_unknown_registry_leaves_everything_alone() {
        // Empty backends means "we do not know", and guessing would rewrite a working cast.
        let mut planned = vec![seg(0, "narrator", SpeakerKind::Narrator, "sherpa:x:0")];
        let before = planned.clone();
        assert!(
            substitute_unrenderable(&mut planned, &[], "azure:narr", "azure:sys", &[]).is_empty()
        );
        assert_eq!(planned, before, "no registry knowledge, no changes");
    }

    #[test]
    fn a_configured_character_voice_equal_to_the_narrator_is_dropped() {
        let chars = vec!["azure:a-f1".to_string(), "azure:a-m1".to_string()];
        let plan = plan_voices(
            "azure:a-f1",
            "azure:a-m2",
            &chars,
            &azure(),
            &azure_catalog(),
        )
        .unwrap();
        assert_eq!(
            plan.characters,
            vec!["azure:a-m1".to_string()],
            "a character must never share the narrator's voice"
        );
    }
}
