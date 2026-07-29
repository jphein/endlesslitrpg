//! `/api/state` and `/api/character/{subject}` — projections of the ledger fold.
//!
//! Neither route holds state. Both are renderings of `Store::snapshot()`, which is
//! itself a fold over `ledger WHERE applied = 1`. That is what makes these screens
//! unable to disagree with the story (spec §9.4.1).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use litrpg_core::VoiceRef;
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

    /// Chapters whose text shipped but whose audio did not (spec §10), oldest first.
    ///
    /// This is the **persisted** approximation of "stuck". It is a superset of the
    /// genuinely-abandoned set: a chapter queued for its first render attempt appears
    /// here too, so presence means "has no audio", not "the engine gave up".
    pub missing_audio: Vec<u32>,
    /// Chapters whose pass-2 delta extraction never succeeded.
    pub dirty_chapters: Vec<u32>,

    /// The speaker → voice cast, with renderability judged against **this process's**
    /// backends. See [`CastEntry`].
    pub cast: Vec<CastEntry>,
    /// The backend ids `cast[].renderable_here` was judged against.
    ///
    /// Published rather than implied: it is the only way a client can tell whether a
    /// `renderable_here: false` reflects the deployment or merely this binary's features.
    pub cast_judged_against_backends: Vec<String>,

    /// Always `false` in the current topology, and present so a client is told rather
    /// than left to assume.
    ///
    /// The engine caps render retries at 3 per chapter and holds those counters **in
    /// memory**, deliberately — a restart clears them, and a restart is exactly what
    /// follows fixing a missing model. The daemon is a **separate process**, so it
    /// cannot read them either. A status pane must therefore not claim
    /// `missing_audio` distinguishes "will retry" from "abandoned"; it cannot.
    pub retry_counts_visible: bool,
}

/// One cast row, annotated with whether the voice it names can be rendered here.
///
/// # Why this annotation exists
///
/// Render-time voice substitution deliberately does **not** rewrite the cast row: on an
/// Azure-only build a `sherpa:` cast entry still renders, via a substituted Azure voice,
/// and the row keeps naming sherpa so the story's history does not depend on which
/// binary produced it. Correct, but it means a cast row can legitimately disagree with
/// the manifest of the audio actually rendered — which reads as a bug to anyone
/// comparing the two. `renderable_here: false` on a chapter that *has* audio is the
/// signature of that substitution, not of a fault.
///
/// # Scope, and why the field is named `_here`
///
/// The judgement is made against this **process's** registry. The daemon and the engine
/// are separate binaries, so a daemon built without the `sherpa` feature will report
/// every `sherpa:` row unrenderable even where the engine renders them happily. The
/// answer is therefore only trustworthy when both are built from the same commit with
/// the same features — which is why [`StateResponse::cast_judged_against_backends`]
/// publishes the basis instead of leaving a client to assume it.
#[derive(Debug, Clone, Serialize)]
pub struct CastEntry {
    pub speaker: String,
    pub voice_ref: String,
    pub kind: String,
    pub first_chapter: u32,
    /// Backend id, split on the **first** colon — Azure voice names contain colons.
    pub backend: Option<String>,
    /// Mirrors `litrpg_engine::voices::is_usable`: the `voice_ref` parses and names a
    /// backend registered in this process. Registration only, deliberately — that is the
    /// predicate the engine substitutes on, so matching it keeps the two answers
    /// comparable.
    pub renderable_here: bool,
    /// Stricter than `renderable_here`: the backend is registered *and* reports `Ready`.
    /// A registered-but-unconfigured backend (Azure with no key) is the gap between them.
    pub backend_available: bool,
    /// Populated only when something is wrong, and always actionable.
    pub reason: Option<String>,
}

/// Mirror of `litrpg_engine::voices::is_usable`.
///
/// Reimplemented rather than imported: the predicate is `VoiceRef::parse` plus a
/// membership test, and `litrpg-core` (which owns `VoiceRef`) is already a dependency —
/// whereas depending on `litrpg-engine` would pull `litrpg-ember`, `reqwest`, `tracing`
/// and `tracing-subscriber` into the HTTP surface and point the dependency arrow from the
/// server at the render orchestrator. See the report for the suggested shared home if the
/// duplication is unwanted.
fn is_usable_here(voice_ref: &str, backends: &[String]) -> bool {
    VoiceRef::parse(voice_ref)
        .map(|v| backends.contains(&v.backend))
        .unwrap_or(false)
}

