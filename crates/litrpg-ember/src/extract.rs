//! Pass 2 — the extraction contract.
//!
//! A `json_schema`-constrained call over the finished chapter returns
//! `{summary, deltas[], new_lore[], quest_updates[]}`, which this module deserializes and
//! maps onto [`litrpg_core::Delta`] and [`litrpg_core::Op`].
//!
//! **Validation is not this crate's job.** The store's gate (§6.2) decides what is
//! acceptable and records rejections with `applied = 0`. This module only produces
//! well-typed *proposals*. Ember really does emit things like `Mana: 45/100` — the gate is
//! the thing that says no, and it says so in an audit trail rather than in a parser.
//!
//! # Structured output on this llama.cpp build (measured 2026-07-29, `b950-555881e`)
//!
//! `response_format: {"type": "json_schema", …}` is supported and **genuinely enforced**:
//! asked for the capital of France under a schema requiring
//! `{"xq_zorp_flag": "ONLY_LEGAL_VALUE"}`, the server returned exactly that. GBNF
//! `grammar` is therefore unnecessary. The server compiles the schema itself and answers
//! **HTTP 400** if it cannot, so a broken [`EXTRACTION_SCHEMA`] fails loudly on the first
//! call rather than degrading into prose.
//!
//! The catch is documented on [`crate::client::EmberConfig::disable_thinking`]: the
//! grammar constrains `content`, not the reasoning pass.

use litrpg_core::{Delta, Op};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::EmberError;

/// The `json_schema.name` sent to the server.
pub const EXTRACTION_SCHEMA_NAME: &str = "chapter_extraction";

