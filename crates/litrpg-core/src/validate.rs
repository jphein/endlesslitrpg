//! The validation gate (spec §6.2).
//!
//! Ember *proposes* deltas; this decides whether they become canon. Pure and
//! `no_std` so it is trivially testable and cannot depend on I/O ordering.

use alloc::collections::BTreeSet;
use alloc::string::String;

use crate::ledger::{Op, StateSnapshot};

pub const NUMERIC_FIELDS: &[&str] = &["hp", "max_hp", "level", "xp", "gold"];
pub const TEXT_FIELDS: &[&str] = &["location", "status"];

/// Inventory counts are dynamic field names: `inv:<item>`. Item names are free-form.
pub const INVENTORY_PREFIX: &str = "inv:";

/// Equipped items: `equip:<slot>`. The slot is whitelisted because each slot is a
/// row on the watch's character screen — an invented slot would break the renderer.
pub const EQUIP_PREFIX: &str = "equip:";
pub const EQUIP_SLOTS: &[&str] = &[
    "head",
    "chest",
    "legs",
    "feet",
    "hands",
    "cloak",
    "main_hand",
    "off_hand",
    "amulet",
    "ring1",
    "ring2",
];

/// Appearance descriptors: `appear:<trait>`. Whitelisted for the same reason.
pub const APPEAR_PREFIX: &str = "appear:";
pub const APPEAR_TRAITS: &[&str] = &["hair", "eyes", "skin", "build", "height", "notable"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub subject: String,
    pub field: String,
    pub op: Op,
    pub value_num: Option<i64>,
    pub value_txt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    UnknownSubject,
    UnknownField,
    MissingNumericValue,
    MissingTextValue,
    TextFieldRequiresSet,
    HpBelowZero,
    HpAboveMax { max: i64 },
    LevelWouldDecrease,
    XpWouldDecrease,
    InventoryWouldGoNegative,
    UnknownEquipSlot,
    UnknownAppearTrait,
}

/// `known_subjects` is the union of cast speakers, existing ledger subjects, and
/// `lore` rows of kind `character`. Pass 2's `new_lore` is applied *before* its
/// deltas, so a character introduced this chapter is already known here.
pub fn validate_delta(
    snap: &StateSnapshot,
    known_subjects: &BTreeSet<String>,
    d: &Delta,
) -> Result<(), Rejection> {
    if !known_subjects.contains(&d.subject) {
        return Err(Rejection::UnknownSubject);
    }

    // Prefixed namespaces are checked first so an unknown slot or trait is
    // reported precisely rather than as a generic UnknownField.
    if let Some(slot) = d.field.strip_prefix(EQUIP_PREFIX) {
        if !EQUIP_SLOTS.contains(&slot) {
            return Err(Rejection::UnknownEquipSlot);
        }
        return validate_text_set(d);
    }

    if let Some(trait_name) = d.field.strip_prefix(APPEAR_PREFIX) {
        if !APPEAR_TRAITS.contains(&trait_name) {
            return Err(Rejection::UnknownAppearTrait);
        }
        return validate_text_set(d);
    }

    if TEXT_FIELDS.contains(&d.field.as_str()) {
        return validate_text_set(d);
    }

    let is_inventory = d.field.starts_with(INVENTORY_PREFIX);
    if !is_inventory && !NUMERIC_FIELDS.contains(&d.field.as_str()) {
        return Err(Rejection::UnknownField);
    }

    let magnitude = d.value_num.ok_or(Rejection::MissingNumericValue)?;
    let current = snap.num(&d.subject, &d.field).unwrap_or(0);
    let next = match d.op {
        Op::Set => magnitude,
        Op::Add => current.saturating_add(magnitude),
        Op::Sub => current.saturating_sub(magnitude),
    };

    if is_inventory {
        if next < 0 {
            return Err(Rejection::InventoryWouldGoNegative);
        }
        return Ok(());
    }

    match d.field.as_str() {
        "hp" => {
            if next < 0 {
                return Err(Rejection::HpBelowZero);
            }
            if let Some(max) = snap.num(&d.subject, "max_hp") {
                if next > max {
                    return Err(Rejection::HpAboveMax { max });
                }
            }
        }
        "level" if next < current => return Err(Rejection::LevelWouldDecrease),
        "xp" if next < current => return Err(Rejection::XpWouldDecrease),
        _ => {}
    }

    Ok(())
}

/// Text-valued fields are absolute assignments only — arithmetic is meaningless on
/// them. An **empty string is legal** and means "slot is empty" / "trait unknown".
fn validate_text_set(d: &Delta) -> Result<(), Rejection> {
    if !matches!(d.op, Op::Set) {
        return Err(Rejection::TextFieldRequiresSet);
    }
    if d.value_txt.is_none() {
        return Err(Rejection::MissingTextValue);
    }
    Ok(())
}
