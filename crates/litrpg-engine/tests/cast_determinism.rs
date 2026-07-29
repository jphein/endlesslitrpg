//! Voice assignment must be a *function* of (existing cast, speaker order) — never of
//! randomness, hash iteration order, or wall-clock. A cast that shuffles between runs
//! turns "continuity" into "a lottery", and because it is only audible, it would be
//! discovered somewhere around chapter forty.

use litrpg_core::SpeakerKind;
use litrpg_engine::cast::{
    CHARACTER_POOL_LEN, SYSTEM_VOICE, VoiceAssigner, character_pool, kokoro_voice_ref,
};
use litrpg_engine::{NARRATOR_FALLBACK_VOICE, ParsedSpeaker};

fn speakers(names: &[(&str, SpeakerKind)]) -> Vec<ParsedSpeaker> {
    names
        .iter()
        .map(|(n, k)| ParsedSpeaker {
            speaker: n.to_string(),
            kind: *k,
        })
        .collect()
}

fn assigner() -> VoiceAssigner {
    VoiceAssigner::new(NARRATOR_FALLBACK_VOICE.to_string())
}

// ---------------------------------------------------------------------------
// The fixed roles
// ---------------------------------------------------------------------------

#[test]
fn the_narrator_always_gets_the_configured_voice() {
    let a = VoiceAssigner::new("sherpa:piper-en_GB-cori:0".to_string());
    let out = a.assign(&speakers(&[("narrator", SpeakerKind::Narrator)]), &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].voice_ref, "sherpa:piper-en_GB-cori:0", "spec D7");
    assert_eq!(out[0].speaker, "narrator");
}

#[test]
fn system_always_gets_the_one_reserved_voice() {
    let out = assigner().assign(&speakers(&[("SYSTEM", SpeakerKind::System)]), &[]);
    assert_eq!(out[0].voice_ref, SYSTEM_VOICE);
}

#[test]
fn the_system_voice_is_not_in_the_character_pool() {
    // Otherwise a character eventually draws the robot's voice and the one cue that
    // tells a listener "this is a stat block, not a person" stops working.
    assert!(
        !character_pool().iter().any(|v| v == SYSTEM_VOICE),
        "SYSTEM_VOICE {SYSTEM_VOICE} must be excluded from the character pool"
    );
}

#[test]
fn the_narrator_fallback_is_not_in_the_character_pool() {
    assert!(
        !character_pool()
            .iter()
            .any(|v| v == NARRATOR_FALLBACK_VOICE)
    );
}

// ---------------------------------------------------------------------------
// Determinism and stability
// ---------------------------------------------------------------------------

#[test]
fn the_same_input_always_produces_the_same_assignment() {
    let sp = speakers(&[
        ("Kaelen", SpeakerKind::Character),
        ("Sera", SpeakerKind::Character),
        ("narrator", SpeakerKind::Narrator),
        ("SYSTEM", SpeakerKind::System),
    ]);

    let first = assigner().assign(&sp, &[]);
    for _ in 0..20 {
        assert_eq!(
            assigner().assign(&sp, &[]),
            first,
            "assignment must be pure"
        );
    }
}

#[test]
fn an_already_cast_speaker_keeps_its_voice_and_is_not_reassigned() {
    let existing = [(
        "Kaelen".to_string(),
        "sherpa:kokoro-multi-lang-v1_0:7".to_string(),
    )];
    let out = assigner().assign(&speakers(&[("Kaelen", SpeakerKind::Character)]), &existing);
    assert!(
        out.is_empty(),
        "a speaker already in the cast needs no new assignment, got {out:?}"
    );
}

#[test]
fn a_new_speaker_never_takes_a_voice_already_in_use() {
    let existing: Vec<(String, String)> = character_pool()
        .iter()
        .take(3)
        .enumerate()
        .map(|(i, v)| (format!("Existing{i}"), v.clone()))
        .collect();

    let out = assigner().assign(
        &speakers(&[("Newcomer", SpeakerKind::Character)]),
        &existing,
    );
    assert_eq!(out.len(), 1);
    let taken: Vec<&String> = existing.iter().map(|(_, v)| v).collect();
    assert!(
        !taken.contains(&&out[0].voice_ref),
        "{} collides with an existing cast member",
        out[0].voice_ref
    );
}