/// The pass-2 JSON Schema.
///
/// `additionalProperties: false` everywhere is what stops the model bolting on a
/// plausible-looking field the engine would then ignore. `op` is an enum so the grammar
/// itself rules out anything but the three ledger operations (§6.0).
///
/// `value_num` / `value_txt` are nullable and *not* required: a text delta has no number
/// and forcing an explicit `null` for both on every entry buys nothing.
///
/// # Generation order is alphabetical, not schema order
///
/// Measured on this build: the keys come back
/// `deltas, new_lore, quest_updates, summary, title` — strict alphabetical order, regardless
/// of the order they appear in here. So reordering `properties` to control what the model
/// "thinks about first" does nothing; the grammar sorts them. Worth knowing before trying
/// it: an earlier attempt to put `title` after `summary` for exactly that reason had no
/// effect at all.
///
/// # Permitted is not requested (2026-07-29)
///
/// `equip:` and `appear:` were legal for six chapters and the model never once used
/// them. Neither did `gold` or `location`. What they had in common was being listed as
/// allowed values rather than asked for: the `field` description was a flat enumeration
/// ending `..., inv:<item>, equip:<slot>, appear:<trait>`, while `speakers` right below
/// it carries a real instruction -- "Reporting who spoke is describing, not inventing" --
/// and speakers came back every chapter.
///
/// So the fix is not a new field, it is asking. `deltas` now says to include what a
/// character wears, carries and looks like, with the same describing-not-inventing
/// framing that made `speakers` reliable, and `field` names the eleven slots and six
/// traits explicitly instead of writing `<slot>`. The slots are whitelisted in
/// `litrpg_core::validate` and anything else is rejected, so naming them costs nothing
/// and removes the guess.
///
/// Note this cannot be fixed by reordering (see above -- generation is alphabetical), so
/// the description is the only lever there is.
///
/// # The same asymmetry, one field to the right (2026-07-29, later)
///
/// Asking for gear worked on the first chapter extracted under it: the model proposed
/// `equip:main_hand` for the protagonist, from "his own hand, resting on the hilt of his
/// sword". It was then REJECTED as `MissingTextValue` -- it named the slot and sent no
/// value.
///
/// `value_txt` said "For text fields. Only valid with op = set." Permitted, not required
/// -- the identical failure shape as the field list, one property along. It now says the
/// value is required, that a slot reported as occupied without saying by what is not a
/// state change, and gives an example for each of `equip:` and `appear:`.
///
/// Worth noting the rejection was more informative than eight chapters of silence: a
/// stored `applied = 0` row with a code says the model is reaching for the field and
/// falling at the last step, where a zero count said only "no gear".
///
/// It happens to land favourably — `title` is alphabetically last, so it is written after
/// the summary and the deltas — but that is luck, not design, and a field named `a_title`
/// would be generated first.
pub const EXTRACTION_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["summary", "title", "deltas", "speakers", "new_lore", "quest_updates"],
  "properties": {
    "summary": {
      "type": "string",
      "description": "Two or three sentences of what happened in the story, for use as long-range context in later chapters. Facts about events, people and places, not atmosphere. Never empty, and never a remark about the extraction itself."
    },
    "title": {
      "type": "string",
      "description": "A short chapter title, at most eight words. Name what happens in the story. Never describe the extraction itself."
    },
    "deltas": {
      "type": "array",
      "description": "State changes the chapter explicitly states. Empty if nothing changed. Include what a character WEARS OR CARRIES and how they LOOK, not only what happens to their numbers -- if the chapter puts a blade in someone's hand or describes their eyes, that is a state the chapter stated and belongs here. Recording it is describing, not inventing.",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["subject", "field", "op"],
        "properties": {
          "subject": {"type": "string", "description": "Character name, spelled exactly as in the known-subjects list."},
          "field": {"type": "string", "description": "One of the legal fields. Numeric: hp, max_hp, level, xp, gold. Text: location, status. Carried: inv:<item name>. Worn or wielded, one of exactly these slots: equip:head, equip:chest, equip:legs, equip:feet, equip:hands, equip:cloak, equip:main_hand, equip:off_hand, equip:amulet, equip:ring1, equip:ring2. Physical description, one of exactly these traits: appear:hair, appear:eyes, appear:skin, appear:build, appear:height, appear:notable. Any other slot or trait name is rejected."},
          "op": {"type": "string", "enum": ["set", "add", "sub"]},
          "value_num": {"type": ["integer", "null"], "description": "For numeric fields. With add or sub this is the magnitude of the change, always positive."},
          "value_txt": {"type": ["string", "null"], "description": "For text fields, and REQUIRED for them -- a text field with no value is refused, not recorded. Only valid with op = set. For `equip:<slot>` name the item as the chapter names it (\"a chipped ash-blade\", \"the quarry overseer's tunic\"); for `appear:<trait>` give the description (\"entirely grey\", \"a hand taller than most\"). Reporting a slot as occupied without saying by what is not a state change, and will be rejected."}
        }
      }
    },
    "speakers": {
      "type": "array",
      "description": "Every speaker who appears in this chapter, including the protagonist. Reporting who spoke is describing, not inventing. Give `gender` where the chapter makes it clear and omit it otherwise.",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["name"],
        "properties": {
          "name": {"type": "string", "description": "The character's name, spelled as the chapter spells it. Never `narrator` or `SYSTEM` -- those are voices, not people."},
          "gender": {"type": ["string", "null"], "enum": ["male", "female", "neutral", null], "description": "So the engine can cast a matching voice. A voice once assigned is permanent, so omit rather than guess."}
        }
      }
    },
    "new_lore": {
      "type": "array",
      "description": "Entities the chapter introduced that later chapters will need to recall.",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "kind", "keywords", "body_md"],
        "properties": {
          "name": {"type": "string"},
          "kind": {"type": "string", "enum": ["character", "place", "item", "faction", "rule"]},
          "gender": {"type": ["string", "null"], "enum": ["male", "female", "neutral", null], "description": "For a character only, so the engine can cast a matching voice. Omit or null if the chapter does not make it clear; a guess is worse than nothing."},
          "keywords": {"type": "string", "description": "Comma-separated trigger words. Specific enough not to fire on unrelated chapters."},
          "body_md": {"type": "string"},
          "priority": {"type": "integer", "description": "Higher is injected first. 0 unless it is central."}
        }
      }
    },
    "quest_updates": {
      "type": "array",
      "description": "Quest progress the chapter states.",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "status"],
        "properties": {
          "name": {"type": "string"},
          "status": {"type": "string", "enum": ["started", "advanced", "completed", "failed"]},
          "detail": {"type": ["string", "null"]}
        }
      }
    }
  }
}"#;

