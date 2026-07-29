//! `/api/state` and `/api/character/{subject}` — projections of the ledger fold.
//!
//! Neither route holds state. Both are renderings of `Store::snapshot()`, which is
//! itself a fold over `ledger WHERE applied = 1`. That is what makes these screens
//! unable to disagree with the story (spec §9.4.1).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use litrpg_core::ledger::{StateSnapshot, Value};
use litrpg_core::validate::{
    APPEAR_PREFIX, APPEAR_TRAITS, EQUIP_PREFIX, EQUIP_SLOTS, INVENTORY_PREFIX, NUMERIC_FIELDS,
    TEXT_FIELDS,
};
use serde::Serialize;
use serde_json::{Value as Json_, json};

use crate::AppState;
use crate::error::ApiResult;

/// `Value` derives `Serialize` as an externally-tagged enum (`{"Num":5}`), which is a
/// poor wire shape for clients that just want a number or a string. Flatten it.
fn value_to_json(v: &Value) -> Json_ {
    match v {
        Value::Num(n) => json!(n),
        Value::Txt(t) => json!(t),
    }
}

#[derive(Debug, Serialize)]
pub struct StateResponse {
    /// `subject -> field -> value`, nested because the snapshot's native
    /// `(subject, field)` tuple key has no JSON representation.
    pub subjects: BTreeMap<String, BTreeMap<String, Json_>>,
    /// Malformed-but-applied entries. Exposed rather than hidden: the fold is total
    /// by design so bookkeeping oddities never panic the engine, which means the only
    /// way anyone notices them is if something surfaces them.
    pub anomalies: Vec<String>,
}

fn reshape(snap: &StateSnapshot) -> BTreeMap<String, BTreeMap<String, Json_>> {
    let mut out: BTreeMap<String, BTreeMap<String, Json_>> = BTreeMap::new();
    for ((subject, field), value) in &snap.values {
        out.entry(subject.clone())
            .or_default()
            .insert(field.clone(), value_to_json(value));
    }
    out
}

/// `GET /api/state` — the whole derived snapshot, for a status pane.
pub async fn get_state(State(state): State<Arc<AppState>>) -> ApiResult<Json<StateResponse>> {
    let store = state.store.lock().await;
    let snap = store.snapshot()?;
    drop(store);

    Ok(Json(StateResponse {
        subjects: reshape(&snap),
        anomalies: snap.anomalies,
    }))
}

#[derive(Debug, Serialize)]
pub struct CharacterResponse {
    pub subject: String,
    /// False when the subject has no ledger entries at all. The response is still
    /// `200` with empty slots: the watch's two screens are a fixed layout, and a
    /// `404` would force it to carry an error path for "character not introduced yet".
    pub known: bool,

    // ── Stats screen (spec §9.4.1) ───────────────────────────────────────────
    pub level: Option<i64>,
    pub xp: Option<i64>,
    pub hp: Option<i64>,
    pub max_hp: Option<i64>,
    pub gold: Option<i64>,
    pub location: Option<String>,
    pub status: Option<String>,
    /// `inv:<item>` → count. Free-form item names, so this map is dynamic.
    pub inventory: BTreeMap<String, i64>,

    // ── Character screen ─────────────────────────────────────────────────────
    /// **All eleven** `equip:*` slots, always present, `None` when empty. Complete
    /// because the whitelist is closed (spec §6.2) — the watch draws a fixed row per
    /// slot and must never have to invent layout for a missing or unexpected key.
    pub equipment: BTreeMap<String, Option<String>>,
    /// All six whitelisted `appear:*` traits, same reasoning.
    pub appearance: BTreeMap<String, Option<String>>,
}

/// `GET /api/character/{subject}`
///
/// The watch defaults `subject` to the protagonist, which it learns from
/// `/api/story`'s `protagonist` field.
pub async fn get_character(
    State(state): State<Arc<AppState>>,
    Path(subject): Path<String>,
) -> ApiResult<Json<CharacterResponse>> {
    let store = state.store.lock().await;
    let snap = store.snapshot()?;
    drop(store);

    let known = snap.subjects().contains(subject.as_str());

    let num = |f: &str| snap.num(&subject, f);
    let txt = |f: &str| snap.txt(&subject, f).map(|s| s.to_string());

    // Inventory keys are dynamic, so scan rather than iterate a whitelist.
    let mut inventory = BTreeMap::new();
    for ((subj, field), value) in &snap.values {
        if subj != &subject {
            continue;
        }
        if let (Some(item), Value::Num(count)) = (field.strip_prefix(INVENTORY_PREFIX), value) {
            inventory.insert(item.to_string(), *count);
        }
    }

    // Whitelist-driven, so every slot/trait is present even when unset.
    let equipment = EQUIP_SLOTS
        .iter()
        .map(|slot| {
            (
                (*slot).to_string(),
                txt(&format!("{EQUIP_PREFIX}{slot}")),
            )
        })
        .collect();

    let appearance = APPEAR_TRAITS
        .iter()
        .map(|t| ((*t).to_string(), txt(&format!("{APPEAR_PREFIX}{t}"))))
        .collect();

    debug_assert!(NUMERIC_FIELDS.contains(&"hp") && TEXT_FIELDS.contains(&"location"));

    Ok(Json(CharacterResponse {
        subject: subject.clone(),
        known,
        level: num("level"),
        xp: num("xp"),
        hp: num("hp"),
        max_hp: num("max_hp"),
        gold: num("gold"),
        location: txt("location"),
        status: txt("status"),
        inventory,
        equipment,
        appearance,
    }))
}
