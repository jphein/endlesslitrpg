//! The HTTP wire shape, tested without a network.
//!
//! `build_request_body` and `parse_completion` are pure, so every assumption we
//! measured against `familiar:8091` is pinned by an offline test.

use litrpg_ember::client::{
    ChatSpec, Completion, EmberConfig, Message, Role, build_request_body, parse_completion,
    strip_reasoning_block,
};
use litrpg_ember::{DEFAULT_BASE_URL, DEFAULT_MODEL, EmberError};
use serde_json::json;

fn spec() -> ChatSpec {
    ChatSpec::new(vec![
        Message::system("You are the author."),
        Message::user("Write chapter 1."),
    ])
}

#[test]
fn defaults_point_at_ember_on_familiar() {
    let cfg = EmberConfig::default();
    assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
    assert_eq!(cfg.base_url, "http://familiar:8091");
    assert_eq!(cfg.model, DEFAULT_MODEL);
    assert_eq!(cfg.model, "qwen36-coder");
}

#[test]
fn body_carries_model_messages_temperature_and_max_tokens() {
    let cfg = EmberConfig::default();
    let body = build_request_body(&cfg, &spec().temperature(0.9).max_tokens(3000));

    assert_eq!(body["model"], "qwen36-coder");
    assert_eq!(body["temperature"], 0.9);
    assert_eq!(body["max_tokens"], 3000);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "You are the author.");
    assert_eq!(body["messages"][1]["role"], "user");
}

#[test]
fn roles_serialize_as_the_openai_lowercase_strings() {
    for (role, want) in [
        (Role::System, "system"),
        (Role::User, "user"),
        (Role::Assistant, "assistant"),
    ] {
        let body = build_request_body(
            &EmberConfig::default(),
            &ChatSpec::new(vec![Message::new(role, "x")]),
        );
        assert_eq!(body["messages"][0]["role"], want);
    }
}

/// Measured 2026-07-29: Qwen3.6 reasons by default. Left on, a grammar-constrained
/// call returns `content: ""` with `finish_reason: "length"` because the reasoning
/// pass consumes the whole token budget. Disabling it is not optional.
#[test]
fn thinking_is_disabled_by_default() {
    let cfg = EmberConfig::default();
    assert!(cfg.disable_thinking, "thinking must default to OFF");

    let body = build_request_body(&cfg, &spec());
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false),
        "the request must carry chat_template_kwargs.enable_thinking = false"
    );
}

#[test]
fn thinking_can_be_re_enabled_and_then_the_key_is_absent() {
    let cfg = EmberConfig {
        disable_thinking: false,
        ..EmberConfig::default()
    };
    let body = build_request_body(&cfg, &spec());
    assert!(
        body.get("chat_template_kwargs").is_none(),
        "with thinking on we send no override at all"
    );
}

#[test]
fn response_format_is_omitted_for_an_unconstrained_pass_1_call() {
    let body = build_request_body(&EmberConfig::default(), &spec());
    assert!(
        body.get("response_format").is_none(),
        "pass 1 is unconstrained; sending a null response_format risks a 400"
    );
}

#[test]
fn response_format_is_passed_through_verbatim_when_present() {
    let rf = json!({
        "type": "json_schema",
        "json_schema": {"name": "x", "strict": true, "schema": {"type": "object"}}
    });
    let body = build_request_body(&EmberConfig::default(), &spec().response_format(rf.clone()));
    assert_eq!(body["response_format"], rf);
}

#[test]
fn parse_completion_reads_content_finish_reason_and_usage() {
    let raw = json!({
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "[narrator] Ash fell."}
        }],
        "usage": {"prompt_tokens": 63, "completion_tokens": 300, "total_tokens": 363}
    });

    let c: Completion = parse_completion(&raw).expect("well-formed envelope");
    assert_eq!(c.content, "[narrator] Ash fell.");
    assert_eq!(c.finish_reason.as_deref(), Some("stop"));
    assert_eq!(c.prompt_tokens, 63);
    assert_eq!(c.completion_tokens, 300);
    assert!(!c.truncated());
}

