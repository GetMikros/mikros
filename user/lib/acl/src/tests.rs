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

fn subject(user: u32, app: u32) -> Subject {
    Subject {
        user,
        app,
        pid: 1,
        koid: 2,
        admin: 0,
        portable_checksum: Checksum::NONE,
    }
}

#[test]
fn keeps_checksums_up_to_their_longest() {
    let full = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(full.len(), CHECKSUM_MAX);
    assert_eq!(Checksum::new(full).unwrap().as_str(), full);
    assert_eq!(Checksum::new("build").unwrap().as_str(), "build");
    assert_eq!(Checksum::new("").unwrap(), Checksum::NONE);
    assert!(Checksum::new(&alloc::format!("{full}0")).is_none());
}

#[test]
fn only_subjects_other_than_user_0_meet_extended_lists() {
    let list = owner_user_app();
    let plain = from_mode(0);
    // Without a subject, as user 0 and without an extended list, rights
    // alone decide.
    assert!(permits(None, Some(&list), 1000, WRITE));
    assert!(permits(Some(&subject(0, 0)), Some(&list), 1000, WRITE));
    assert!(permits(Some(&subject(1002, 0)), Some(&plain), 1000, WRITE));
    assert!(permits(Some(&subject(1002, 0)), None, 1000, WRITE));
    assert!(!permits(Some(&subject(1001, 0)), Some(&list), 1000, WRITE));
    assert!(permits(Some(&subject(1001, 0)), Some(&list), 1000, READ));
    assert_eq!(new_owner(None), 0);
    assert_eq!(new_owner(Some(&subject(1001, 3))), 1001);
}

#[test]
fn others_than_the_owner_and_user_0_need_the_permission() {
    assert!(!needs_manage(None, 1000));
    assert!(!needs_manage(Some(&subject(0, 0)), 1000));
    assert!(!needs_manage(Some(&subject(1000, 0)), 1000));
    assert!(needs_manage(Some(&subject(1001, 0)), 1000));
}

#[test]
fn new_nodes_take_the_default_list() {
    let list = owner_user_app();
    assert_eq!(
        inherit(None, 0o644, true),
        Inherited {
            access: None,
            mode: 0o644,
            default: None
        }
    );
    let file = inherit(Some(&list), 0o100666, false);
    assert_eq!(file.access.as_deref(), Some(&list[..]));
    assert_eq!(file.mode, 0o100640);
    assert_eq!(file.default, None);
    let dir = inherit(Some(&list), 0o40777, true);
    assert_eq!(dir.default.as_deref(), Some(&list[..]));
    // A default list of three entries sets only the mode.
    let plain = inherit(Some(&from_mode(0o750)), 0o777, false);
    assert_eq!((plain.access, plain.mode), (None, 0o750));
}
