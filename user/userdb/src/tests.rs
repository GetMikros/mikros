use super::*;
use alloc::vec::Vec;

fn hash(n: u32) -> String {
    alloc::format!("$argon2id$v=19$m=65536,t=3,p=4$salt{n}$hash{n}")
}

fn with(users: &[(&str, Kind)]) -> Users {
    let mut db = Users::new();
    for (i, (name, kind)) in users.iter().enumerate() {
        db.create(name, "Full Name", *kind, hash(i as u32)).unwrap();
    }
    db
}

fn ids(db: &Users, after: u32) -> Vec<u32> {
    db.list(after).map(|r| r.id).collect()
}

#[test]
fn names_have_the_form_of_a_user_name() {
    for good in ["a", "ab", "a-b", "a0", "z9-", &"a".repeat(32)] {
        assert!(valid_name(good), "{good}");
    }
    for bad in [
        "",
        "A",
        "0a",
        "-a",
        "a_b",
        "a.b",
        "a b",
        "é",
        &"a".repeat(33),
    ] {
        assert!(!valid_name(bad), "{bad}");
    }
}

#[test]
fn ids_start_at_1000_and_increase() {
    let db = with(&[("ann", Kind::Admin), ("bob", Kind::Standard)]);
    assert_eq!(ids(&db, 0), [1000, 1001]);
    assert_eq!(db.get(1001).unwrap().name, "bob");
    assert_eq!(db.by_name("ann").unwrap().id, 1000);
    assert_eq!(db.next_id(), 1002);
}

#[test]
fn ids_are_never_reused() {
    let mut db = with(&[("ann", Kind::Admin), ("bob", Kind::Standard)]);
    db.delete(1001).unwrap();
    let id = db.create("bob", "", Kind::Standard, hash(9)).unwrap();
    assert_eq!(id, 1002);
    assert_eq!(ids(&db, 0), [1000, 1002]);
}

#[test]
fn creating_checks_the_names() {
    let mut db = with(&[("ann", Kind::Admin)]);
    assert_eq!(
        db.create("Ann", "", Kind::Standard, hash(1)),
        Err(Refusal::Invalid)
    );
    let long = "x".repeat(FULL_NAME_MAX + 1);
    assert_eq!(
        db.create("bob", &long, Kind::Standard, hash(1)),
        Err(Refusal::Invalid)
    );
    assert_eq!(
        db.create("ann", "", Kind::Standard, hash(1)),
        Err(Refusal::Exists)
    );
    assert_eq!(db.next_id(), 1001);
}

#[test]
fn ids_run_out_after_the_last() {
    let mut db = Users::new();
    db.next = LAST_ID;
    assert_eq!(db.create("ann", "", Kind::Admin, hash(0)), Ok(LAST_ID));
    assert_eq!(
        db.create("bob", "", Kind::Admin, hash(1)),
        Err(Refusal::Exhausted)
    );
    assert_eq!(ids(&db, LAST_ID - 1), [LAST_ID]);
    assert_eq!(ids(&db, LAST_ID), []);
    assert_eq!(ids(&db, u32::MAX), []);
}

#[test]
fn listings_are_pages_in_order_of_id() {
    let mut db = Users::new();
    for i in 0..150 {
        db.create(&alloc::format!("u{i}"), "", Kind::Admin, hash(i))
            .unwrap();
    }
    assert_eq!(ids(&db, 0), (1000..1064).collect::<Vec<_>>());
    assert_eq!(ids(&db, 1063), (1064..1128).collect::<Vec<_>>());
    assert_eq!(ids(&db, 1127), (1128..1150).collect::<Vec<_>>());
    assert_eq!(ids(&db, 1149), []);
}

#[test]
fn the_last_administrator_stays() {
    let mut db = with(&[("ann", Kind::Admin), ("bob", Kind::Standard)]);
    assert_eq!(db.delete(1000), Err(Refusal::LastAdmin));
    assert_eq!(db.set_kind(1000, Kind::Standard), Err(Refusal::LastAdmin));
    assert_eq!(db.set_kind(1000, Kind::Admin), Ok(()));
    db.set_kind(1001, Kind::Admin).unwrap();
    db.set_kind(1000, Kind::Standard).unwrap();
    assert_eq!(db.delete(1001), Err(Refusal::LastAdmin));
    db.delete(1000).unwrap();
    assert_eq!(ids(&db, 0), [1001]);
}

#[test]
fn unknown_ids_are_not_found() {
    let mut db = with(&[("ann", Kind::Admin)]);
    assert_eq!(db.delete(1001), Err(Refusal::NotFound));
    assert_eq!(db.set_kind(1001, Kind::Admin), Err(Refusal::NotFound));
    assert_eq!(db.set_full_name(1001, ""), Err(Refusal::NotFound));
    assert_eq!(db.set_hash(1001, hash(1)), Err(Refusal::NotFound));
}

#[test]
fn records_change() {
    let mut db = with(&[("ann", Kind::Admin)]);
    db.set_full_name(1000, "Ann Example").unwrap();
    db.set_hash(1000, hash(7)).unwrap();
    let ann = db.get(1000).unwrap();
    assert_eq!(ann.full_name, "Ann Example");
    assert_eq!(ann.hash, hash(7));
    let long = "x".repeat(FULL_NAME_MAX + 1);
    assert_eq!(db.set_full_name(1000, &long), Err(Refusal::Invalid));
}

#[test]
fn files_hold_the_users_and_the_next_id() {
    let mut db = with(&[
        ("ann", Kind::Admin),
        ("bob", Kind::Standard),
        ("cy", Kind::Standard),
    ]);
    db.delete(1001).unwrap();
    db.set_full_name(1002, "Cy Ünïcode").unwrap();
    let bytes = encode(&db);
    assert_eq!(decode(&bytes), Ok(db.clone()));
    assert_eq!(decode(&encode(&Users::new())), Ok(Users::new()));
}

#[test]
fn corrupt_files_are_refused() {
    let db = with(&[("ann", Kind::Admin), ("bob", Kind::Standard)]);
    let bytes = encode(&db);
    for len in 0..bytes.len() {
        assert_eq!(decode(&bytes[..len]), Err(Corrupt), "{len}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(decode(&longer), Err(Corrupt));
    let mut magic = bytes.clone();
    magic[0] = b'X';
    assert_eq!(decode(&magic), Err(Corrupt));
    // The next ID at or below an existing ID.
    let mut next = bytes.clone();
    next[8..12].copy_from_slice(&1001u32.to_le_bytes());
    assert_eq!(decode(&next), Err(Corrupt));
    // An unknown kind.
    let mut kind = bytes.clone();
    kind[20] = 3;
    assert_eq!(decode(&kind), Err(Corrupt));
    // A name that is not a user's name.
    let mut name = bytes.clone();
    name[23] = b'A';
    assert_eq!(decode(&name), Err(Corrupt));
}

#[test]
fn duplicate_names_are_refused() {
    let db = with(&[("ann", Kind::Admin), ("bob", Kind::Standard)]);
    let mut bytes = encode(&db);
    let at = bytes.windows(3).rposition(|w| w == b"bob").unwrap();
    bytes[at..at + 3].copy_from_slice(b"ann");
    assert_eq!(decode(&bytes), Err(Corrupt));
}
