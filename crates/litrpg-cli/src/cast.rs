//! `litrpg cast` — list speaker → voice assignments, or override one.

use litrpg_core::VoiceRef;
use litrpg_store::Store;
use serde::Serialize;

use crate::{CliError, Result};

/// Default `kind` for a deliberately-added cast member (§6.0 allows
/// `narrator` | `character` | `system`).
pub const DEFAULT_KIND: &str = "character";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CastEntry {
    pub speaker: String,
    pub voice_ref: String,
    pub kind: String,
    pub first_chapter: u32,
    /// `voice_ref` split on the first colon only — Azure voice names contain colons.
    pub backend: Option<String>,
}

pub fn list(store: &Store) -> Result<Vec<CastEntry>> {
    Ok(store
        .cast()?
        .into_iter()
        .map(|c| CastEntry {
            backend: VoiceRef::parse(&c.voice_ref).ok().map(|v| v.backend),
            speaker: c.speaker,
            voice_ref: c.voice_ref,
            kind: c.kind,
            first_chapter: c.first_chapter,
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum CastSetOutcome {
    Overridden {
        speaker: String,
        from: String,
        to: String,
    },
    Added {
        speaker: String,
        voice_ref: String,
        kind: String,
        first_chapter: u32,
    },
}

/// Assign a voice to a speaker.
///
/// An unknown speaker is refused unless `allow_new` is set. This is deliberate:
/// `cast` rows feed `Store::known_subjects`, so silently creating one from a
/// mistyped name would hand the validation gate a subject it should have rejected —
/// the same ghost-character failure the `applied = 1` filter closes on the ledger
/// side. Requiring `--new` keeps a typo from becoming canon.
pub fn set(
    store: &Store,
    speaker: &str,
    voice_ref: &str,
    allow_new: bool,
    kind: Option<&str>,
) -> Result<CastSetOutcome> {
    // Validate before writing, so a bad ref never reaches the database.
    VoiceRef::parse(voice_ref).map_err(|e| CliError::BadVoiceRef {
        got: voice_ref.to_string(),
        reason: e.to_string(),
    })?;

    let existing = store.cast()?.into_iter().find(|c| c.speaker == speaker);

    match existing {
        Some(prev) => {
            store.upsert_cast(speaker, voice_ref, &prev.kind, prev.first_chapter)?;
            Ok(CastSetOutcome::Overridden {
                speaker: speaker.to_string(),
                from: prev.voice_ref,
                to: voice_ref.to_string(),
            })
        }
        None if allow_new => {
            // A pre-assigned voice belongs to the chapter that has not been written
            // yet, hence latest + 1.
            let first_chapter = store.latest_number()?.saturating_add(1);
            let kind = kind.unwrap_or(DEFAULT_KIND);
            store.upsert_cast(speaker, voice_ref, kind, first_chapter)?;
            Ok(CastSetOutcome::Added {
                speaker: speaker.to_string(),
                voice_ref: voice_ref.to_string(),
                kind: kind.to_string(),
                first_chapter,
            })
        }
        None => Err(CliError::UnknownSpeaker {
            speaker: speaker.to_string(),
        }),
    }
}

pub fn render_list(entries: &[CastEntry]) -> String {
    if entries.is_empty() {
        return "No cast members yet.\n".to_string();
    }
    let mut out = format!(
        "{:<20} {:<40} {:<10} {}\n",
        "SPEAKER", "VOICE_REF", "KIND", "FIRST_CH"
    );
    for e in entries {
        out.push_str(&format!(
            "{:<20} {:<40} {:<10} {}\n",
            e.speaker, e.voice_ref, e.kind, e.first_chapter
        ));
    }
    out
}

pub fn render_set(o: &CastSetOutcome) -> String {
    match o {
        CastSetOutcome::Overridden { speaker, from, to } => format!(
            "{speaker}: {from} -> {to}\n\nExisting audio keeps the old voice; re-render affected chapters to apply it.\n"
        ),
        CastSetOutcome::Added {
            speaker,
            voice_ref,
            kind,
            first_chapter,
        } => format!(
            "Added {speaker} ({kind}, first chapter {first_chapter}) with voice {voice_ref}\n"
        ),
    }
}
