//! `litrpg state [subject]` — print the folded ledger snapshot.
//!
//! The single-subject view mirrors the watch's character screen (§9.4.1): the
//! whitelisted equipment slots and appearance traits are listed in the order
//! `litrpg-core` declares them, including the empty ones, so the layout is stable
//! between invocations exactly as it is on the watch.

use std::collections::BTreeMap;

use litrpg_core::ledger::Value;
use litrpg_core::validate::{
    APPEAR_PREFIX, APPEAR_TRAITS, EQUIP_PREFIX, EQUIP_SLOTS, INVENTORY_PREFIX, NUMERIC_FIELDS,
    TEXT_FIELDS,
};
use litrpg_store::{Result, Store};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SubjectView {
    pub subject: String,
    /// Whitelisted numeric fields (`hp`, `level`, …) that have a value.
    pub stats: BTreeMap<String, i64>,
    /// `inv:<item>` counts, keyed by bare item name.
    pub inventory: BTreeMap<String, i64>,
    /// `equip:<slot>`, keyed by bare slot name.
    pub equipment: BTreeMap<String, String>,
    /// `appear:<trait>`, keyed by bare trait name.
    pub appearance: BTreeMap<String, String>,
    /// `location`, `status`.
    pub text_fields: BTreeMap<String, String>,
    /// Anything the fold recorded that no whitelist covers. Surfaced rather than
    /// dropped: an unexpected field here means the gate's whitelist and the
    /// prompt have diverged.
    pub other: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateReport {
    pub subjects: Vec<SubjectView>,
    /// From `StateSnapshot::anomalies` — malformed-but-applied ledger rows.
    pub anomalies: Vec<String>,
    /// Subject pairs that may be one character recorded under two names.
    ///
    /// Reported in `state` rather than `cast` on purpose: `cast` maps speakers to
    /// voices, while this is about ledger *subjects*, and `state` is the view where the
    /// symptom actually shows — a character sheet that is half empty because the other
    /// half is filed under a second name.
    pub possible_aliases: Vec<(String, String)>,
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Num(n) => n.to_string(),
        Value::Txt(t) => t.clone(),
    }
}

/// Fold the ledger and bucket each `(subject, field)` into its namespace.
pub fn state(store: &Store, subject: Option<&str>) -> Result<StateReport> {
    let snap = store.snapshot()?;
    let mut by_subject: BTreeMap<String, SubjectView> = BTreeMap::new();

    for ((subj, field), value) in &snap.values {
        if let Some(want) = subject
            && subj != want
        {
            continue;
        }
        let view = by_subject
            .entry(subj.clone())
            .or_insert_with(|| SubjectView {
                subject: subj.clone(),
                ..Default::default()
            });

        if let Some(item) = field.strip_prefix(INVENTORY_PREFIX) {
            if let Value::Num(n) = value {
                view.inventory.insert(item.to_string(), *n);
            } else {
                view.other.insert(field.clone(), value_to_string(value));
            }
        } else if let Some(slot) = field.strip_prefix(EQUIP_PREFIX) {
            view.equipment
                .insert(slot.to_string(), value_to_string(value));
        } else if let Some(tr) = field.strip_prefix(APPEAR_PREFIX) {
            view.appearance
                .insert(tr.to_string(), value_to_string(value));
        } else if TEXT_FIELDS.contains(&field.as_str()) {
            view.text_fields
                .insert(field.clone(), value_to_string(value));
        } else if NUMERIC_FIELDS.contains(&field.as_str()) {
            if let Value::Num(n) = value {
                view.stats.insert(field.clone(), *n);
            } else {
                view.other.insert(field.clone(), value_to_string(value));
            }
        } else {
            view.other.insert(field.clone(), value_to_string(value));
        }
    }

    // Computed over every subject in the snapshot, not just the filtered view: asking
    // for one character should still surface that their stats are split.
    let all_subjects: Vec<String> = snap
        .values
        .keys()
        .map(|(s, _)| s.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(StateReport {
        subjects: by_subject.into_values().collect(),
        anomalies: snap.anomalies.clone(),
        possible_aliases: crate::naming::possible_aliases(&all_subjects),
    })
}

/// Overview: one line per subject.
pub fn render_all(r: &StateReport) -> String {
    let mut out = String::new();
    render_anomalies(&mut out, r);
    render_aliases(&mut out, r);
    if r.subjects.is_empty() {
        out.push_str("No state recorded yet.\n");
    }
    for s in &r.subjects {
        let hp = match (s.stats.get("hp"), s.stats.get("max_hp")) {
            (Some(hp), Some(max)) => format!("hp {hp}/{max}"),
            (Some(hp), None) => format!("hp {hp}"),
            _ => "hp —".to_string(),
        };
        let level = s
            .stats
            .get("level")
            .map(|l| format!("lvl {l}"))
            .unwrap_or_else(|| "lvl —".to_string());
        let where_ = s
            .text_fields
            .get("location")
            .cloned()
            .unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "{:<20} {:<12} {:<10} {}\n",
            s.subject, hp, level, where_
        ));
    }
    out
}

