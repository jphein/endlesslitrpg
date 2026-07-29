//! Subject identity: the stored `identity_key`, and one-hop alias resolution (#11, #14).
//!
//! Both exist because "are these two speaker names the same person" had seven answers in
//! seven places. `litrpg_core::speaker` now owns the rule; this module is how SQL reaches it —
//! by **storing the rule's output** and indexing that, never by re-expressing the rule in
//! another language.

use std::collections::BTreeMap;

use litrpg_core::speaker::identity_key;
use rusqlite::params;

use crate::{Result, Store, now_ms};

/// One recorded alias: a name in the ledger that denotes an existing subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    /// The name as it appears in the ledger, in display form.
    pub alias_key: String,
    /// The subject it resolves to, in display form.
    pub canonical: String,
    pub canonical_key: String,
    pub created_at: i64,
}

impl Store {
    /// Fill `cast.identity_key` from the owner, then enforce uniqueness on it.
    ///
    /// The index is created here rather than in the `.sql` because it cannot exist before the
    /// values do, and the values cannot be computed in SQL without restating the rule.
    ///
    /// Failure is deliberate and loud: a database holding two cast rows that are one person
    /// has a real ambiguity about which voice that person has, and only a human can say which.
    /// The `.sql` carries the diagnostic query.
    pub(crate) fn backfill_cast_identity_keys(&self) -> Result<()> {
        let rows: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare("SELECT id, speaker FROM cast")?;
            let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?)))?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, speaker) in rows {
            self.conn.execute(
                "UPDATE cast SET identity_key = ?1 WHERE id = ?2",
                params![identity_key(&speaker), id],
            )?;
        }
        self.conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS cast_identity_key_idx ON cast (identity_key)",
        )?;
        Ok(())
    }

    /// Apply the `Kaelen -> Kaelen Vord` mapping, but **only where the collision exists**.
    ///
    /// A fresh database has no Kaelen and must not be told it does, so this is conditional on
    /// both names actually appearing in the ledger. Idempotent, so re-running `migrate` on an
    /// already-seeded database changes nothing.
    #[doc(hidden)]
    pub fn seed_known_aliases_for_test(&self) -> Result<()> {
        self.seed_known_aliases()
    }

    pub(crate) fn seed_known_aliases(&self) -> Result<()> {
        const SEEDS: &[(&str, &str)] = &[("Kaelen", "Kaelen Vord")];
        for (alias, canonical) in SEEDS {
            if self.ledger_has_subject(alias)? && self.ledger_has_subject(canonical)? {
                self.add_alias(alias, canonical)?;
            }
        }
        Ok(())
    }

    /// Whether any ledger row denotes `name`.
    ///
    /// Compared in Rust, not with `lower(subject) = ?`. That SQL would have been rule number
    /// eight in this very file: `lower` is only *part* of `identity_key`, so a subject written
    /// with doubled internal whitespace would not have matched and the seed would have been
    /// silently skipped. Reading the distinct subjects and asking the owner is a few more rows
    /// and one fewer rule.
    fn ledger_has_subject(&self, name: &str) -> Result<bool> {
        let key = identity_key(name);
        let mut stmt = self.conn.prepare("SELECT DISTINCT subject FROM ledger")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if identity_key(&r.get::<_, String>(0)?) == key {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record that `alias` denotes the same character as `canonical`.
    ///
    /// Both sides are reduced with `identity_key`, so an alias matches regardless of case or
    /// internal whitespace. Self-aliasing is refused by the table's CHECK; a name that already
    /// resolves is overwritten, because the last decision is the operative one.
    pub fn add_alias(&self, alias: &str, canonical: &str) -> Result<()> {
        let alias_key = identity_key(alias);
        let canonical_key = identity_key(canonical);
        if alias_key.is_empty() || canonical_key.is_empty() {
            return Err(crate::StoreError::EmptyField { field: "alias" });
        }
        if alias_key == canonical_key {
            return Err(crate::StoreError::SelfAlias {
                name: canonical.to_string(),
            });
        }
        self.conn.execute(
            "INSERT INTO subject_alias (alias_key, canonical, canonical_key, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(alias_key) DO UPDATE SET
                 canonical     = excluded.canonical,
                 canonical_key = excluded.canonical_key,
                 created_at    = excluded.created_at",
            params![alias_key, canonical, canonical_key, now_ms()],
        )?;
        Ok(())
    }

    /// Undo an alias. Returns whether one was removed.
    ///
    /// The rows it was resolving are untouched, so removing an alias un-merges the sheet
    /// rather than losing anything — which is what makes the decision reversible, and is only
    /// true because the ledger was never rewritten.
    pub fn remove_alias(&self, alias: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM subject_alias WHERE alias_key = ?1",
            params![identity_key(alias)],
        )?;
        Ok(n > 0)
    }

    pub fn aliases(&self) -> Result<Vec<Alias>> {
        let mut stmt = self.conn.prepare(
            "SELECT alias_key, canonical, canonical_key, created_at FROM subject_alias
             ORDER BY alias_key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Alias {
                alias_key: r.get(0)?,
                canonical: r.get(1)?,
                canonical_key: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// `identity_key -> canonical display name`, for resolving a batch of subjects.
    ///
    /// Loaded once per fold rather than queried per row: `entries` resolves every ledger row
    /// and a per-row query would make the fold O(rows) round-trips.
    pub(crate) fn alias_map(&self) -> Result<BTreeMap<String, String>> {
        Ok(self
            .aliases()?
            .into_iter()
            .map(|a| (a.alias_key, a.canonical))
            .collect())
    }

    /// The subject a name resolves to, in display form. One hop, never a chain.
    ///
    /// Chained aliases would make the result depend on resolution order, so the table forbids
    /// a name being its own alias and this deliberately does not follow a second hop.
    pub fn resolve_subject(&self, name: &str) -> Result<String> {
        Ok(self
            .alias_map()?
            .get(&identity_key(name))
            .cloned()
            .unwrap_or_else(|| name.to_string()))
    }
}

/// Resolve with an already-loaded map. Free function so the fold can resolve thousands of
/// rows without touching the database again.
pub(crate) fn resolve_with(map: &BTreeMap<String, String>, name: &str) -> String {
    map.get(&identity_key(name))
        .cloned()
        .unwrap_or_else(|| name.to_string())
}
