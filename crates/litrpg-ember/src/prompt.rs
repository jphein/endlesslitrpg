//! Prompt assembly. Pure functions, no I/O, no network.
//!
//! Keeping this module free of I/O is what makes the interesting part — *what Ember is
//! told* — a plain unit test instead of an integration test that needs a GPU.
//!
//! Two rules from the spec are enforced structurally rather than by discipline:
//!
//! * **§6.3 — previous chapters are never fed back verbatim.** [`Pass1Input`] has no
//!   field for chapter prose. Only [`ChapterSummary`] gets in. Raw prose in context makes
//!   the model mimic its own recent phrasing until the story collapses into a loop, so
//!   the type refuses what discipline would eventually forget.
//! * **§6.2 — the field whitelists come from `litrpg_core`.** [`pass2_messages`] builds
//!   its list of legal fields from the validator's own constants, so the prompt cannot
//!   drift away from the gate that judges its output.

use litrpg_core::validate::{
    APPEAR_PREFIX, APPEAR_TRAITS, EQUIP_PREFIX, EQUIP_SLOTS, INVENTORY_PREFIX, NUMERIC_FIELDS,
    TEXT_FIELDS,
};
use litrpg_core::{StateSnapshot, Value};

use crate::client::Message;

/// Spec §6.0 — ≈13 minutes of audio at ~150 wpm.
pub const DEFAULT_TARGET_WORDS: u32 = 2000;

/// A `lore` row (spec §6). Mirrors the table so the store can hand these over directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoreEntry {
    pub name: String,
    /// `character` | `place` | `item` | `faction` | `rule`
    pub kind: String,
    /// Comma-separated. An entry is injected when any keyword appears in the scan text.
    pub keywords: String,
    pub body_md: String,
    pub priority: i32,
    pub always_on: bool,
}

/// A `summaries` row at level 0 (spec §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterSummary {
    pub chapter: u32,
    pub body_md: String,
}

/// Everything pass 1 is allowed to see. Note the absence of any chapter-prose field.
#[derive(Debug, Clone)]
pub struct Pass1Input<'a> {
    pub chapter_number: u32,
    pub story_prompt: &'a str,
    pub arc_outline: &'a str,
    /// Rendered by [`render_state_snapshot`] from the ledger fold.
    pub state_snapshot: &'a str,
    /// Already selected, usually by [`match_lore`].
    pub lore: &'a [&'a LoreEntry],
    pub recent_summaries: &'a [ChapterSummary],
    pub director_notes: &'a [String],
    pub target_words: u32,
}

/// Select lore entries for injection (spec §6.3, the SillyTavern/KoboldAI pattern).
///
/// An entry matches when any of its comma-separated keywords occurs, case-insensitively,
/// in `scan_text` (normally the last chapter's text plus the arc outline). `always_on`
/// entries always match. Results are ordered by descending `priority`, then by name so
/// that identical state produces an identical prompt — which matters because
/// `chapters.prompt_hash` is the provenance column (§9.3).
///
/// Matching is plain substring containment, as specified. That means a keyword of `ash`
/// also fires on `cash`; keep keywords specific.
pub fn match_lore<'a>(entries: &'a [LoreEntry], scan_text: &str) -> Vec<&'a LoreEntry> {
    let scan = scan_text.to_lowercase();

    let mut hits: Vec<&LoreEntry> = entries
        .iter()
        .filter(|e| e.always_on || keywords_of(e).any(|k| scan.contains(&k.to_lowercase())))
        .collect();

    hits.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.name.cmp(&b.name))
    });
    hits
}

/// Non-empty, trimmed keywords. A trailing comma must not yield an empty keyword —
/// `"".contains()` is always true, which would inject every entry into every chapter.
fn keywords_of(e: &LoreEntry) -> impl Iterator<Item = &str> {
    e.keywords
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
}