/// Single-subject view — the same rows the watch shows (§9.4.1).
pub fn render_subject(r: &StateReport, subject: &str) -> String {
    let mut out = String::new();
    render_anomalies(&mut out, r);
    render_aliases(&mut out, r);
    let Some(s) = r.subjects.iter().find(|s| s.subject == subject) else {
        out.push_str(&format!("No state recorded for {subject:?}.\n"));
        return out;
    };

    out.push_str(&format!("{}\n", s.subject));

    out.push_str("\n  Stats\n");
    if s.stats.is_empty() {
        out.push_str("    —\n");
    }
    // Declared order, not alphabetical: the watch's rows do not reshuffle.
    for field in NUMERIC_FIELDS {
        if let Some(v) = s.stats.get(*field) {
            out.push_str(&format!("    {field:<10} {v}\n"));
        }
    }
    for field in TEXT_FIELDS {
        if let Some(v) = s.text_fields.get(*field) {
            out.push_str(&format!("    {field:<10} {v}\n"));
        }
    }

    out.push_str("\n  Equipment\n");
    for slot in EQUIP_SLOTS {
        let v = s.equipment.get(*slot).map(String::as_str).unwrap_or("");
        let shown = if v.is_empty() { "—" } else { v };
        out.push_str(&format!("    {slot:<10} {shown}\n"));
    }

    out.push_str("\n  Appearance\n");
    for tr in APPEAR_TRAITS {
        let v = s.appearance.get(*tr).map(String::as_str).unwrap_or("");
        let shown = if v.is_empty() { "—" } else { v };
        out.push_str(&format!("    {tr:<10} {shown}\n"));
    }

    if !s.inventory.is_empty() {
        out.push_str("\n  Inventory\n");
        for (item, n) in &s.inventory {
            out.push_str(&format!("    {n:>5}  {item}\n"));
        }
    }

    if !s.other.is_empty() {
        out.push_str("\n  Unrecognized fields (whitelist and prompt have diverged)\n");
        for (field, v) in &s.other {
            out.push_str(&format!("    {field:<14} {v}\n"));
        }
    }

    out
}

/// Anomalies are rendered **first**, above the state itself.
///
/// `litrpg-store` reports ledger rows it could not decode through this same
/// channel, so it is the only place an operator learns their database contains a
/// row nothing can read. Printed after the character sheet it would be scrolled
/// past; the whole point is that it is not normal and should stop you.
/// Split-identity notice. After the anomaly banner but before the state, because it
/// changes how every number below should be read.
fn render_aliases(out: &mut String, r: &StateReport) {
    if r.possible_aliases.is_empty() {
        return;
    }
    out.push_str(
        "!! These subjects may be one character recorded under two names, in which case\n\
         !! the stats below are split between them and neither sheet is complete:\n",
    );
    for (short, long) in &r.possible_aliases {
        out.push_str(&format!("!!   {short:?} and {long:?}\n"));
    }
    out.push_str(
        "!! The ledger is append-only, so this cannot be merged retrospectively. Going\n\
         !! forward, name the character the same way in prompt.md and story.protagonist.\n\n",
    );
}

fn render_anomalies(out: &mut String, r: &StateReport) {
    if r.anomalies.is_empty() {
        return;
    }
    out.push_str(&format!(
        "!! {} ANOMAL{} — the fold could not interpret these ledger rows.\n\
         !! Undecodable rows are skipped, so the state below is incomplete.\n",
        r.anomalies.len(),
        if r.anomalies.len() == 1 { "Y" } else { "IES" },
    ));
    for a in &r.anomalies {
        out.push_str(&format!("!!   {a}\n"));
    }
    out.push('\n');
}
