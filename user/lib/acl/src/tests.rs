use crate::*;
use alloc::vec;

fn owner_user_app() -> Vec<Entry> {
    vec![
        Entry::new(Tag::Owner, 0, READ | WRITE),
        Entry::new(Tag::User, 1001, READ | WRITE),
        Entry::new(Tag::Group, 0, READ),
        Entry::new(Tag::App, 7, READ | WRITE | EXECUTE),
        Entry::new(Tag::Mask, 0, READ),
        Entry::new(Tag::Other, 0, 0),
    ]
}

#[test]
fn a_mode_gives_three_entries() {
    assert_eq!(
        from_mode(0o750),
        [
            Entry::new(Tag::Owner, 0, READ | WRITE | EXECUTE),
            Entry::new(Tag::Group, 0, READ | EXECUTE),
            Entry::new(Tag::Other, 0, 0),
        ]
    );
    assert!(!is_extended(&from_mode(0o750)));
    assert!(is_extended(&owner_user_app()));
}

#[test]
fn checks_and_orders_lists() {
    let mut reversed = owner_user_app();
    reversed.reverse();
    assert_eq!(checked(&reversed), Some(owner_user_app()));
    assert!(checked(&from_mode(0o644)).is_some());
    // A named entry needs a mask, and a mask needs a named entry.
    let mut no_mask = owner_user_app();
    no_mask.retain(|e| e.tag != Tag::Mask);
    assert_eq!(checked(&no_mask), None);
    let mut lone_mask = from_mode(0o644);
    lone_mask.push(Entry::new(Tag::Mask, 0, READ));
    assert_eq!(checked(&lone_mask), None);
    // Duplicates, missing entries, IDs on unnamed entries, other bits.
    let mut twice = owner_user_app();
    twice.push(Entry::new(Tag::User, 1001, READ));
    assert_eq!(checked(&twice), None);
    assert_eq!(checked(&from_mode(0o644)[..2]), None);
    let mut with_id = from_mode(0o644);
    with_id[0].id = 5;
    assert_eq!(checked(&with_id), None);
    let mut odd = from_mode(0o644);
    odd[2].rights = 0x8;
    assert_eq!(checked(&odd), None);
}

#[test]
fn a_list_sets_the_mode_from_its_mask() {
    assert_eq!(mode_of(&owner_user_app(), 0o100777), 0o100640);
    assert_eq!(mode_of(&from_mode(0o751), 0o40000), 0o40751);
}

#[test]
fn allows_by_owner_named_entries_within_the_mask_and_other() {
    let list = owner_user_app();
    // The owner, by the OWNER entry.
    assert!(allows(&list, 1000, 1000, 0, READ | WRITE));
    assert!(!allows(&list, 1000, 1000, 0, EXECUTE));
    // A named user, limited by the mask to reading.
    assert!(allows(&list, 1000, 1001, 0, READ));
    assert!(!allows(&list, 1000, 1001, 0, WRITE));
    // An app, also limited by the mask.
    assert!(allows(&list, 1000, 1002, 7, READ));
    assert!(!allows(&list, 1000, 1002, 7, EXECUTE));
    // Anyone else gets OTHER; the GROUP entry never counts.
    assert!(!allows(&list, 1000, 1002, 8, READ));
}

#[test]
fn round_trips_through_the_linux_format() {
    let list = owner_user_app();
    let bytes = encode(&list);
    assert_eq!(&bytes[..4], &2u32.to_le_bytes());
    assert_eq!(bytes.len(), 4 + 8 * list.len());
    // ACL_USER_OBJ with rw- and no ID.
    assert_eq!(&bytes[4..12], &[1, 0, 6, 0, 0xff, 0xff, 0xff, 0xff]);
    // ACL_GROUP for app 7 with rwx.
    assert_eq!(&bytes[28..36], &[8, 0, 7, 0, 7, 0, 0, 0]);
    assert_eq!(decode(&bytes), Some(list));
    assert_eq!(decode(&[1, 0, 0, 0]), None);
    assert_eq!(decode(&bytes[..10]), None);
}