/// Render the ledger fold into the block that goes in the prompt.
///
/// Grouped by subject, with inventory, equipment and appearance broken out so the model
/// sees the same shape the watch's character screen shows (§9.4.1).
pub fn render_state_snapshot(snap: &StateSnapshot) -> String {
    if snap.values.is_empty() {
        // Never leave this section blank: an empty heading invites the model to invent
        // numbers, and invented numbers are exactly what the ledger exists to prevent.
        return "(no state recorded yet — this is the opening chapter. Establish starting \
                values in prose and let pass 2 record them; do not assume prior history.)"
            .to_string();
    }

    // `values` is a BTreeMap keyed by (subject, field), so iteration is already grouped
    // by subject and sorted by field — stable output for free.
    let mut out = String::new();
    let mut current_subject: Option<&str> = None;
    let mut inventory: Vec<String> = Vec::new();
    let mut equipment: Vec<String> = Vec::new();
    let mut appearance: Vec<String> = Vec::new();

    for ((subject, field), value) in &snap.values {
        if current_subject != Some(subject.as_str()) {
            flush_groups(&mut out, &mut inventory, &mut equipment, &mut appearance);
            out.push_str(subject);
            out.push('\n');
            current_subject = Some(subject.as_str());
        }

        let shown = show(value);
        if let Some(item) = field.strip_prefix(INVENTORY_PREFIX) {
            inventory.push(format!("{item} x{shown}"));
        } else if let Some(slot) = field.strip_prefix(EQUIP_PREFIX) {
            equipment.push(format!("{slot} = {shown}"));
        } else if let Some(tr) = field.strip_prefix(APPEAR_PREFIX) {
            appearance.push(format!("{tr} = {shown}"));
        } else {
            out.push_str(&format!("  {field}: {shown}\n"));
        }
    }
    flush_groups(&mut out, &mut inventory, &mut equipment, &mut appearance);

    out
}

fn flush_groups(
    out: &mut String,
    inventory: &mut Vec<String>,
    equipment: &mut Vec<String>,
    appearance: &mut Vec<String>,
) {
    for (label, group) in [
        ("inventory", inventory),
        ("equipped", equipment),
        ("appearance", appearance),
    ] {
        if !group.is_empty() {
            out.push_str(&format!("  {label}: {}\n", group.join(", ")));
            group.clear();
        }
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::Num(n) => n.to_string(),
        Value::Txt(s) => s.clone(),
    }
}

const PASS1_SYSTEM: &str = "\
You are the author of an endless LitRPG serial. You write one chapter per call, in third \
person past tense, and you never address the reader.

OUTPUT FORMAT — this is parsed by a machine, so it is not optional.

Emit only the chapter body as speaker-tagged text. A tag is a name in square brackets at \
the very start of the line:

[narrator] The vale smelled of iron and wet ash.
[Kaelen] \"You brought a sword to a debt collection?\"
[SYSTEM] Quest updated — The Ashen Ledger: 1 of 3 seals broken.

Rules:
- A tag only counts at the start of the line. Brackets in the middle of a line are read \
as ordinary prose.
- Use [narrator] for narration and description, [SYSTEM] for RPG stat blocks, level-ups, \
loot and quest notifications, and [CharacterName] for spoken dialogue.
- Use the same spelling for a character every time. Each distinct name is cast with its \
own voice and that casting is permanent.
- Put a blank line between blocks. After a [SYSTEM] block, start the next line with an \
explicit tag.
- No markdown headings, no chapter number, no title, no commentary about the task, no \
author's note.
- Numbers are the engine's job, not yours: state changes in prose or in a [SYSTEM] block, \
but never contradict the state you were given.";

