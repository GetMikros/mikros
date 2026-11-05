extern crate std;

use crate::*;
use alloc::string::ToString;
use alloc::vec;

fn service(name: &str, start: Start, after: &[&str]) -> Service {
    Service {
        name: name.to_string(),
        binary: std::format!("/usr/bin/{name}"),
        provides: vec![],
        after: after.iter().map(|a| a.to_string()).collect(),
        start,
        restart: Restart::Never,
        boot_handles: vec![],
        permissions: vec![],
        priority: 16,
    }
}

fn boot(name: &str, after: &[&str]) -> Service {
    service(name, Start::Boot, after)
}

const FIRST: &str = "permissions";

#[test]
fn first_starts_before_everything() {
    let services = [boot("a", &[]), boot("b", &[]), boot(FIRST, &[])];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [2, 0, 1]);
    assert!(o.blocked.is_empty());
}

#[test]
fn after_is_respected_and_ties_keep_slice_order() {
    let services = [
        boot("c", &["b"]),
        boot("b", &["a"]),
        boot("a", &[]),
        boot("d", &[]),
        boot(FIRST, &[]),
    ];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [4, 2, 1, 0, 3]);
}

#[test]
fn on_demand_services_are_left_out() {
    let services = [boot(FIRST, &[]), service("lazy", Start::OnDemand, &[])];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [0]);
    assert!(o.blocked.is_empty());
}

#[test]
fn duplicate_names_block_every_copy_and_their_dependents() {
    let services = [
        boot(FIRST, &[]),
        boot("a", &[]),
        service("a", Start::OnDemand, &[]),
        boot("b", &["a"]),
    ];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [0]);
    assert_eq!(
        o.blocked,
        [
            (1, Blocked::DuplicateName),
            (2, Blocked::DuplicateName),
            (3, Blocked::Dependency("a".to_string())),
        ]
    );
}

#[test]
fn unknown_dependencies_block() {
    let services = [boot(FIRST, &[]), boot("a", &["nowhere"]), boot("b", &["a"])];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [0]);
    assert_eq!(
        o.blocked,
        [
            (1, Blocked::UnknownAfter("nowhere".to_string())),
            (2, Blocked::Dependency("a".to_string())),
        ]
    );
}

#[test]
fn on_demand_dependencies_start_at_boot() {
    let services = [
        boot("b", &["lazy"]),
        service("lazy", Start::OnDemand, &["lazier"]),
        service("lazier", Start::OnDemand, &[]),
        service("unused", Start::OnDemand, &[]),
        boot(FIRST, &[]),
    ];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [4, 2, 1, 0]);
    assert!(o.blocked.is_empty());
}

#[test]
fn blocked_on_demand_dependencies_block_their_dependents() {
    let services = [
        boot(FIRST, &[]),
        service("lazy", Start::OnDemand, &["nowhere"]),
        boot("b", &["lazy"]),
        service("unused", Start::OnDemand, &["nowhere"]),
    ];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [0]);
    assert_eq!(
        o.blocked,
        [
            (1, Blocked::UnknownAfter("nowhere".to_string())),
            (2, Blocked::Dependency("lazy".to_string())),
        ]
    );
}

#[test]
fn cycles_block_their_members_and_dependents() {
    let services = [
        boot(FIRST, &[]),
        boot("a", &["b"]),
        boot("b", &["a"]),
        boot("c", &["a"]),
        boot("self", &["self"]),
        boot("ok", &[]),
    ];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [0, 5]);
    assert_eq!(
        o.blocked,
        [
            (1, Blocked::Cycle),
            (2, Blocked::Cycle),
            (3, Blocked::Dependency("a".to_string())),
            (4, Blocked::Cycle),
        ]
    );
}

#[test]
fn first_waiting_on_another_service_is_a_cycle() {
    let services = [boot(FIRST, &["a"]), boot("a", &[])];
    let o = order(&services, FIRST);
    assert!(o.start.is_empty());
    assert_eq!(o.blocked, [(0, Blocked::Cycle), (1, Blocked::Cycle)]);
}

#[test]
fn without_first_nothing_else_starts_and_an_on_demand_first_starts_at_boot() {
    let services = [boot("a", &[]), boot("b", &[])];
    let o = order(&services, FIRST);
    assert!(o.start.is_empty());
    assert_eq!(o.blocked, [(0, Blocked::NoFirst), (1, Blocked::NoFirst)]);

    let services = [service(FIRST, Start::OnDemand, &[]), boot("a", &[])];
    let o = order(&services, FIRST);
    assert_eq!(o.start, [0, 1]);
    assert!(o.blocked.is_empty());
}

#[test]
fn blocked_first_blocks_the_rest() {
    let services = [boot(FIRST, &["nowhere"]), boot("a", &[])];
    let o = order(&services, FIRST);
    assert_eq!(
        o.blocked,
        [
            (0, Blocked::UnknownAfter("nowhere".to_string())),
            (1, Blocked::Dependency(FIRST.to_string())),
        ]
    );
}
