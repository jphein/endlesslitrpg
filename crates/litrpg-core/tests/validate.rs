use litrpg_core::ledger::{LedgerEntry, Op, fold};
use litrpg_core::validate::{Delta, Rejection, validate_delta};
use std::collections::BTreeSet;

fn known(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| String::from(*s)).collect()
}

fn set_at(seq: u64, subject: &str, field: &str, n: i64) -> LedgerEntry {
    LedgerEntry {
        seq,
        chapter: 1,
        subject: subject.into(),
        field: field.into(),
        op: Op::Set,
        value_num: Some(n),
        value_txt: None,
        applied: true,
    }
}

fn set(subject: &str, field: &str, n: i64) -> LedgerEntry {
    set_at(1, subject, field, n)
}

fn delta(subject: &str, field: &str, op: Op, n: i64) -> Delta {
    Delta {
        subject: subject.into(),
        field: field.into(),
        op,
        value_num: Some(n),
        value_txt: None,
    }
}

fn text_delta(field: &str, op: Op, value: Option<&str>) -> Delta {
    Delta {
        subject: "Kaelen".into(),
        field: field.into(),
        op,
        value_num: None,
        value_txt: value.map(String::from),
    }
}

#[test]
fn accepts_a_plain_damage_delta() {
    let snap = fold(&[
        set("Kaelen", "hp", 100),
        set_at(2, "Kaelen", "max_hp", 100),
    ]);
    let d = delta("Kaelen", "hp", Op::Sub, 12);
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_unknown_subject() {
    let snap = fold(&[]);
    let d = delta("Kaelenn", "hp", Op::Sub, 5); // typo
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownSubject)
    );
}

#[test]
fn rejects_unknown_field() {
    let snap = fold(&[]);
    let d = delta("Kaelen", "charisma", Op::Set, 18);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownField)
    );
}

#[test]
fn rejects_hp_below_zero() {
    let snap = fold(&[set("Kaelen", "hp", 10)]);
    let d = delta("Kaelen", "hp", Op::Sub, 50);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::HpBelowZero)
    );
}

#[test]
fn rejects_hp_above_max() {
    let snap = fold(&[set("Kaelen", "hp", 50), set_at(2, "Kaelen", "max_hp", 100)]);
    let d = delta("Kaelen", "hp", Op::Add, 500);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::HpAboveMax { max: 100 })
    );
}

#[test]
fn allows_hp_above_current_when_max_is_unknown() {
    let snap = fold(&[set("Kaelen", "hp", 50)]);
    let d = delta("Kaelen", "hp", Op::Add, 500);
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_level_decrease() {
    let snap = fold(&[set("Kaelen", "level", 7)]);
    let d = delta("Kaelen", "level", Op::Set, 6);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::LevelWouldDecrease)
    );
}

#[test]
fn rejects_xp_decrease() {
    let snap = fold(&[set("Kaelen", "xp", 4000)]);
    let d = delta("Kaelen", "xp", Op::Sub, 1);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::XpWouldDecrease)
    );
}

#[test]
fn rejects_negative_inventory() {
    let snap = fold(&[set("Kaelen", "inv:ration", 2)]);
    let d = delta("Kaelen", "inv:ration", Op::Sub, 5);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::InventoryWouldGoNegative)
    );
}

#[test]
fn accepts_inventory_within_bounds() {
    let snap = fold(&[set("Kaelen", "inv:ration", 5)]);
    let d = delta("Kaelen", "inv:ration", Op::Sub, 5);
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn text_fields_require_set_with_text() {
    let snap = fold(&[]);
    let subjects = known(&["Kaelen"]);

    let ok = text_delta("location", Op::Set, Some("Ashen Vale"));
    assert_eq!(validate_delta(&snap, &subjects, &ok), Ok(()));

    let no_text = text_delta("location", Op::Set, None);
    assert_eq!(
        validate_delta(&snap, &subjects, &no_text),
        Err(Rejection::MissingTextValue)
    );

    let arithmetic = delta("Kaelen", "location", Op::Add, 1);
    assert_eq!(
        validate_delta(&snap, &subjects, &arithmetic),
        Err(Rejection::TextFieldRequiresSet)
    );
}

#[test]
fn numeric_field_without_a_number_is_rejected() {
    let snap = fold(&[]);
    let d = Delta {
        subject: "Kaelen".into(),
        field: "hp".into(),
        op: Op::Set,
        value_num: None,
        value_txt: Some("lots".into()),
    };
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::MissingNumericValue)
    );
}

#[test]
fn accepts_equipping_a_whitelisted_slot() {
    let snap = fold(&[]);
    let d = text_delta("equip:main_hand", Op::Set, Some("Ashen Blade"));
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn empty_string_unequips_a_slot() {
    let snap = fold(&[]);
    let d = text_delta("equip:cloak", Op::Set, Some(""));
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_an_invented_equipment_slot() {
    let snap = fold(&[]);
    let d = text_delta("equip:third_arm", Op::Set, Some("Spare Sword"));
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownEquipSlot)
    );
}

#[test]
fn rejects_arithmetic_on_an_equipment_slot() {
    let snap = fold(&[]);
    let d = delta("Kaelen", "equip:head", Op::Add, 1);
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::TextFieldRequiresSet)
    );
}

#[test]
fn accepts_a_whitelisted_appearance_trait() {
    let snap = fold(&[]);
    let d = text_delta("appear:hair", Op::Set, Some("black, shorn at the temples"));
    assert_eq!(validate_delta(&snap, &known(&["Kaelen"]), &d), Ok(()));
}

#[test]
fn rejects_an_invented_appearance_trait() {
    let snap = fold(&[]);
    let d = text_delta("appear:aura", Op::Set, Some("crackling violet"));
    assert_eq!(
        validate_delta(&snap, &known(&["Kaelen"]), &d),
        Err(Rejection::UnknownAppearTrait)
    );
}

#[test]
fn all_eleven_slots_are_accepted() {
    let snap = fold(&[]);
    let subjects = known(&["Kaelen"]);
    assert_eq!(litrpg_core::validate::EQUIP_SLOTS.len(), 11);
    for slot in litrpg_core::validate::EQUIP_SLOTS {
        let d = text_delta(&format!("equip:{slot}"), Op::Set, Some("Something"));
        assert_eq!(validate_delta(&snap, &subjects, &d), Ok(()), "slot {slot}");
    }
}
