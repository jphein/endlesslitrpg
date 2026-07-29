//! One fixture, driven through `litrpg_core::speaker` **and** the engine's own paths, so a future
//! divergence fails a test rather than living in a comment (§5.6).
//!
//! This exists because there were seven rules for "are these the same person" and they disagreed —
//! most consequentially, `cast.speaker TEXT NOT NULL UNIQUE` is a *binary* constraint in SQLite
//! while every reader compared with `eq_ignore_ascii_case`, so which voice a character got was
//! decided by `ORDER BY first_chapter, speaker` rather than by anyone's intent.
//!
//! Describing the agreement in prose is what let it rot the first time. These assert it.

mod support;

use litrpg_core::SpeakerKind;
use litrpg_core::speaker::{canonical, identity_key, is_reserved, same_speaker};
use litrpg_engine::{BufferCursor, CastAssignment, ParsedSpeaker, VoiceAssigner, plan_segments};
use support::*;

/// Spellings of one person that every layer must agree about.
const SAME_PERSON: [&str; 5] = ["Kaelen", "kaelen", "KAELEN", "  Kaelen  ", "Kaelen"];

/// Names that must stay distinct from `Kaelen`, including the near-misses.
const DIFFERENT_PEOPLE: [&str; 4] = ["Kaelith", "Kaelen Vord", "Kael", "Sera"];

#[test]
fn core_agrees_with_itself_on_the_fixture() {
    for a in SAME_PERSON {
        for b in SAME_PERSON {
            assert!(same_speaker(a, b), "{a:?} and {b:?} are one person");
            assert_eq!(identity_key(a), identity_key(b));
        }
        // `canonical` is the *storage* form and **keeps case on purpose** — it is the name a
        // reader sees. Only whitespace is normalised. `identity_key` is what folds case, which is
        // why the two are separate functions and must not be merged.
        assert_eq!(
            canonical(a),
            a.split_whitespace().collect::<Vec<_>>().join(" "),
            "canonical must preserve the case of {a:?}"
        );
        assert_eq!(
            identity_key(a),
            "kaelen",
            "but the comparison form folds it"
        );
    }
    for other in DIFFERENT_PEOPLE {
        assert!(!same_speaker("Kaelen", other), "{other:?} is someone else");
    }
}

/// The engine's cast lookup, which is what actually decides a character's voice.
#[test]
fn the_cast_lookup_agrees_with_same_speaker() {
    let assigner = VoiceAssigner::with_voices(
        "azure:narr".into(),
        "azure:sys".into(),
        vec!["azure:c1".into(), "azure:c2".into()],
    );

    for spelling in SAME_PERSON {
        // One spelling is already cast; every other spelling must resolve to it rather than
        // minting a second row with a second voice.
        let existing = vec![("Kaelen".to_string(), "azure:c1".to_string())];
        let new = assigner.assign(
            &[ParsedSpeaker {
                speaker: spelling.to_string(),
                kind: SpeakerKind::Character,
            }],
            &existing,
        );
        assert!(
            new.is_empty(),
            "{spelling:?} must be recognised as the already-cast Kaelen, got {new:?}"
        );
    }

    // And a genuinely different name *does* get cast.
    for other in DIFFERENT_PEOPLE {
        let existing = vec![("Kaelen".to_string(), "azure:c1".to_string())];
        let new = assigner.assign(
            &[ParsedSpeaker {
                speaker: other.to_string(),
                kind: SpeakerKind::Character,
            }],
            &existing,
        );
        assert_eq!(new.len(), 1, "{other:?} is a new person and needs a voice");
        assert_ne!(new[0].voice_ref, "azure:c1", "and not Kaelen's voice");
    }
}

/// `plan_segments` is the other lookup, and it must not disagree with the assigner.
#[test]
fn plan_segments_agrees_with_same_speaker() {
    let existing = vec![("Kaelen".to_string(), "azure:c1".to_string())];

    for spelling in SAME_PERSON {
        let parsed = vec![litrpg_ember::ParsedSegment {
            idx: 0,
            speaker: spelling.to_string(),
            kind: SpeakerKind::Character,
            text: "\"Mine.\"".to_string(),
        }];
        let planned = plan_segments(&parsed, &existing, &[], "azure:narr");
        assert_eq!(
            planned[0].voice_ref, "azure:c1",
            "{spelling:?} must get Kaelen's voice"
        );
    }
}