fn cast_entries(state: &AppState, rows: Vec<litrpg_store::CastRow>) -> Vec<CastEntry> {
    let backends: Vec<String> = state.tts.ids().into_iter().map(String::from).collect();

    rows.into_iter()
        .map(|r| {
            let parsed = VoiceRef::parse(&r.voice_ref);
            let backend = parsed.as_ref().ok().map(|v| v.backend.clone());
            let renderable_here = is_usable_here(&r.voice_ref, &backends);

            let availability = backend
                .as_deref()
                .and_then(|id| state.tts.get(id))
                .map(|b| b.available());
            let backend_available = availability.as_ref().map(|a| a.is_ready()).unwrap_or(false);

            let reason = match (&parsed, renderable_here, &availability) {
                (Err(e), _, _) => Some(format!("malformed voice_ref: {e}")),
                (Ok(v), false, _) => Some(format!(
                    "backend {:?} is not registered in this process; audio for this \
                     speaker was rendered by a substituted voice",
                    v.backend
                )),
                (Ok(_), true, Some(a)) => a
                    .reason()
                    .map(|why| format!("backend registered but unavailable: {why}")),
                (Ok(_), true, None) => None,
            };

            CastEntry {
                speaker: r.speaker,
                voice_ref: r.voice_ref,
                kind: r.kind,
                first_chapter: r.first_chapter,
                backend,
                renderable_here,
                backend_available,
                reason,
            }
        })
        .collect()
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
///
/// Also carries the render-health lists, because the daemon is the only long-lived HTTP
/// surface: `litrpg status` is a short-lived process and cannot show a running engine's
/// in-memory retry state either. See [`StateResponse::retry_counts_visible`] for exactly
/// what this can and cannot tell you.
pub async fn get_state(State(state): State<Arc<AppState>>) -> ApiResult<Json<StateResponse>> {
    let store = state.store.lock().await;
    let snap = store.snapshot()?;
    let missing_audio = store.chapters_missing_audio()?;
    let dirty_chapters = store.dirty_chapters()?;
    let cast_rows = store.cast()?;
    drop(store);

    Ok(Json(StateResponse {
        subjects: reshape(&snap),
        anomalies: snap.anomalies,
        missing_audio,
        dirty_chapters,
        cast: cast_entries(&state, cast_rows),
        cast_judged_against_backends: state.tts.ids().into_iter().map(String::from).collect(),
        retry_counts_visible: false,
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

/// `GET /api/character` — the protagonist, so the watch's character screen (spec
/// §9.4.1) need not already know whose story this is.
///
/// Resolution order: the **`story` table's `protagonist` column**, then
/// `config.story.protagonist`, then a `400`. The table wins because it is the canonical
/// record of whose story this is; config is the bootstrap default for a deployment where
/// `litrpg init` has not yet run.
///
/// An unresolved protagonist is a `400`, not an empty `200`: silently answering for the
/// subject `""` would render a blank character screen that looks like a story with no
/// protagonist rather than a daemon that was never told who it is.
pub async fn get_protagonist(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<CharacterResponse>> {
    let from_store = {
        let store = state.store.lock().await;
        store
            .story()?
            .map(|s| s.protagonist.trim().to_string())
            .filter(|p| !p.is_empty())
    };

    let protagonist =
        from_store.unwrap_or_else(|| state.config.story.protagonist.trim().to_string());

    if protagonist.is_empty() {
        return Err(crate::error::ApiError::BadRequest(
            "no protagonist resolved: the story table has no row (run `litrpg init`) \
             and no fallback is configured; set LITRPG_PROTAGONIST or request \
             /api/character/{subject} explicitly"
                .into(),
        ));
    }
    character_for(&state, protagonist).await
}

/// `GET /api/character/{subject}`
pub async fn get_character(
    State(state): State<Arc<AppState>>,
    Path(subject): Path<String>,
) -> ApiResult<Json<CharacterResponse>> {
    character_for(&state, subject).await
}

/// Shared body, so the protagonist route and the explicit route cannot drift.
async fn character_for(
    state: &Arc<AppState>,
    subject: String,
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
        .map(|slot| ((*slot).to_string(), txt(&format!("{EQUIP_PREFIX}{slot}"))))
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