/// The `response_format` value for a pass-2 call.
///
/// # Panics
/// If [`EXTRACTION_SCHEMA`] is not valid JSON — a compile-time-constant bug caught by the
/// first unit test in this crate, never at runtime.
pub fn response_format() -> Value {
    let schema: Value = serde_json::from_str(EXTRACTION_SCHEMA)
        .expect("EXTRACTION_SCHEMA is a const and must be valid JSON");
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": EXTRACTION_SCHEMA_NAME,
            "strict": true,
            "schema": schema,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extraction {
    /// Short, story-facing chapter title. Empty when an older payload omitted it; the
    /// engine falls back to `Chapter N`.
    #[serde(default)]
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub deltas: Vec<ProposedDelta>,
    /// Who spoke in this chapter. The general source of gender hints; `new_lore` is the
    /// specific one, for a character the chapter genuinely introduces.
    #[serde(default)]
    pub speakers: Vec<ProposedSpeaker>,
    #[serde(default)]
    pub new_lore: Vec<ProposedLore>,
    #[serde(default)]
    pub quest_updates: Vec<QuestUpdate>,
}

/// A delta as the model wrote it. `op` stays a `String` here on purpose: mapping it
/// ourselves lets an out-of-enum value become a typed error instead of a serde failure
/// that takes the whole chapter's bookkeeping with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedDelta {
    pub subject: String,
    pub field: String,
    pub op: String,
    #[serde(default)]
    pub value_num: Option<i64>,
    #[serde(default)]
    pub value_txt: Option<String>,
}

impl ProposedDelta {
    /// Map onto the core type the validation gate consumes. Case-insensitive on `op`.
    pub fn to_delta(&self) -> Result<Delta, EmberError> {
        let op = match self.op.trim().to_ascii_lowercase().as_str() {
            "set" => Op::Set,
            "add" => Op::Add,
            "sub" => Op::Sub,
            _ => {
                return Err(EmberError::UnknownOp {
                    op: self.op.clone(),
                });
            }
        };

        Ok(Delta {
            subject: self.subject.trim().to_string(),
            field: self.field.trim().to_string(),
            op,
            value_num: self.value_num,
            value_txt: self.value_txt.clone(),
        })
    }
}

/// A proposed `lore` row. `always_on` is absent by design — the model does not get to
/// decide that an entry is injected into every future chapter.
/// A speaker the chapter contained, with an optional gender hint.
///
/// This exists because `new_lore` means "newly *introduced*", and the characters whose voices
/// matter most — the protagonist, anyone named in the premise — are never new. So the one hint
/// the engine needs is the one `new_lore` will never carry. Listing who spoke is something the
/// model can always do from the prose in front of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedSpeaker {
    pub name: String,
    /// `male` | `female` | `neutral`. **A hint, never a contract** — optional in the schema and
    /// here, so a model that omits it degrades to un-gendered casting rather than failing.
    #[serde(default)]
    pub gender: Option<String>,
}

impl ProposedSpeaker {
    /// The normalised gender hint, if the model supplied a recognisable one.
    pub fn gender_hint(&self) -> Option<&'static str> {
        normalise_gender(self.gender.as_deref())
    }
}

