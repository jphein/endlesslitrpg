//! Issue #6, reproduced offline and then pinned.
//!
//! The live cast had three characters and **all three drew female-presenting voices** while Kaelen
//! is `he` throughout the prompt. Three candidate causes: the hint never reached the assigner, the
//! pool had no male voice, or the gender data was wrong.
//!
//! It was the first. These tests reproduce the live draw exactly from the real Azure catalogue,
//! with no network and no credentials, and pin both the cause and the fix.

use litrpg_core::SpeakerKind;
use litrpg_engine::{ParsedSpeaker, VoiceAssigner, plan_voices};
use litrpg_tts::{TtsBackend, azure::AzureBackend, azure::AzureConfig};
use std::collections::BTreeMap;

/// The real Azure catalogue, with no credentials and no network: `voices()` is a static list.
fn azure_catalogue() -> Vec<litrpg_tts::VoiceDesc> {
    let cfg = AzureConfig::from_json_str(r#"{"key":"not-used","tts_region":"eastus"}"#)
        .expect("static config");
    TtsBackend::voices(&AzureBackend::new(cfg))
}

fn genders(v: &[litrpg_tts::VoiceDesc]) -> BTreeMap<String, litrpg_tts::Gender> {
    v.iter().map(|d| (d.voice_ref.clone(), d.gender)).collect()
}

/// The pool is balanced and gendered correctly, so neither of the other two causes holds.
#[test]
fn the_derived_pool_alternates_gender_and_has_male_voices() {
    let advertised = azure_catalogue();
    let g = genders(&advertised);
    let plan = plan_voices(
        "sherpa:piper-en_GB-cori-high:0",
        "sherpa:kokoro-multi-lang-v1_0:24",
        &[],
        &["azure".to_string()],
        &advertised,
    )
    .unwrap();

    let male = plan
        .characters
        .iter()
        .filter(|v| g.get(*v) == Some(&litrpg_tts::Gender::Male))
        .count();
    assert!(
        male >= 5,
        "the pool is not short of male voices: {male} of {}",
        plan.characters.len()
    );

    // And it alternates, so position 0 is female, 1 male, 2 female.
    let at = |i: usize| g.get(&plan.characters[i]).copied();
    assert_eq!(at(0), Some(litrpg_tts::Gender::Female));
    assert_eq!(at(1), Some(litrpg_tts::Gender::Male));
    assert_eq!(at(2), Some(litrpg_tts::Gender::Female));
}

/// The cause: with a hint for one character only, the other two keep their round-robin draw — and
/// because the pool alternates from a *fixed* start, whoever speaks first is female regardless of
/// who they are. That reproduces the live cast exactly.
#[test]
fn a_missing_hint_reproduces_the_live_cast_exactly() {
    let advertised = azure_catalogue();
    let g = genders(&advertised);
    let plan = plan_voices(
        "sherpa:piper-en_GB-cori-high:0",
        "sherpa:kokoro-multi-lang-v1_0:24",
        &[],
        &["azure".to_string()],
        &advertised,
    )
    .unwrap();
    let assigner = VoiceAssigner::with_voices(plan.narrator, plan.system, plan.characters)
        .with_genders(g.clone());

    let speakers: Vec<ParsedSpeaker> = ["Kaelen", "Sera", "Shadow"]
        .iter()
        .map(|n| ParsedSpeaker {
            speaker: n.to_string(),
            kind: SpeakerKind::Character,
        })
        .collect();
    let drawn = assigner.assign(&speakers, &[]);
    let all: Vec<String> = drawn.iter().map(|a| a.voice_ref.clone()).collect();

    // Live: pass 2 reported a gender for Sera and silently omitted Kaelen and Shadow.
    let mut only_sera = BTreeMap::new();
    only_sera.insert("sera".to_string(), "female".to_string());
    let fixed = assigner.regender(&drawn, &only_sera, &all);

    let final_voice = |name: &str| -> String {
        fixed
            .iter()
            .find(|f| f.speaker == name)
            .or_else(|| drawn.iter().find(|d| d.speaker == name))
            .unwrap()
            .voice_ref
            .clone()
    };

    // Byte-for-byte the live cast table.
    assert_eq!(
        final_voice("Kaelen"),
        "azure:en-US-Aria:DragonHDLatestNeural"
    );
    assert_eq!(final_voice("Sera"), "azure:en-US-Bree:DragonHDLatestNeural");
    assert_eq!(
        final_voice("Shadow"),
        "azure:en-US-Ava:DragonHDLatestNeural"
    );
    for name in ["Kaelen", "Sera", "Shadow"] {
        assert_eq!(
            g.get(&final_voice(name)),
            Some(&litrpg_tts::Gender::Female),
            "{name} drew a female voice, as observed live"
        );
    }
}

/// And with the hint present for everyone, the machinery is correct — so the defect was the missing
/// input, not the casting.
#[test]
fn hints_for_everyone_produce_correctly_gendered_voices() {
    let advertised = azure_catalogue();
    let g = genders(&advertised);
    let plan = plan_voices(
        "sherpa:piper-en_GB-cori-high:0",
        "sherpa:kokoro-multi-lang-v1_0:24",
        &[],
        &["azure".to_string()],
        &advertised,
    )
    .unwrap();
    let assigner = VoiceAssigner::with_voices(plan.narrator, plan.system, plan.characters)
        .with_genders(g.clone());

    let speakers: Vec<ParsedSpeaker> = ["Kaelen", "Sera", "Shadow"]
        .iter()
        .map(|n| ParsedSpeaker {
            speaker: n.to_string(),
            kind: SpeakerKind::Character,
        })
        .collect();
    let drawn = assigner.assign(&speakers, &[]);
    let all: Vec<String> = drawn.iter().map(|a| a.voice_ref.clone()).collect();

    let mut hints = BTreeMap::new();
    hints.insert("kaelen".to_string(), "male".to_string());
    hints.insert("sera".to_string(), "female".to_string());
    hints.insert("shadow".to_string(), "male".to_string());
    let fixed = assigner.regender(&drawn, &hints, &all);

    let want = [
        ("Kaelen", litrpg_tts::Gender::Male),
        ("Sera", litrpg_tts::Gender::Female),
        ("Shadow", litrpg_tts::Gender::Male),
    ];
    for (name, expect) in want {
        let voice = fixed
            .iter()
            .find(|f| f.speaker == name)
            .map(|f| f.voice_ref.clone())
            .unwrap_or_else(|| {
                drawn
                    .iter()
                    .find(|d| d.speaker == name)
                    .unwrap()
                    .voice_ref
                    .clone()
            });
        assert_eq!(g.get(&voice), Some(&expect), "{name} -> {voice}");
    }

    // No two characters share a voice after re-gendering.
    let mut voices: Vec<String> = ["Kaelen", "Sera", "Shadow"]
        .iter()
        .map(|name| {
            fixed
                .iter()
                .find(|f| f.speaker == *name)
                .map(|f| f.voice_ref.clone())
                .unwrap_or_else(|| {
                    drawn
                        .iter()
                        .find(|d| d.speaker == *name)
                        .unwrap()
                        .voice_ref
                        .clone()
                })
        })
        .collect();
    voices.sort();
    let before = voices.len();
    voices.dedup();
    assert_eq!(
        before,
        voices.len(),
        "re-gendering must not make two characters share a voice"
    );
}

#[test]
#[ignore = "diagnostic dump, kept for the next time this needs investigating"]
fn reproduce_the_live_draw() {
    let advertised = azure_catalogue();
    let backends = vec!["azure".to_string()];

    // Exactly the live situation: config's sherpa voices are unrenderable on an Azure-only build,
    // so `plan_voices` derives the pool from the registry.
    let plan = plan_voices(
        "sherpa:piper-en_GB-cori-high:0",
        "sherpa:kokoro-multi-lang-v1_0:24",
        &[],
        &backends,
        &advertised,
    )
    .expect("a pool");
    eprintln!("narrator = {}", plan.narrator);
    eprintln!("system   = {}", plan.system);
    eprintln!(
        "pool[0..6] = {:#?}",
        &plan.characters[..6.min(plan.characters.len())]
    );

    let g = genders(&advertised);
    let gender_of = |v: &str| g.get(v).copied();
    eprintln!("\npool genders in order:");
    for (i, v) in plan.characters.iter().take(6).enumerate() {
        eprintln!("  {i}: {v} -> {:?}", gender_of(v));
    }

    // Cast the three live characters, in the order the log shows them appearing.
    let assigner = VoiceAssigner::with_voices(
        plan.narrator.clone(),
        plan.system.clone(),
        plan.characters.clone(),
    )
    .with_genders(g.clone());

    let speakers: Vec<ParsedSpeaker> = ["Kaelen", "Sera", "Shadow"]
        .iter()
        .map(|n| ParsedSpeaker {
            speaker: n.to_string(),
            kind: SpeakerKind::Character,
        })
        .collect();
    let drawn = assigner.assign(&speakers, &[]);
    eprintln!("\ninitial draw:");
    for a in &drawn {
        eprintln!(
            "  {:<8} {:<45} {:?}",
            a.speaker,
            a.voice_ref,
            gender_of(&a.voice_ref)
        );
    }

    // The live log shows pass 2 reporting a hint for Sera only.
    let mut wanted = BTreeMap::new();
    wanted.insert("sera".to_string(), "female".to_string());
    let all: Vec<String> = drawn.iter().map(|a| a.voice_ref.clone()).collect();
    let fixed = assigner.regender(&drawn, &wanted, &all);
    eprintln!("\nregender with a hint for Sera only:");
    for f in &fixed {
        eprintln!(
            "  {:<8} -> {:<45} {:?}",
            f.speaker,
            f.voice_ref,
            gender_of(&f.voice_ref)
        );
    }

    // Now with a hint for everyone, which is what should have happened.
    let mut all_hints = BTreeMap::new();
    all_hints.insert("kaelen".to_string(), "male".to_string());
    all_hints.insert("sera".to_string(), "female".to_string());
    all_hints.insert("shadow".to_string(), "male".to_string());
    let fixed_all = assigner.regender(&drawn, &all_hints, &all);
    eprintln!("\nregender with hints for all three:");
    for f in &fixed_all {
        eprintln!(
            "  {:<8} -> {:<45} {:?}",
            f.speaker,
            f.voice_ref,
            gender_of(&f.voice_ref)
        );
    }
}