#[test]
fn assignment_is_stable_as_the_cast_grows_across_cycles() {
    // Simulate three cycles. Each cycle's assignments are persisted, and every
    // previously-cast speaker must still map to the voice it was first given.
    let mut cast: Vec<(String, String)> = Vec::new();
    let mut history: Vec<(String, String)> = Vec::new();

    for (cycle, new_names) in [
        vec!["Kaelen", "Sera"],
        vec!["Kaelen", "Sera", "Vance"],
        vec!["Vance", "Kaelen", "Ilex", "Sera"],
    ]
    .into_iter()
    .enumerate()
    {
        let sp = speakers(
            &new_names
                .iter()
                .map(|n| (*n, SpeakerKind::Character))
                .collect::<Vec<_>>(),
        );
        for a in assigner().assign(&sp, &cast) {
            cast.push((a.speaker.clone(), a.voice_ref.clone()));
            history.push((a.speaker, a.voice_ref));
        }
        assert_eq!(
            cast.len(),
            match cycle {
                0 => 2,
                1 => 3,
                _ => 4,
            },
            "cycle {cycle} assigned the wrong number of new voices"
        );
    }

    // Nobody was assigned twice, and every voice is distinct.
    let names: Vec<&String> = history.iter().map(|(n, _)| n).collect();
    let mut uniq = names.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(
        names.len(),
        uniq.len(),
        "a speaker was cast twice: {history:?}"
    );

    let voices: Vec<&String> = history.iter().map(|(_, v)| v).collect();
    let mut uniq_v = voices.clone();
    uniq_v.sort();
    uniq_v.dedup();
    assert_eq!(voices.len(), uniq_v.len(), "two characters share a voice");
}

#[test]
fn new_speakers_are_assigned_in_first_appearance_order() {
    let out = assigner().assign(
        &speakers(&[
            ("Sera", SpeakerKind::Character),
            ("Kaelen", SpeakerKind::Character),
        ]),
        &[],
    );
    assert_eq!(out[0].speaker, "Sera", "segment order, not alphabetical");
    assert_eq!(out[1].speaker, "Kaelen");

    // Reversing the appearance order must swap the voices, proving the ordering is
    // the appearance order and not a property of the name.
    let rev = assigner().assign(
        &speakers(&[
            ("Kaelen", SpeakerKind::Character),
            ("Sera", SpeakerKind::Character),
        ]),
        &[],
    );
    assert_eq!(rev[0].voice_ref, out[0].voice_ref);
    assert_eq!(rev[0].speaker, "Kaelen");
}

#[test]
fn a_speaker_appearing_twice_in_one_chapter_is_assigned_once() {
    let out = assigner().assign(
        &speakers(&[
            ("Kaelen", SpeakerKind::Character),
            ("Sera", SpeakerKind::Character),
            ("Kaelen", SpeakerKind::Character),
        ]),
        &[],
    );
    assert_eq!(out.len(), 2, "got {out:?}");
}

#[test]
fn matching_against_the_existing_cast_is_case_insensitive() {
    let existing = [(
        "kaelen".to_string(),
        "sherpa:kokoro-multi-lang-v1_0:7".to_string(),
    )];
    let out = assigner().assign(&speakers(&[("Kaelen", SpeakerKind::Character)]), &existing);
    assert!(
        out.is_empty(),
        "a case variant must not mint a second cast row with a second voice"
    );
}

// ---------------------------------------------------------------------------
// Pool shape: distinct voices for a large cast, alternating as it grows
// ---------------------------------------------------------------------------

#[test]
fn the_pool_covers_the_kokoro_english_range_without_duplicates() {
    let pool = character_pool();
    assert_eq!(pool.len(), CHARACTER_POOL_LEN);
    assert!(pool.len() >= 24, "a large cast needs many distinct voices");

    let mut sorted = pool.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), pool.len(), "the pool contains a duplicate");

    for v in &pool {
        assert!(
            v.starts_with("sherpa:kokoro-multi-lang-v1_0:"),
            "unexpected voice_ref shape: {v}"
        );
    }
}

#[test]
fn the_pool_alternates_rather_than_exhausting_one_block() {
    // Spec §4.4 sid map: 0-10 Am-F, 11-19 Am-M, 20-23 Br-F, 24-27 Br-M. Taking the
    // first four voices should span all four groups, so a four-person cast sounds
    // like four people rather than four American women.
    fn group(v: &str) -> &'static str {
        let sid: u32 = v.rsplit(':').next().unwrap().parse().unwrap();
        match sid {
            0..=10 => "am-f",
            11..=19 => "am-m",
            20..=23 => "br-f",
            _ => "br-m",
        }
    }

    let pool = character_pool();
    let first_four: Vec<&str> = pool.iter().take(4).map(|v| group(v)).collect();
    let mut uniq = first_four.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        4,
        "the first four voices should span all four gender/accent groups, got {first_four:?}"
    );
}

#[test]
fn a_cast_larger_than_the_pool_wraps_deterministically_instead_of_failing() {
    // Voices are a finite resource. Running out must not abort a chapter (§10: a
    // bookkeeping failure never costs a chapter), so assignment wraps.
    let existing: Vec<(String, String)> = character_pool()
        .iter()
        .enumerate()
        .map(|(i, v)| (format!("Existing{i}"), v.clone()))
        .collect();

    let sp = speakers(&[("Overflow", SpeakerKind::Character)]);
    let out = assigner().assign(&sp, &existing);
    assert_eq!(out.len(), 1, "must still assign something");
    assert!(character_pool().contains(&out[0].voice_ref));
    assert_eq!(
        out,
        assigner().assign(&sp, &existing),
        "wrap must be deterministic"
    );
}

#[test]
fn kokoro_voice_ref_matches_the_spec_format() {
    assert_eq!(kokoro_voice_ref(18), "sherpa:kokoro-multi-lang-v1_0:18");
}