/// A new assignment and an existing row must be looked up the same way.
#[test]
fn the_new_cast_lookup_agrees_too() {
    let new_cast = vec![CastAssignment {
        speaker: "Kaelen".to_string(),
        kind: SpeakerKind::Character,
        voice_ref: "azure:fresh".to_string(),
    }];

    for spelling in SAME_PERSON {
        let parsed = vec![litrpg_ember::ParsedSegment {
            idx: 0,
            speaker: spelling.to_string(),
            kind: SpeakerKind::Character,
            text: "x".to_string(),
        }];
        let planned = plan_segments(&parsed, &[], &new_cast, "azure:narr");
        assert_eq!(planned[0].voice_ref, "azure:fresh", "{spelling:?}");
    }
}

/// Ember's parser and core must produce the same storage form, or a row's identity depends on
/// which crate wrote it.
#[test]
fn embers_canonical_speaker_agrees_with_cores_canonical() {
    for name in [
        "Kaelen",
        "  Kaelen  ",
        "Sera   Vane",
        "System Lord",
        "narrator",
        "SYSTEM",
    ] {
        assert_eq!(
            litrpg_ember::parse::canonical_speaker(name),
            canonical(name),
            "ember and core disagree on the storage form of {name:?}"
        );
    }
}

/// The reserved names are one value, not three copies of a string literal.
#[test]
fn the_reserved_names_are_shared_not_duplicated() {
    assert_eq!(
        litrpg_ember::parse::NARRATOR,
        litrpg_core::speaker::NARRATOR
    );
    assert_eq!(litrpg_ember::parse::SYSTEM, litrpg_core::speaker::SYSTEM);
    assert!(is_reserved(litrpg_ember::parse::NARRATOR));
    assert!(is_reserved(litrpg_ember::parse::SYSTEM));
}

/// End to end: the whole cycle must treat the fixture's spellings as one character, so the cast
/// gains one row and the ledger one subject.
#[tokio::test]
async fn the_cycle_treats_the_fixture_as_one_person() {
    let prose = "\
[Kaelen] \"One.\"

[kaelen] \"Two.\"

[  KAELEN  ] \"Three.\"

[narrator] The vale was quiet.

[SYSTEM] XP gained: 10.";

    let extraction = extraction_with(
        vec![
            delta("Kaelen", "xp", "add", Some(10)),
            delta("kaelen", "gold", "add", Some(5)),
        ],
        vec![],
    );

    let e = fixture_engine(prose, extraction);
    e.run_cycle(BufferCursor::At(0)).await.unwrap();

    let characters: Vec<String> = e
        .cast_pairs()
        .into_iter()
        .filter(|(s, _)| !is_reserved(s))
        .map(|(s, _)| s)
        .collect();
    assert_eq!(
        characters.len(),
        1,
        "three spellings must be one cast row, got {characters:?}"
    );

    let snap = e.with_store(|s| s.snapshot()).unwrap();
    let subjects: Vec<&str> = snap.subjects().into_iter().collect();
    assert_eq!(
        subjects.len(),
        1,
        "and one ledger subject, got {subjects:?}"
    );
    assert_eq!(snap.num(subjects[0], "xp"), Some(10));
    assert_eq!(snap.num(subjects[0], "gold"), Some(5));
}

fn fixture_engine(prose: &str, extraction: litrpg_ember::Extraction) -> FakeEngine {
    litrpg_engine::Engine::new(
        store(),
        FakeGenerator::new()
            .with_prose(prose)
            .with_extraction(extraction),
        FakeRenderer::new(),
        FakeLibrary::new(),
        FakeArtifacts::new(),
        litrpg_engine::EngineConfig::default(),
    )
}