/// Map a free-text gender onto the three values the engine understands.
///
/// Anything unrecognised becomes `None`, so a stray value is ignored rather than mis-casting.
fn normalise_gender(g: Option<&str>) -> Option<&'static str> {
    match g?.trim().to_ascii_lowercase().as_str() {
        "male" => Some("male"),
        "female" => Some("female"),
        "neutral" => Some("neutral"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedLore {
    pub name: String,
    pub kind: String,
    pub keywords: String,
    pub body_md: String,
    #[serde(default)]
    pub priority: i32,
    /// `male` | `female` | `neutral`, for a character.
    ///
    /// **A hint, never a contract.** Optional in the schema and `Option` here, so a model that
    /// omits it degrades to un-gendered casting rather than failing an extraction — which
    /// would cost the chapter's whole bookkeeping over a cosmetic detail.
    ///
    /// The engine consumes this to pick a voice and then discards it: the `cast` row's
    /// `voice_ref` is the durable record, so there is nothing to persist and no schema change
    /// in `lore`.
    #[serde(default)]
    pub gender: Option<String>,
}

impl ProposedLore {
    /// The normalised gender hint, if this is a character and the model supplied one.
    pub fn gender_hint(&self) -> Option<&'static str> {
        if !self.kind.eq_ignore_ascii_case("character") {
            return None;
        }
        normalise_gender(self.gender.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestUpdate {
    pub name: String,
    /// `started` | `advanced` | `completed` | `failed`
    pub status: String,
    #[serde(default)]
    pub detail: Option<String>,
}

impl Extraction {
    /// Convert every proposal to a [`Delta`].
    ///
    /// Fails the whole batch on a bad `op` rather than dropping the offender: the grammar
    /// makes an illegal op impossible, so seeing one means the constraint was not applied
    /// and the rest of the output is equally untrustworthy. The engine's answer is a
    /// retry, then `state_dirty = 1` — not a half-applied ledger.
    pub fn to_deltas(&self) -> Result<Vec<Delta>, EmberError> {
        self.deltas.iter().map(ProposedDelta::to_delta).collect()
    }
}

/// Deserialize a pass-2 response body.
///
/// Tolerant of a ```` ```json ```` fence and of a prose preamble, because losing a
/// chapter's bookkeeping to a stray "Sure! Here you go:" would be an absurd way to fail.
/// Not tolerant of anything ambiguous: truncated or wrongly-shaped output is a typed
/// [`EmberError::Malformed`] so the engine can retry with temperature jitter and, failing
/// that, ship the chapter with `state_dirty = 1` (§10).
pub fn parse_extraction(content: &str) -> Result<Extraction, EmberError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(EmberError::Malformed {
            body: content.to_string(),
            detail: "response body was empty".to_string(),
        });
    }

    let span = json_object_span(trimmed).ok_or_else(|| EmberError::Malformed {
        body: content.to_string(),
        detail: "no complete JSON object found (truncated output?)".to_string(),
    })?;

    let parsed: Extraction = serde_json::from_str(span).map_err(|e| EmberError::Malformed {
        body: content.to_string(),
        detail: e.to_string(),
    })?;

    // Measured live: with `title` added to the schema, the model started returning a good title
    // and `"summary": ""`. That is worse than it looks. Summaries are the *only* long-range
    // context the story ever gets — §6.3 forbids feeding previous chapters back verbatim — so an
    // empty one means the next chapter has no memory of this one, silently. Treating it as
    // malformed routes it into the existing temperature-jitter retry instead.
    if parsed.summary.trim().is_empty() {
        return Err(EmberError::Malformed {
            body: content.to_string(),
            detail: "summary was empty; it is the only long-range context the story has"
                .to_string(),
        });
    }

    // The same loophole: `title` is required, but an empty string satisfies a required string.
    // Left alone, `derive_title`'s `Chapter N` fallback would mask a retry-worthy generation —
    // that fallback exists for the `state_dirty` case, not to paper over an extraction that
    // otherwise succeeded.
    if parsed.title.trim().is_empty() {
        return Err(EmberError::Malformed {
            body: content.to_string(),
            detail: "title was empty; the schema requires one and readers see it".to_string(),
        });
    }

    Ok(parsed)
}

/// The first complete, brace-balanced JSON object in `s`.
///
/// String-aware, so a `{` or `}` inside a quoted value cannot throw the depth count off —
/// which matters because chapter summaries contain braces about as often as they contain
/// anything else, and a naive `rfind('}')` would truncate at the wrong place.
fn json_object_span(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for i in start..bytes.len() {
        let c = bytes[i];

        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }

        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }

    // Ran out of input with braces still open: the generation was cut short.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object_span_ignores_braces_inside_strings() {
        let s = r#"{"summary": "he said {this} and }that{"}"#;
        assert_eq!(json_object_span(s), Some(s));
    }

    #[test]
    fn json_object_span_handles_escaped_quotes() {
        let s = r#"{"summary": "she called it \"the ledger\"}"}"#;
        assert_eq!(json_object_span(s), Some(s));
    }

    #[test]
    fn json_object_span_finds_nested_objects() {
        let s = r#"prefix {"a": {"b": 1}} suffix"#;
        assert_eq!(json_object_span(s), Some(r#"{"a": {"b": 1}}"#));
    }

    #[test]
    fn json_object_span_rejects_an_unbalanced_object() {
        assert_eq!(json_object_span(r#"{"summary": "cut off"#), None);
        assert_eq!(json_object_span("no object here"), None);
    }

    #[test]
    fn the_schema_const_parses() {
        let _: Value = serde_json::from_str(EXTRACTION_SCHEMA).expect("valid JSON");
    }
}
