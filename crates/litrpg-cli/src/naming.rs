//! Comparing character names: the prompt against `story.protagonist`, and subjects
//! against each other.
//!
//! Both checks exist because of one incident. `litrpg init --protagonist "Kaelen"` ran
//! against a prompt calling him "Kaelen Vord", so the model addressed deltas to the
//! full name. Both names became legitimately known subjects, stats split across two
//! keys, and the ledger is append-only so it cannot be merged retrospectively.
//!
//! `story.protagonist` is load-bearing for exactly this reason: it seeds the known-subject
//! set, and the model addresses stat changes to whatever the *prompt* calls the character.
//! When the two disagree, every protagonist delta is either rejected as `UnknownSubject`
//! or filed under a second identity — and both failures are silent.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{Result, io_err};

/// Whether a character is a word character for boundary purposes.
///
/// Alphanumeric only, deliberately. Including `'` would make `Kaelen's blade` fail to
/// match `Kaelen`, which is plainly a mention; excluding it costs a false match of
/// `Brien` inside `O'Brien`, which requires a protagonist named after a name fragment.
/// The first case is common and the second is not.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Byte index of the first whole-word, case-insensitive occurrence of `needle`.
///
/// Whole-word rather than substring: `Kaelen` must not match inside `Kaelendra`, which
/// is a different character. Same reasoning as lore-keyword matching.
pub fn find_whole_word(haystack: &str, needle: &str) -> Option<usize> {
    let needle = needle.trim();
    if needle.is_empty() {
        return None;
    }
    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();

    // Lower-casing can change byte length (e.g. 'İ'), so search the lowered haystack
    // and only report offsets that are valid boundaries in it.
    let mut from = 0usize;
    while let Some(rel) = hay_lower[from..].find(&needle_lower) {
        let start = from + rel;
        let end = start + needle_lower.len();
        let before_ok = hay_lower[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = hay_lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            return Some(start);
        }
        // Advance past this occurrence's first character.
        from = start + hay_lower[start..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// The capitalised word immediately after `end`, when only spaces separate them.
///
/// Used to spot `Kaelen` inside `Kaelen Vord`. A heuristic: it will also fire on
/// `Kaelen The Collector`. That is acceptable because the result is a *suggestion* in a
/// warning, never a refusal — but it is why the wording says "may call them".
fn following_capitalised_word(text: &str, end: usize) -> Option<String> {
    let rest = text.get(end..)?;
    let mut chars = rest.char_indices();
    let mut seen_space = false;
    let (word_start, first) = loop {
        let (i, c) = chars.next()?;
        if c == ' ' || c == '\t' {
            seen_space = true;
            continue;
        }
        if !seen_space {
            return None; // adjacent punctuation, not a name continuation
        }
        break (i, c);
    };
    if !first.is_uppercase() {
        return None;
    }
    let word: String = rest[word_start..]
        .chars()
        .take_while(|c| is_word_char(*c))
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Outcome of comparing `story.protagonist` against the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum ProtagonistCheck {
    /// No protagonist recorded, so there is nothing to compare.
    Unset,
    /// The prompt file could not be read; the check could not run.
    PromptUnreadable { path: PathBuf },
    /// Named in the prompt, and not obviously part of a longer name.
    Named,
    /// Named, but the prompt appears to use a longer form — the incident case.
    NamedWithinLongerName { in_prompt: String },
    /// Absent. `also_absent` lists which words of the name were searched for
    /// individually and *were* found, which catches the mirror mistake: a protagonist
    /// recorded as "Kaelen Vord" against a prompt that only ever says "Kaelen".
    Absent { words_found: Vec<String> },
}

impl ProtagonistCheck {
    /// Whether this warrants telling the operator about.
    pub fn is_warning(&self) -> bool {
        matches!(
            self,
            Self::NamedWithinLongerName { .. }
                | Self::Absent { .. }
                | Self::PromptUnreadable { .. }
        )
    }
}

/// Compare a protagonist name against prompt text. Pure, so it is testable without
/// a filesystem or a database.
pub fn check_protagonist(protagonist: &str, prompt_text: &str) -> ProtagonistCheck {
    let name = protagonist.trim();
    if name.is_empty() {
        return ProtagonistCheck::Unset;
    }

    if let Some(start) = find_whole_word(prompt_text, name) {
        let end = start + name.len();
        // Check the *original* text for capitalisation, not a lowered copy.
        if let Some(next) = prompt_text
            .get(start..)
            .and_then(|_| following_capitalised_word(prompt_text, end))
        {
            return ProtagonistCheck::NamedWithinLongerName {
                in_prompt: format!("{name} {next}"),
            };
        }
        return ProtagonistCheck::Named;
    }

    // Not found whole. Report which individual words of the name do appear, so a
    // protagonist recorded more fully than the prompt uses is still diagnosable.
    let words_found: Vec<String> = name
        .split_whitespace()
        .filter(|w| w.chars().any(is_word_char))
        .filter(|w| find_whole_word(prompt_text, w).is_some())
        .map(str::to_string)
        .collect();
    ProtagonistCheck::Absent { words_found }
}

/// Run the check against a prompt file. A missing or unreadable prompt is reported
/// rather than treated as "absent" — the check did not run, which is different from
/// running and finding nothing.
pub fn check_protagonist_file(protagonist: &str, prompt_path: &Path) -> Result<ProtagonistCheck> {
    if protagonist.trim().is_empty() {
        return Ok(ProtagonistCheck::Unset);
    }
    match std::fs::read_to_string(prompt_path) {
        Ok(text) => Ok(check_protagonist(protagonist, &text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(ProtagonistCheck::PromptUnreadable {
                path: prompt_path.to_path_buf(),
            })
        }
        Err(source) => Err(io_err(prompt_path)(source)),
    }
}

/// The warning to show, or `None` when there is nothing worth saying.
///
/// Every branch states the *consequence* rather than that the situation is unusual:
/// the model addresses stat changes to whatever the prompt calls the character, so a
/// mismatch means they are rejected or filed under a second name.
pub fn warning(check: &ProtagonistCheck, protagonist: &str) -> Option<String> {
    match check {
        ProtagonistCheck::Unset | ProtagonistCheck::Named => None,

        ProtagonistCheck::NamedWithinLongerName { in_prompt } => Some(format!(
            "!! The prompt may call the protagonist {in_prompt:?}, but the story records\n\
             !! {protagonist:?}. The model addresses stat changes to the name the prompt uses,\n\
             !! so they would be filed under a second identity and the character's stats\n\
             !! would split across two names.\n\
             !! Make the two agree. Editing prompt.md to say {protagonist:?} is the safe\n\
             !! direction — `init --force` would rewrite prompt.md to the starter template\n\
             !! and lose the premise.\n"
        )),

        ProtagonistCheck::Absent { words_found } if !words_found.is_empty() => Some(format!(
            "!! The prompt does not name the protagonist {protagonist:?}, though it does\n\
             !! mention {}. The model addresses stat changes to the name the prompt uses,\n\
             !! so recording a different one means they are rejected as UnknownSubject or\n\
             !! filed under a second identity.\n",
            words_found
                .iter()
                .map(|w| format!("{w:?}"))
                .collect::<Vec<_>>()
                .join(" and ")
        )),

        ProtagonistCheck::Absent { .. } => Some(format!(
            "!! The prompt does not name the protagonist {protagonist:?}. The model addresses\n\
             !! stat changes to the name the prompt uses, so they would be rejected as\n\
             !! UnknownSubject. This is fine if the premise deliberately leaves them unnamed\n\
             !! and lets the model choose — otherwise name them in the prompt, or record the\n\
             !! name the prompt uses.\n"
        )),

        ProtagonistCheck::PromptUnreadable { path } => Some(format!(
            "!! Could not read {} to check the protagonist's name against it.\n",
            path.display()
        )),
    }
}

/// Subject pairs where one name is a whole-word prefix of the other, suggesting one
/// character recorded under two identities.
///
/// A heuristic, and deliberately a narrow one: only a *leading* whole-word containment
/// counts, so `Kaelen` / `Kaelen Vord` is flagged while `Vessa` / `Mara` is not. It will
/// occasionally flag two genuinely distinct characters (`Vessa` and `Vessa the Elder`),
/// which is why the report says "may be" and suggests nothing destructive.
pub fn possible_aliases(subjects: &[String]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (i, a) in subjects.iter().enumerate() {
        for b in subjects.iter().skip(i + 1) {
            let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
            if short == long {
                continue;
            }
            // Leading containment on a word boundary: "Kaelen" starts "Kaelen Vord".
            if find_whole_word(long, short) == Some(0) {
                pairs.push((short.clone(), long.clone()));
            }
        }
    }
    pairs
}