/// Assemble the creative pass. Empty sections are omitted entirely rather than shipped as
/// dangling headings — an empty "Director notes:" reads to the model as an instruction to
/// invent some.
pub fn pass1_messages(input: &Pass1Input) -> Vec<Message> {
    let mut user = String::new();

    user.push_str(&format!("Write chapter {}.\n\n", input.chapter_number));

    push_section(&mut user, "Story premise", input.story_prompt);
    push_section(&mut user, "Arc outline", input.arc_outline);
    push_section(
        &mut user,
        "Current state (authoritative)",
        input.state_snapshot,
    );

    if !input.lore.is_empty() {
        let body = input
            .lore
            .iter()
            .map(|e| format!("- {} ({}): {}", e.name, e.kind, e.body_md.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        push_section(&mut user, "Relevant lore", &body);
    }

    if !input.recent_summaries.is_empty() {
        let body = input
            .recent_summaries
            .iter()
            .map(|s| format!("- Chapter {}: {}", s.chapter, s.body_md.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        push_section(&mut user, "Recent chapter summaries", &body);
    }

    if !input.director_notes.is_empty() {
        let body = input
            .director_notes
            .iter()
            .map(|n| format!("- {}", n.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        push_section(
            &mut user,
            "Director notes (honour these this chapter)",
            &body,
        );
    }

    user.push_str(&format!(
        "Target length: about {} words. End the chapter at a natural beat — a decision \
         made, a door opened, a price named — not on a cliff-hanger fragment.\n",
        input.target_words
    ));

    vec![Message::system(PASS1_SYSTEM), Message::user(user)]
}

fn push_section(out: &mut String, heading: &str, body: &str) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    out.push_str(heading);
    out.push_str(":\n");
    out.push_str(body);
    out.push_str("\n\n");
}

const PASS2_SYSTEM: &str = "\
You are a bookkeeping extractor for a LitRPG serial. You are given a finished chapter and \
you report what changed, as JSON matching the supplied schema. You invent nothing. If the \
chapter does not state a change, do not report one.";

/// Assemble the extraction pass. Pair with [`crate::extract::response_format`] and
/// temperature 0.
///
/// The legal field list is generated from `litrpg_core`'s own whitelists, so the prompt
/// and the validation gate cannot drift apart (§6.2).
pub fn pass2_messages(chapter_text: &str, known_subjects: &[String]) -> Vec<Message> {
    let subjects = if known_subjects.is_empty() {
        "(none yet — this is the opening chapter, so every subject is new)".to_string()
    } else {
        known_subjects.join(", ")
    };

    let user = format!(
        "Chapter text:\n{chapter}\n\n\
         Known subjects — use these names exactly, and only add a new one if the chapter \
         genuinely introduces a character: {subjects}\n\n\
         Legal fields, and nothing else:\n\
         - numeric: {numeric}\n\
         - text: {text}\n\
         - inventory counts: `{inv}<item name>`, numeric, never negative\n\
         - equipment (set only): `{equip}<slot>` where slot is one of: {slots}\n\
         - appearance (set only): `{appear}<trait>` where trait is one of: {traits}\n\n\
         Operations: `set` writes an absolute value, `add` and `sub` are relative and \
         numeric only.\n\n\
         Report a delta only for a change the chapter actually states. A field outside \
         this list, or a subject that is not a real character, is rejected by the engine \
         and the change is lost — so do not invent either.",
        chapter = chapter_text.trim(),
        subjects = subjects,
        numeric = NUMERIC_FIELDS.join(", "),
        text = TEXT_FIELDS.join(", "),
        inv = INVENTORY_PREFIX,
        equip = EQUIP_PREFIX,
        slots = EQUIP_SLOTS.join(", "),
        appear = APPEAR_PREFIX,
        traits = APPEAR_TRAITS.join(", "),
    );

    vec![Message::system(PASS2_SYSTEM), Message::user(user)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_of_drops_empties_from_a_trailing_comma() {
        let e = LoreEntry {
            name: "x".into(),
            kind: "place".into(),
            keywords: "vale, ,ash,".into(),
            body_md: String::new(),
            priority: 0,
            always_on: false,
        };
        assert_eq!(keywords_of(&e).collect::<Vec<_>>(), vec!["vale", "ash"]);
    }

    #[test]
    fn push_section_skips_blank_bodies() {
        let mut s = String::new();
        push_section(&mut s, "Heading", "   \n ");
        assert!(s.is_empty());
    }
}