#[test]
fn parse_completion_flags_truncation() {
    let raw = json!({
        "choices": [{"finish_reason": "length", "message": {"content": "half a chap"}}]
    });
    let c = parse_completion(&raw).expect("envelope is fine, the generation is not");
    assert!(
        c.truncated(),
        "finish_reason=length means max_tokens was hit and the text is incomplete"
    );
}

/// The exact failure we measured with thinking left on.
#[test]
fn empty_content_is_its_own_error_not_an_empty_success() {
    let raw = json!({
        "choices": [{
            "finish_reason": "length",
            "message": {"role": "assistant", "content": "", "reasoning_content": "Here's a thinking process..."}
        }]
    });

    let err = parse_completion(&raw).expect_err("empty content must not parse as success");
    match &err {
        EmberError::EmptyContent {
            finish_reason,
            reasoning_chars,
        } => {
            assert_eq!(finish_reason.as_deref(), Some("length"));
            assert!(
                *reasoning_chars > 0,
                "the error should say the reasoning pass ate the budget"
            );
        }
        other => panic!("expected EmptyContent, got {other:?}"),
    }
    assert!(err.is_malformed(), "empty output is a model failure");
    assert!(
        !err.is_transport(),
        "nothing about this is a network problem"
    );
}

#[test]
fn missing_choices_is_a_protocol_error() {
    let err = parse_completion(&json!({"choices": []})).expect_err("no choices");
    assert!(matches!(err, EmberError::Protocol { .. }));

    let err = parse_completion(&json!({"object": "chat.completion"})).expect_err("no choices key");
    assert!(matches!(err, EmberError::Protocol { .. }));
}

#[test]
fn usage_is_optional() {
    let raw = json!({"choices": [{"message": {"content": "ok"}}]});
    let c = parse_completion(&raw).expect("usage block is not required");
    assert_eq!(c.completion_tokens, 0);
}

/// llama.cpp splits reasoning into `reasoning_content`, but a server started with a
/// different `--reasoning-format` inlines `<think>…</think>` instead. Strip it either
/// way rather than feeding "Here's a thinking process" into the TTS renderer.
#[test]
fn inline_think_blocks_are_stripped_from_content() {
    assert_eq!(
        strip_reasoning_block("<think>plan plan plan</think>\n[narrator] Ash fell."),
        "[narrator] Ash fell."
    );
    assert_eq!(strip_reasoning_block("  <think>x</think>  body  "), "body");
    assert_eq!(
        strip_reasoning_block("[narrator] no think block here"),
        "[narrator] no think block here"
    );
    // An unterminated block is left alone: better a visible artifact than losing
    // the whole chapter.
    assert_eq!(
        strip_reasoning_block("<think>never closed and then prose"),
        "<think>never closed and then prose"
    );
}

#[test]
fn error_classification_lets_the_engine_pick_a_recovery_path() {
    // Spec §10: Ember unreachable -> backoff. Malformed -> temp jitter, then ship.
    let transport = EmberError::Transport {
        detail: "connection refused".into(),
    };
    assert!(transport.is_transport());
    assert!(!transport.is_malformed());

    let bad_request = EmberError::Status {
        status: 400,
        body: "JSON schema conversion failed".into(),
    };
    assert!(
        !bad_request.is_retryable(),
        "a 400 means OUR schema is wrong; retrying just burns the buffer"
    );

    let server_down = EmberError::Status {
        status: 503,
        body: "loading model".into(),
    };
    assert!(server_down.is_retryable(), "5xx is worth a backoff");
    assert!(server_down.is_transport());
}

#[test]
fn chat_spec_defaults_are_sane_for_a_creative_pass() {
    let s = ChatSpec::new(vec![Message::user("go")]);
    assert!(
        s.temperature > 0.0,
        "pass 1 is creative; a zero default would be a silent quality regression"
    );
    assert!(
        s.max_tokens >= 4000,
        "a 2000-word chapter needs well over 2000 tokens of headroom"
    );
}
