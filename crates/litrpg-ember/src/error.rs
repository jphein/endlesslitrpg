//! One error type, classified so the engine can pick a recovery path.
//!
//! Spec §10 defines three different degradations and they must not be conflated:
//!
//! | Failure | Behaviour |
//! |---|---|
//! | Ember unreachable | exponential backoff; buffer drains; nothing corrupts |
//! | Pass 1 malformed/empty | 2 retries with temperature jitter, then skip the cycle |
//! | Pass 2 schema failure | chapter **ships anyway** with `state_dirty = 1` |
//!
//! So the engine needs to ask two questions of any failure: *was this the network?*
//! ([`EmberError::is_transport`]) and *was this the model?* ([`EmberError::is_malformed`]).
//! Those are the two predicates this module exists to answer.

use thiserror::Error;

/// How much of an offending body to keep in `Display`. Enough to diagnose a schema
/// drift six months in; short enough not to dump a whole chapter into a log line.
const BODY_EXCERPT: usize = 400;

#[derive(Debug, Error)]
pub enum EmberError {
    /// Could not complete the HTTP exchange: connect refused, DNS, TLS, timeout, or a
    /// body that died mid-read. The daemon should back off and try again.
    #[error("transport failure talking to Ember: {detail}")]
    Transport { detail: String },

    /// Ember answered, but not with 2xx. A `400` is *our* bug — llama.cpp validates the
    /// JSON schema server-side and refuses to compile a bad one — so it must not be
    /// retried. A `5xx`/`429` is worth a backoff.
    #[error("Ember returned HTTP {status}: {}", excerpt(body))]
    Status { status: u16, body: String },

    /// A 2xx response whose envelope did not look like an OpenAI chat completion.
    #[error("unexpected response shape from Ember: {detail}")]
    Protocol { detail: String },

    /// The model produced no visible content. Measured cause: Qwen3.6 reasons by
    /// default and the reasoning pass is *not* grammar-constrained, so it happily
    /// spends the entire `max_tokens` budget thinking and returns `content: ""`.
    /// See [`crate::client::EmberConfig::disable_thinking`].
    #[error(
        "Ember returned empty content (finish_reason={finish_reason:?}, \
         {reasoning_chars} chars of reasoning) — the reasoning pass may have consumed \
         the token budget; check EmberConfig::disable_thinking"
    )]
    EmptyContent {
        finish_reason: Option<String>,
        reasoning_chars: usize,
    },

    /// Content came back, but it is not the shape the schema promised. Retry with
    /// temperature jitter; if pass 2 keeps failing, ship the chapter `state_dirty`.
    #[error(
        "Ember output did not match the extraction schema ({detail}); body was: {}",
        excerpt(body)
    )]
    Malformed { body: String, detail: String },

    /// A delta arrived with an operation outside `set | add | sub`. The grammar should
    /// make this impossible, so seeing it means the constraint was not applied.
    #[error("Ember proposed an unknown ledger op {op:?} (expected set, add or sub)")]
    UnknownOp { op: String },

    /// The HTTP client itself could not be constructed. Not recoverable at runtime.
    #[error("could not build the Ember HTTP client: {detail}")]
    Config { detail: String },
}

impl EmberError {
    /// True when the failure was the network or the server being unwell, so the right
    /// response is to wait rather than to re-prompt.
    pub fn is_transport(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Status { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }

    /// True when Ember answered but the *output* was unusable. The engine's response is
    /// a temperature-jittered retry, and after that `state_dirty = 1` for pass 2.
    pub fn is_malformed(&self) -> bool {
        matches!(
            self,
            Self::EmptyContent { .. } | Self::Malformed { .. } | Self::UnknownOp { .. }
        )
    }

    /// Whether trying the same call again could plausibly succeed.
    ///
    /// Deliberately `false` for a `4xx` other than `429`: a rejected schema or a
    /// malformed request will be rejected identically every time, and retrying it just
    /// burns the rendered-ahead buffer while looking like progress.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Config { .. } => false,
            Self::Status { status, .. } => *status >= 500 || *status == 429,
            _ => true,
        }
    }
}

fn excerpt(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= BODY_EXCERPT {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(BODY_EXCERPT).collect();
    format!("{head}… ({} chars total)", trimmed.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_is_not_malformed_and_vice_versa() {
        let t = EmberError::Transport {
            detail: "connection refused".into(),
        };
        assert!(t.is_transport() && !t.is_malformed() && t.is_retryable());

        let m = EmberError::Malformed {
            body: "{".into(),
            detail: "eof".into(),
        };
        assert!(m.is_malformed() && !m.is_transport() && m.is_retryable());
    }

    #[test]
    fn a_four_hundred_is_never_retried_but_a_five_hundred_is() {
        assert!(
            !EmberError::Status {
                status: 400,
                body: "bad schema".into()
            }
            .is_retryable()
        );
        assert!(
            EmberError::Status {
                status: 500,
                body: "boom".into()
            }
            .is_retryable()
        );
        assert!(
            EmberError::Status {
                status: 429,
                body: "slow down".into()
            }
            .is_retryable()
        );
    }

    #[test]
    fn a_long_body_is_excerpted_not_dumped_whole() {
        let err = EmberError::Malformed {
            body: "x".repeat(5_000),
            detail: "nope".into(),
        };
        let shown = format!("{err}");
        assert!(shown.len() < 1_000, "log lines must stay readable");
        assert!(shown.contains("5000 chars total"));
    }

    #[test]
    fn a_short_body_survives_verbatim() {
        let err = EmberError::Malformed {
            body: "not json at all".into(),
            detail: "expected value".into(),
        };
        assert!(format!("{err}").contains("not json at all"));
    }
}
