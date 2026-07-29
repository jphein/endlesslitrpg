//! The append-only ledger and its fold.
//!
//! Current state is never stored. It is computed by folding
//! `ledger WHERE applied = 1 ORDER BY seq`, which is what makes `rewind N` free:
//! deactivate rows past chapter N and the snapshot is simply correct again.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Set,
    Add,
    Sub,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    Num(i64),
    Txt(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub seq: u64,
    pub chapter: u32,
    pub subject: String,
    pub field: String,
    pub op: Op,
    pub value_num: Option<i64>,
    pub value_txt: Option<String>,
    /// `false` means the validation gate rejected it. Kept for audit; inert in the fold.
    pub applied: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateSnapshot {
    pub values: BTreeMap<(String, String), Value>,
    /// Malformed-but-applied entries. Non-fatal by design: the fold is total so a
    /// bookkeeping oddity can never panic the engine mid-chapter.
    pub anomalies: Vec<String>,
}

impl StateSnapshot {
    pub fn num(&self, subject: &str, field: &str) -> Option<i64> {
        match self
            .values
            .get(&(String::from(subject), String::from(field)))
        {
            Some(Value::Num(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn txt(&self, subject: &str, field: &str) -> Option<&str> {
        match self
            .values
            .get(&(String::from(subject), String::from(field)))
        {
            Some(Value::Txt(t)) => Some(t.as_str()),
            _ => None,
        }
    }

    pub fn subjects(&self) -> BTreeSet<&str> {
        self.values.keys().map(|(s, _)| s.as_str()).collect()
    }
}

/// Fold ledger entries into current state. Sorts by `seq` internally, so callers
/// may pass rows in any order.
pub fn fold(entries: &[LedgerEntry]) -> StateSnapshot {
    let mut ordered: Vec<&LedgerEntry> = entries.iter().filter(|e| e.applied).collect();
    ordered.sort_by_key(|e| e.seq);

    let mut snap = StateSnapshot::default();
    for e in ordered {
        let key = (e.subject.clone(), e.field.clone());
        match e.op {
            Op::Set => {
                if let Some(n) = e.value_num {
                    snap.values.insert(key, Value::Num(n));
                } else if let Some(t) = &e.value_txt {
                    snap.values.insert(key, Value::Txt(t.clone()));
                } else {
                    snap.anomalies.push(anomaly(e, "set with no value"));
                }
            }
            Op::Add | Op::Sub => {
                let Some(magnitude) = e.value_num else {
                    snap.anomalies
                        .push(anomaly(e, "add/sub with no numeric value"));
                    continue;
                };
                let signed = if matches!(e.op, Op::Sub) {
                    -magnitude
                } else {
                    magnitude
                };
                match snap.values.get(&key) {
                    Some(Value::Num(cur)) => {
                        let next = cur.saturating_add(signed);
                        snap.values.insert(key, Value::Num(next));
                    }
                    Some(Value::Txt(_)) => {
                        snap.anomalies
                            .push(anomaly(e, "add/sub against a text value"));
                    }
                    None => {
                        snap.values.insert(key, Value::Num(signed));
                    }
                }
            }
        }
    }
    snap
}

fn anomaly(e: &LedgerEntry, why: &str) -> String {
    alloc::format!("seq {} {}.{}: {}", e.seq, e.subject, e.field, why)
}

/// Keep only entries at or before `through_chapter` (inclusive).
pub fn rewind(entries: &[LedgerEntry], through_chapter: u32) -> Vec<LedgerEntry> {
    entries
        .iter()
        .filter(|e| e.chapter <= through_chapter)
        .cloned()
        .collect()
}
