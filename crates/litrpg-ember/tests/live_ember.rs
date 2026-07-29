//! Live tests against Ember on `familiar:8091` (spec §11: one `#[ignore]` live test).
//!
//! Run with:
//!   cargo test -p litrpg-ember -- --ignored --nocapture
//!
//! These exist to keep the empirical findings honest. Everything they assert was
//! measured by hand on 2026-07-29 before the implementation was written:
//!
//! * `response_format: {"type":"json_schema", …}` **is** supported and genuinely
//!   constrains the tokens. GBNF `grammar` is not required.
//! * The server converts the schema itself and answers **HTTP 400** on a schema it
//!   cannot compile, so a bad schema is a loud request error, not silent prose.
//! * Qwen3.6 **reasons by default**, and the reasoning is *not* grammar-constrained.
//!   Left on, a constrained call returns `content: ""` with `finish_reason:"length"`.
//!   `chat_template_kwargs.enable_thinking = false` is mandatory.

use litrpg_core::SpeakerKind;
use litrpg_ember::client::{ChatSpec, EmberClient, EmberConfig, Message};
use litrpg_ember::extract::response_format;
use litrpg_ember::parse::parse_tagged_prose;
use litrpg_ember::prompt::{ChapterSummary, Pass1Input, pass1_messages, pass2_messages};
use litrpg_ember::{EmberError, extract};
use serde_json::json;

fn client() -> EmberClient {
    EmberClient::new(EmberConfig::default()).expect("client construction is infallible offline")
}

#[tokio::test]
#[ignore = "requires Ember on familiar:8091"]
async fn ember_is_reachable_and_serves_the_expected_model() {
    let models = client().models().await.expect("GET /v1/models");
    assert!(
        models.iter().any(|m| m == "qwen36-coder"),
        "expected qwen36-coder, got {models:?}"
    );
}

#[tokio::test]
#[ignore = "requires Ember on familiar:8091"]
async fn json_schema_response_format_really_constrains_the_output() {
    // A schema the model would never satisfy by accident. If the grammar were being
    // ignored, the answer to "capital of France" could not possibly come back as this.
    let rf = json!({
        "type": "json_schema",
        "json_schema": {
            "name": "probe",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["xq_zorp_flag"],
                "properties": {
                    "xq_zorp_flag": {"type": "string", "enum": ["ONLY_LEGAL_VALUE"]}
                }
            }
        }
    });

    let spec = ChatSpec::new(vec![Message::user(
        "What is the capital of France? Answer in one word.",
    )])
    .temperature(0.0)
    .max_tokens(200)
    .response_format(rf);

    let out = client().chat(&spec).await.expect("constrained call");
    let v: serde_json::Value = serde_json::from_str(&out.content)
        .unwrap_or_else(|e| panic!("content was not JSON: {e}\n{}", out.content));
    assert_eq!(
        v["xq_zorp_flag"], "ONLY_LEGAL_VALUE",
        "the grammar is not being enforced; got {}",
        out.content
    );
}

#[tokio::test]
#[ignore = "requires Ember on familiar:8091"]
async fn an_uncompilable_schema_is_rejected_with_http_400() {
    let rf = json!({
        "type": "json_schema",
        "json_schema": {"name": "bad", "schema": {"type": "not_a_real_type"}}
    });
    let spec = ChatSpec::new(vec![Message::user("hi")])
        .max_tokens(20)
        .response_format(rf);

    let err = client()
        .chat(&spec)
        .await
        .expect_err("server must reject this");
    match err {
        EmberError::Status { status, ref body } => {
            assert_eq!(status, 400);
            assert!(
                body.contains("schema"),
                "expected a schema-conversion message, got {body}"
            );
        }
        other => panic!("expected Status 400, got {other:?}"),
    }
    assert!(
        !err.is_retryable(),
        "a schema we wrote wrong must not be retried against the buffer"
    );
}

/// The trap, pinned. With reasoning enabled the reasoning pass consumes the token
/// budget and `content` comes back empty -- which is exactly why
/// `EmberConfig::disable_thinking` defaults to true.
#[tokio::test]
#[ignore = "requires Ember on familiar:8091"]
async fn leaving_thinking_enabled_starves_the_constrained_output() {
    let cfg = EmberConfig {
        disable_thinking: false,
        ..EmberConfig::default()
    };
    let c = EmberClient::new(cfg).unwrap();
    let spec = ChatSpec::new(vec![Message::user(
        "Kaelen gained 150 xp and lost 12 hp. Extract summary, deltas, new_lore and quest_updates.",
    )])
    .temperature(0.0)
    .max_tokens(300)
    .response_format(response_format());

    match c.chat(&spec).await {
        Err(EmberError::EmptyContent {
            finish_reason,
            reasoning_chars,
        }) => {
            eprintln!(
                "confirmed: thinking-on starved the output (finish_reason={finish_reason:?}, \
                 {reasoning_chars} chars of reasoning)"
            );
        }
        Err(other) => panic!("expected EmptyContent, got {other:?}"),
        Ok(out) => panic!(
            "this server no longer starves the output -- reasoning defaults may have \
             changed. content={:?}",
            out.content
        ),
    }
}

