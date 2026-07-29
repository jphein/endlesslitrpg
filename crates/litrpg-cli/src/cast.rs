//! `litrpg cast` — list speaker → voice assignments, or override one.

use std::collections::BTreeMap;

use litrpg_core::VoiceRef;
use litrpg_store::Store;
use serde::Serialize;

use crate::{CliError, Result};

/// How many rendered chapters back to look when detecting voice substitution.
///
/// Bounded because this is one query per chapter and a long-running story has
/// hundreds. A speaker absent from the window is reported as "not seen recently"
/// rather than as agreeing — silence is not evidence.
pub const SUBSTITUTION_SCAN_LIMIT: usize = 20;

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
    /// The voice this speaker was *actually* rendered with most recently, when it
    /// differs from the cast row.
    ///
    /// An Azure-only build substitutes at render time without rewriting the cast row
    /// — deliberately, since the story's history should not depend on which binary
    /// ran. The consequence is that `litrpg cast` can show a `sherpa:` ref for audio
    /// produced by Azure, which reads as a bug to anyone comparing against a
    /// manifest. Reporting the observed divergence turns that into information.
    ///
    /// `None` means either agreement or no rendered appearance in the scan window;
    /// `scanned` on [`CastListing`] distinguishes those.
    pub rendered_as: Option<String>,
}

/// A cast listing plus what was actually observed in rendered audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CastListing {
    pub entries: Vec<CastEntry>,
    /// Chapter numbers scanned for rendered voices, newest first.
    pub scanned: Vec<u32>,
}

impl CastListing {
    pub fn substituted(&self) -> impl Iterator<Item = &CastEntry> {
        self.entries.iter().filter(|e| e.rendered_as.is_some())
    }
}

/// The voice each speaker was last rendered with, newest rendered chapter winning.
fn last_rendered_voices(
    store: &Store,
    limit: usize,
) -> Result<(BTreeMap<String, String>, Vec<u32>)> {
    let rendered: Vec<u32> = store
        .chapters_since(0)?
        .into_iter()
        .filter(|c| c.has_audio)
        .map(|c| c.number)
        .collect();

    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut scanned = Vec::new();
    for number in rendered.iter().rev().take(limit) {
        scanned.push(*number);
        for seg in store.segments(*number)? {
            // Newest-first iteration, so the first value wins.
            seen.entry(seg.speaker).or_insert(seg.voice_ref);
        }
    }
    Ok((seen, scanned))
}

pub fn list(store: &Store) -> Result<CastListing> {
    let (rendered_voices, scanned) = last_rendered_voices(store, SUBSTITUTION_SCAN_LIMIT)?;
    let entries = store
        .cast()?
        .into_iter()
        .map(|c| {
            let rendered_as = rendered_voices
                .get(&c.speaker)
                .filter(|used| **used != c.voice_ref)
                .cloned();
            CastEntry {
                backend: VoiceRef::parse(&c.voice_ref).ok().map(|v| v.backend),
                speaker: c.speaker,
                voice_ref: c.voice_ref,
                kind: c.kind,
                first_chapter: c.first_chapter,
                rendered_as,
            }
        })
        .collect();
    Ok(CastListing { entries, scanned })
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

pub fn render_list(listing: &CastListing) -> String {
    if listing.entries.is_empty() {
        return "No cast members yet.\n".to_string();
    }
    let mut out = format!(
        "{:<3}{:<20} {:<40} {:<10} {}\n",
        "", "SPEAKER", "VOICE_REF", "KIND", "FIRST_CH"
    );
    for e in &listing.entries {
        out.push_str(&format!(
            "{:<3}{:<20} {:<40} {:<10} {}\n",
            if e.rendered_as.is_some() { "!!" } else { "" },
            e.speaker,
            e.voice_ref,
            e.kind,
            e.first_chapter
        ));
    }

    let substituted: Vec<&CastEntry> = listing.substituted().collect();
    if !substituted.is_empty() {
        out.push_str(
            "\n!! Rendered with a different voice than the cast records. The cast row is\n\
             !! the story's intent; the substitution came from the build that rendered it,\n\
             !! so audio and this table disagree until re-rendered:\n",
        );
        for e in substituted {
            out.push_str(&format!(
                "!!   {:<20} rendered as {}\n",
                e.speaker,
                e.rendered_as.as_deref().unwrap_or("?")
            ));
        }
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
