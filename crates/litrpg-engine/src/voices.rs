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

use litrpg_core::VoiceRef;
use litrpg_tts::{Gender, VoiceDesc};

use crate::error::EngineError;

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
