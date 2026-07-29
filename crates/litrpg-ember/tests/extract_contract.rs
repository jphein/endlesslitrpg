//! The pass-2 extraction contract: schema, deserialization, and the mapping onto
//! `litrpg_core` types. Validation is the store's job -- this crate only produces
//! well-typed *proposals*.

use litrpg_core::{Op, validate_delta};
use litrpg_ember::EmberError;
use litrpg_ember::extract::{
    EXTRACTION_SCHEMA, EXTRACTION_SCHEMA_NAME, Extraction, ProposedDelta, ProposedSpeaker,
    parse_extraction, response_format,
};
use serde_json::{Value, json};

fn good_payload() -> String {
    json!({
        "summary": "Kaelen broke the first seal and lost 12 hp.",
        "title": "The First Seal Breaks",
        "deltas": [
            {"subject": "Kaelen", "field": "xp", "op": "add", "value_num": 150, "value_txt": null},
            {"subject": "Kaelen", "field": "hp", "op": "sub", "value_num": 12, "value_txt": null},
            {"subject": "Kaelen", "field": "location", "op": "set", "value_num": null, "value_txt": "Ashen Vale"}
        ],
        "new_lore": [
            {"name": "The First Seal", "kind": "item", "keywords": "first seal,seal",
             "body_md": "A vortex of violet energy in black stone.", "priority": 10}
        ],
        "quest_updates": [
            {"name": "The Ashen Ledger", "status": "advanced", "detail": "1 of 3 seals broken."}
        ]
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// The schema itself. llama.cpp validates it server-side and answers 400 on a bad
// one (measured 2026-07-29), so a broken const here would fail *every* call.
// ---------------------------------------------------------------------------

#[test]
fn the_extraction_schema_is_valid_json() {
    let parsed: Value =
        serde_json::from_str(EXTRACTION_SCHEMA).expect("EXTRACTION_SCHEMA must be valid JSON");
    assert_eq!(parsed["type"], "object");
}

/// Measured 2026-07-29: deriving a title from the *summary* produced
/// `"No stat changes, inventory changes, or location changes are…"` as a user-facing
/// chapter title, because a summary is written for a bookkeeping audience. A title is
/// content, so the model that just read the chapter writes it.
#[test]
fn the_schema_asks_for_a_story_facing_title() {
    let s: Value = serde_json::from_str(EXTRACTION_SCHEMA).unwrap();
    let required: Vec<&str> = s["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"title"), "title must be required");

    let desc = s["properties"]["title"]["description"].as_str().unwrap();
    assert!(
        desc.contains("story"),
        "the title must be told to name the story: {desc}"
    );

    // And the summary must be told to describe events, not the extraction.
    let sum = s["properties"]["summary"]["description"].as_str().unwrap();
    assert!(
        sum.contains("in the story"),
        "the summary must be told to cover story events: {sum}"
    );
    // Measured live: adding `title` made the model start returning `"summary": ""`.
    assert!(
        sum.contains("Never empty"),
        "the floor must be stated: {sum}"
    );
}

/// **Policy change, deliberate.** This previously asserted that a payload with no title parsed
/// and left `title` empty, on the grounds that `derive_title` would fall back to `Chapter N`.
/// That was wrong: it makes a retry-worthy generation indistinguishable from a good one, and the
/// fallback exists for the `state_dirty` case rather than to absorb a silent omission. A missing
/// or empty title is now malformed, which routes it into the temperature-jitter retry — the same
/// treatment an empty summary gets, for the same reason.
#[test]
fn a_payload_with_no_title_is_malformed_rather_than_silently_defaulted() {
    let payload = json!({
        "summary": "s", "deltas": [], "new_lore": [], "quest_updates": []
    })
    .to_string();
    let err = parse_extraction(&payload).expect_err("a missing title must not be absorbed");
    assert!(err.is_malformed(), "got {err:?}");
    assert!(format!("{err}").contains("title"), "{err}");
}

#[test]
fn the_title_is_deserialized_when_present() {
    let payload = json!({
        "title": "The First Seal Breaks",
        "summary": "s", "deltas": [], "new_lore": [], "quest_updates": []
    })
    .to_string();
    assert_eq!(
        parse_extraction(&payload).unwrap().title,
        "The First Seal Breaks"
    );
}

#[test]
fn the_schema_requires_all_four_top_level_keys() {
    let s: Value = serde_json::from_str(EXTRACTION_SCHEMA).unwrap();
    let required: Vec<&str> = s["required"]
        .as_array()
        .expect("required must be an array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for key in ["summary", "deltas", "new_lore", "quest_updates"] {
        assert!(required.contains(&key), "schema does not require {key}");
        assert!(
            s["properties"].get(key).is_some(),
            "schema has no property {key}"
        );
    }
}

#[test]
fn the_schema_constrains_op_to_the_three_ledger_operations() {
    let s: Value = serde_json::from_str(EXTRACTION_SCHEMA).unwrap();
    let ops: Vec<&str> = s["properties"]["deltas"]["items"]["properties"]["op"]["enum"]
        .as_array()
        .expect("op must be an enum so the grammar rules out anything else")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(ops, vec!["set", "add", "sub"], "spec §6.0");
}

#[test]
fn the_schema_forbids_extra_properties_everywhere() {
    let s: Value = serde_json::from_str(EXTRACTION_SCHEMA).unwrap();
    assert_eq!(s["additionalProperties"], json!(false));
    assert_eq!(
        s["properties"]["deltas"]["items"]["additionalProperties"],
        json!(false)
    );
}

#[test]
fn response_format_matches_what_llama_cpp_accepted() {
    let rf = response_format();
    assert_eq!(rf["type"], "json_schema");
    assert_eq!(rf["json_schema"]["name"], EXTRACTION_SCHEMA_NAME);
    assert_eq!(rf["json_schema"]["strict"], json!(true));
    let embedded = &rf["json_schema"]["schema"];
    let standalone: Value = serde_json::from_str(EXTRACTION_SCHEMA).unwrap();
    assert_eq!(
        embedded, &standalone,
        "the wrapper must embed the const verbatim"
    );
}

// ---------------------------------------------------------------------------
// Deserialization
// ---------------------------------------------------------------------------

#[test]
fn a_well_formed_payload_deserializes_completely() {
    let e: Extraction = parse_extraction(&good_payload()).expect("valid payload");
    assert_eq!(e.summary, "Kaelen broke the first seal and lost 12 hp.");
    assert_eq!(e.deltas.len(), 3);
    assert_eq!(e.new_lore.len(), 1);
    assert_eq!(e.new_lore[0].name, "The First Seal");
    assert_eq!(e.new_lore[0].keywords, "first seal,seal");
    assert_eq!(e.quest_updates.len(), 1);
    assert_eq!(e.quest_updates[0].status, "advanced");
}

#[test]
fn empty_arrays_are_fine_a_quiet_chapter_changes_nothing() {
    let payload = json!({
        "summary": "They walked, and nothing happened.", "title": "A Quiet Road",
        "deltas": [], "new_lore": [], "quest_updates": []
    })
    .to_string();
    let e = parse_extraction(&payload).expect("a chapter may legitimately change no state");
    assert!(e.deltas.is_empty());
}

#[test]
fn missing_optional_value_keys_default_to_none() {
    let payload = json!({
        "summary": "s", "title": "t",
        "deltas": [{"subject": "Kaelen", "field": "xp", "op": "add", "value_num": 10}],
        "new_lore": [], "quest_updates": []
    })
    .to_string();
    let e = parse_extraction(&payload).expect("value_txt is optional");
    assert_eq!(e.deltas[0].value_num, Some(10));
    assert_eq!(e.deltas[0].value_txt, None);
}

#[test]
fn a_fenced_code_block_is_tolerated() {
    let payload = format!("```json\n{}\n```", good_payload());
    let e = parse_extraction(&payload).expect("fences must not cost us a chapter's state");
    assert_eq!(e.deltas.len(), 3);
}

#[test]
fn leading_chatter_before_the_object_is_tolerated() {
    let payload = format!("Sure! Here is the extraction:\n{}", good_payload());
    let e = parse_extraction(&payload).expect("prose preamble must not defeat extraction");
    assert_eq!(e.deltas.len(), 3);
}

// ---------------------------------------------------------------------------
// Failure modes -- the engine must be able to tell these apart (spec §10)
// ---------------------------------------------------------------------------

#[test]
fn broken_json_is_a_malformed_error_not_a_panic() {
    let err = parse_extraction("{\"summary\": \"truncated mid-fli")
        .expect_err("truncated JSON must not parse");
    assert!(err.is_malformed(), "got {err:?}");
    assert!(
        !err.is_transport(),
        "the engine must not treat a bad generation as an outage and back off"
    );
}

#[test]
fn valid_json_with_the_wrong_shape_is_malformed() {
    let err =
        parse_extraction(&json!({"deltas": []}).to_string()).expect_err("summary is required");
    assert!(err.is_malformed());
}

/// An empty summary is the story losing its memory: §6.3 forbids feeding previous chapters
/// back verbatim, so summaries are the *only* long-range context there is. Measured live —
/// adding `title` to the schema made the model start returning a good title and an empty
/// summary, which every other layer would have reported as a success.
#[test]
fn an_empty_summary_is_malformed_so_the_retry_machinery_engages() {
    for empty in ["", "   ", "\n\t "] {
        let payload = json!({
            "summary": empty, "title": "T",
            "deltas": [], "new_lore": [], "quest_updates": []
        })
        .to_string();
        let err =
            parse_extraction(&payload).expect_err("an empty summary must not parse as success");
        assert!(err.is_malformed(), "got {err:?}");
        assert!(
            format!("{err}").contains("long-range context"),
            "the error should say why it matters: {err}"
        );
    }
}

/// Same loophole as the summary: `title` is required, but `""` satisfies a required string, and
/// `derive_title`'s `Chapter N` fallback would quietly absorb it.
#[test]
fn an_empty_title_is_malformed_too() {
    for empty in ["", "  ", "\n"] {
        let payload = json!({
            "summary": "Something happened.", "title": empty,
            "deltas": [], "speakers": [], "new_lore": [], "quest_updates": []
        })
        .to_string();
        let err = parse_extraction(&payload).expect_err("an empty title must not parse");
        assert!(err.is_malformed(), "got {err:?}");
        assert!(format!("{err}").contains("title"), "{err}");
    }
}

#[test]
fn the_schema_asks_for_every_speaker_not_only_new_ones() {
    // `new_lore` means "newly introduced", so the protagonist and anyone named in the premise
    // are never in it -- the one gender hint the engine needs is the one it would never get.
    let s: Value = serde_json::from_str(EXTRACTION_SCHEMA).unwrap();
    let props = &s["properties"]["speakers"]["items"]["properties"];
    assert!(props.get("name").is_some());

    let genders: Vec<&str> = props["gender"]["enum"]
        .as_array()
        .expect("gender must be an enum")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(genders, vec!["male", "female", "neutral"]);

    // `name` is required; `gender` is not, so an omitted hint degrades rather than failing.
    let required: Vec<&str> = s["properties"]["speakers"]["items"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, vec!["name"], "gender must stay optional");

    let desc = s["properties"]["speakers"]["description"].as_str().unwrap();
    assert!(
        desc.contains("describing, not inventing"),
        "the summary fix's distinction applies here too: {desc}"
    );
}

#[test]
fn speaker_gender_hints_normalise_and_reject_nonsense() {
    let sp = |g: Option<&str>| ProposedSpeaker {
        name: "Kaelen".into(),
        gender: g.map(str::to_string),
    };
    assert_eq!(sp(Some("male")).gender_hint(), Some("male"));
    assert_eq!(sp(Some("  FEMALE ")).gender_hint(), Some("female"));
    assert_eq!(sp(Some("Neutral")).gender_hint(), Some("neutral"));
    assert_eq!(sp(Some("wizard")).gender_hint(), None);
    assert_eq!(sp(Some("")).gender_hint(), None);
    assert_eq!(sp(None).gender_hint(), None);
}

#[test]
fn a_payload_without_speakers_still_parses() {
    // Strictly additive: an older or terser payload must not lose its whole extraction.
    let payload = json!({
        "summary": "s", "title": "t", "deltas": [], "new_lore": [], "quest_updates": []
    })
    .to_string();
    assert!(parse_extraction(&payload).unwrap().speakers.is_empty());
}

#[test]
fn speakers_deserialize_with_and_without_gender() {
    let payload = json!({
        "summary": "s", "title": "t", "deltas": [], "new_lore": [], "quest_updates": [],
        "speakers": [
            {"name": "Kaelen", "gender": "male"},
            {"name": "Sera"},
            {"name": "Ilex", "gender": null}
        ]
    })
    .to_string();
    let e = parse_extraction(&payload).unwrap();
    assert_eq!(e.speakers.len(), 3);
    assert_eq!(e.speakers[0].gender_hint(), Some("male"));
    assert_eq!(e.speakers[1].gender_hint(), None);
    assert_eq!(e.speakers[2].gender_hint(), None);
}

#[test]
fn a_present_summary_still_parses() {
    let payload = json!({
        "summary": "Kaelen broke the seal.", "title": "T",
        "deltas": [], "new_lore": [], "quest_updates": []
    })
    .to_string();
    assert_eq!(
        parse_extraction(&payload).unwrap().summary,
        "Kaelen broke the seal."
    );
}

#[test]
fn empty_output_is_malformed() {
    assert!(parse_extraction("").expect_err("empty").is_malformed());
    assert!(
        parse_extraction("   \n ")
            .expect_err("blank")
            .is_malformed()
    );
}

#[test]
fn the_malformed_error_keeps_the_body_so_the_failure_is_diagnosable() {
    let err = parse_extraction("not json at all").expect_err("bad");
    let shown = format!("{err}");
    assert!(
        shown.contains("not json at all") || format!("{err:?}").contains("not json at all"),
        "a schema failure at chapter 60 is unfixable without the offending body: {shown}"
    );
}

// ---------------------------------------------------------------------------
// Mapping onto litrpg_core
// ---------------------------------------------------------------------------

#[test]
fn proposed_deltas_map_onto_the_core_delta_type() {
    let e = parse_extraction(&good_payload()).unwrap();
    let deltas = e.to_deltas().expect("all three ops are legal");

    assert_eq!(deltas.len(), 3);
    assert_eq!(deltas[0].subject, "Kaelen");
    assert_eq!(deltas[0].field, "xp");
    assert_eq!(deltas[0].op, Op::Add);
    assert_eq!(deltas[0].value_num, Some(150));
    assert_eq!(deltas[1].op, Op::Sub);
    assert_eq!(deltas[2].op, Op::Set);
    assert_eq!(deltas[2].value_txt.as_deref(), Some("Ashen Vale"));
}

#[test]
fn op_mapping_is_case_insensitive() {
    for raw in ["set", "SET", "Set"] {
        let d = ProposedDelta {
            subject: "Kaelen".into(),
            field: "hp".into(),
            op: raw.into(),
            value_num: Some(1),
            value_txt: None,
        };
        assert_eq!(d.to_delta().expect("case should not matter").op, Op::Set);
    }
}

#[test]
fn an_unknown_op_is_a_typed_malformed_error() {
    let d = ProposedDelta {
        subject: "Kaelen".into(),
        field: "hp".into(),
        op: "multiply".into(),
        value_num: Some(2),
        value_txt: None,
    };
    let err = d.to_delta().expect_err("multiply is not a ledger op");
    assert!(matches!(err, EmberError::UnknownOp { .. }));
    assert!(err.is_malformed());
}

#[test]
fn to_deltas_reports_the_offending_op_rather_than_dropping_the_delta() {
    let payload = json!({
        "summary": "s", "title": "t",
        "deltas": [
            {"subject": "Kaelen", "field": "xp", "op": "add", "value_num": 10},
            {"subject": "Kaelen", "field": "hp", "op": "multiply", "value_num": 2}
        ],
        "new_lore": [], "quest_updates": []
    })
    .to_string();
    let e = parse_extraction(&payload).unwrap();
    let err = e
        .to_deltas()
        .expect_err("one bad op fails the batch loudly");
    assert!(format!("{err}").contains("multiply"));
}

/// The proposals must be exactly what the store's gate consumes -- no adapter layer.
#[test]
fn produced_deltas_feed_the_core_validation_gate_unchanged() {
    let snap = litrpg_core::fold(&[litrpg_core::LedgerEntry {
        seq: 1,
        chapter: 1,
        subject: "Kaelen".into(),
        field: "max_hp".into(),
        op: Op::Set,
        value_num: Some(60),
        value_txt: None,
        applied: true,
    }]);
    let known: std::collections::BTreeSet<String> = ["Kaelen".to_string()].into_iter().collect();

    let deltas = parse_extraction(&good_payload())
        .unwrap()
        .to_deltas()
        .unwrap();
    for d in &deltas {
        // The point is that this call compiles and runs on our output as-is.
        let _ = validate_delta(&snap, &known, d);
    }

    let bad = ProposedDelta {
        subject: "Kaelen".into(),
        field: "mana".into(),
        op: "set".into(),
        value_num: Some(45),
        value_txt: None,
    }
    .to_delta()
    .unwrap();
    assert!(
        validate_delta(&snap, &known, &bad).is_err(),
        "Ember really does emit `Mana: 45/100`; the gate must be the thing that says no"
    );
}