#[tokio::test]
#[ignore = "requires Ember on familiar:8091"]
async fn pass_2_extraction_round_trips_against_the_live_model() {
    let subjects = ["Kaelen".to_string(), "Sera".to_string()];
    let chapter = "Kaelen swung the Blade of Unpaid Debts and the first seal cracked. \
                   The backlash opened his shoulder; he lost twelve points of health, but the \
                   dead god's ledger credited him one hundred and fifty experience. Sera \
                   watched from the altar in the Ashen Vale and said nothing.";

    let spec = ChatSpec::new(pass2_messages(chapter, &subjects))
        .temperature(0.0)
        .max_tokens(2000)
        .response_format(response_format());

    let out = client().chat(&spec).await.expect("pass 2 call");
    eprintln!("pass 2 raw output:\n{}", out.content);

    let e = extract::parse_extraction(&out.content).expect("schema-constrained output must parse");
    assert!(!e.summary.trim().is_empty(), "summary must not be empty");
    let deltas = e.to_deltas().expect("ops must be legal ledger ops");
    eprintln!("extracted {} deltas: {:#?}", deltas.len(), deltas);
    assert!(
        !deltas.is_empty(),
        "a chapter that states +150 xp and -12 hp should yield deltas"
    );
    assert!(
        deltas
            .iter()
            .all(|d| !d.subject.is_empty() && !d.field.is_empty()),
        "no delta may have an empty subject or field"
    );
}

/// # This test is known to be flaky. Do not treat a failure as a fresh regression.
///
/// It failed once on 2026-07-29 and passed on every run before and after; the assertion was not
/// captured before it went green, so the cause is unknown. Pass 1 is sampled at temperature 0.9
/// against a live model, so **non-determinism is expected by design** — the chapter's shape,
/// speaker count and tag discipline vary run to run.
///
/// What the assertions actually guarantee is only the floor the renderer needs:
///
/// * more than one segment, so the chapter is not one undivided block;
/// * no empty segment, because that would render as silence;
/// * at least one narrator and one character, so the multi-voice cast (D3) is exercised;
/// * no tag markup left in text bound for TTS;
/// * dense, zero-based indices, so a manifest can be built straight from them.
///
/// A failure means the model produced output violating one of those, which is worth *reading*
/// (the raw output is printed above the panic) rather than assuming the parser broke. One
/// observed near-miss: the model satisfied "every chapter needs a `[SYSTEM]` block" with a bare
/// `[SYSTEM]` and no content — handled, and pinned by `parse_table.rs`.
#[tokio::test]
#[ignore = "requires Ember on familiar:8091"]
async fn pass_1_output_parses_into_usable_segments() {
    let summaries = [ChapterSummary {
        chapter: 1,
        body_md: "Kaelen took the contract and entered the Ashen Vale.".into(),
    }];
    let input = Pass1Input {
        chapter_number: 2,
        story_prompt: "Kaelen is a debt-collector for a dead god. Grim, wry, second-person-free \
                       third-person past tense.",
        arc_outline: "Arc 1: break the three seals of the Ashen Vale.",
        state_snapshot: "Kaelen\n  level: 7\n  hp: 41\n  max_hp: 60\n  location: Ashen Vale",
        lore: &[],
        recent_summaries: &summaries,
        director_notes: &["Give Sera one good line.".to_string()],
        target_words: 350,
    };

    let spec = ChatSpec::new(pass1_messages(&input))
        .temperature(0.9)
        .max_tokens(1200);

    let out = client().chat(&spec).await.expect("pass 1 call");
    eprintln!("pass 1 raw output:\n{}", out.content);

    let segs = parse_tagged_prose(&out.content);
    eprintln!(
        "parsed {} segments: {:#?}",
        segs.len(),
        segs.iter()
            .map(|s| (
                s.idx,
                s.speaker.as_str(),
                s.kind,
                s.text.chars().take(60).collect::<String>()
            ))
            .collect::<Vec<_>>()
    );

    assert!(segs.len() >= 2, "a chapter should yield several segments");
    assert!(
        segs.iter().all(|s| !s.text.trim().is_empty()),
        "no segment may be empty -- that would render silence"
    );
    assert!(
        segs.iter().any(|s| s.kind == SpeakerKind::Narrator),
        "every chapter needs narration"
    );
    assert!(
        segs.iter().any(|s| s.kind == SpeakerKind::Character),
        "the prompt asked for dialogue, so a character voice must appear"
    );
    // No tag markup may leak into text destined for TTS.
    for s in &segs {
        assert!(
            !s.text.trim_start().starts_with('['),
            "segment {} still begins with a tag: {:?}",
            s.idx,
            s.text
        );
    }
    // Indices must be dense so the manifest can be built directly from them.
    for (i, s) in segs.iter().enumerate() {
        assert_eq!(s.idx, i as u32);
    }
}
