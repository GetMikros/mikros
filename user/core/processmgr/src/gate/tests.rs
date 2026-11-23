extern crate std;

use super::*;
use std::vec;

type M = Machine<u32>;

fn proceed(calls: &[u32]) -> Vec<(u32, Outcome<i32>)> {
    calls.iter().map(|&c| (c, Outcome::Proceed)).collect()
}

#[test]
fn the_first_call_asks_and_the_others_wait() {
    let mut m = M::new(false);
    assert_eq!(m.arrive(1), Arrival::Ask);
    assert_eq!(m.arrive(2), Arrival::Wait);
    assert_eq!(m.arrive(3), Arrival::Wait);
    assert_eq!(m.phase(), Phase::Asking);
    assert!(!m.allowed());
}

#[test]
fn allowed_lets_the_waiting_calls_go_on_in_order() {
    let mut m = M::new(false);
    m.arrive(1);
    m.arrive(2);
    assert_eq!(m.ended::<i32>(Answer::Allowed), proceed(&[1, 2]));
    assert!(m.allowed());
    assert_eq!(m.arrive(3), Arrival::Proceed);
}

#[test]
fn denied_is_not_remembered() {
    let mut m = M::new(false);
    m.arrive(1);
    m.arrive(2);
    let out = m.ended::<i32>(Answer::Denied);
    assert_eq!(out, vec![(1, Outcome::Denied), (2, Outcome::Denied)]);
    assert_eq!(m.phase(), Phase::Idle);
    assert_eq!(m.arrive(3), Arrival::Ask);
}

#[test]
fn a_failure_passes_its_status_and_is_not_remembered() {
    let mut m = M::new(false);
    m.arrive(1);
    m.arrive(2);
    let out = m.ended(Answer::Failed(-7));
    assert_eq!(
        out,
        vec![(1, Outcome::Failed(-7)), (2, Outcome::Failed(-7))]
    );
    assert_eq!(m.arrive(3), Arrival::Ask);
}

#[test]
fn revoking_ends_every_phase_and_asks_no_more() {
    for steps in 0..3 {
        let mut m = M::new(false);
        if steps >= 1 {
            m.arrive(1);
        }
        if steps >= 2 {
            m.ended::<i32>(Answer::Allowed);
        }
        let out = m.revoke::<i32>();
        assert_eq!(out.len(), usize::from(steps == 1));
        assert!(out.iter().all(|(_, o)| *o == Outcome::Denied));
        assert_eq!(m.phase(), Phase::Revoked);
        assert!(!m.allowed());
        assert_eq!(m.arrive(9), Arrival::Refuse);
    }
}

#[test]
fn an_answer_after_the_revocation_releases_nobody() {
    let mut m = M::new(false);
    m.arrive(1);
    let out = m.revoke::<i32>();
    assert_eq!(out, vec![(1, Outcome::Denied)]);
    assert!(m.ended::<i32>(Answer::Allowed).is_empty());
    assert!(!m.allowed());
    assert_eq!(m.arrive(2), Arrival::Refuse);
}

#[test]
fn a_subject_bound_after_the_revocation_is_refused() {
    let mut m = M::new(true);
    assert_eq!(m.phase(), Phase::Revoked);
    assert_eq!(m.arrive(1), Arrival::Refuse);
}

#[test]
fn eight_permissions_get_a_pending_entry() {
    assert_eq!(PENDING.len(), 8);
    let names: Vec<&str> = PENDING.iter().map(|(p, _)| *p).collect();
    assert!(!names.contains(&"user.full_disk.access"));
    assert!(!names.contains(&"user.network_volumes.access"));
    assert_eq!(
        PENDING
            .iter()
            .filter(|(_, t)| *t == Target::Volumes)
            .count(),
        1
    );
}

#[test]
fn the_quick_calls_keep_their_answers_unless_revoked_after_forwarding() {
    let mut m = M::new(false);
    assert!(m.unanswered());
    m.arrive(1);
    assert!(m.unanswered());
    m.ended::<i32>(Answer::Allowed);
    assert!(!m.unanswered());
    m.revoke::<i32>();
    assert!(!m.unanswered());
    let mut never = M::new(false);
    never.arrive(1);
    never.revoke::<i32>();
    assert!(never.unanswered());
}

#[test]
fn routes_the_calls_of_a_gate_connection() {
    use Class::*;
    // Without a subject, only `Restrict` and the quick calls are served.
    assert_eq!(route(Restrict, false, true), Route::Restrict);
    assert_eq!(route(Quick, false, true), Route::Fixed);
    assert_eq!(route(WaitingLock, false, true), Route::Refuse);
    assert_eq!(route(Other, false, true), Route::Refuse);
    // With a subject and no answer, the quick calls answer at once and
    // the others wait, the waiting lock apart from the rest.
    assert_eq!(route(Restrict, true, true), Route::Restrict);
    assert_eq!(route(Quick, true, true), Route::Fixed);
    assert_eq!(route(WaitingLock, true, true), Route::Alone);
    assert_eq!(route(Other, true, true), Route::Lane);
    // Once the gate forwarded, the quick calls are forwarded too, each in
    // a task of its own.
    assert_eq!(route(Quick, true, false), Route::Alone);
    assert_eq!(route(WaitingLock, true, false), Route::Alone);
    assert_eq!(route(Other, true, false), Route::Lane);
}
