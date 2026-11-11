use super::*;

fn two() -> Jobs {
    let mut jobs = Jobs::new();
    jobs.add(10, vec![10, 11], "sleep 10 | cat");
    jobs.add(20, vec![20], "vim notes.txt");
    jobs
}

#[test]
fn numbers_jobs_and_tracks_current_and_previous() {
    let jobs = two();
    assert_eq!(jobs.current(), Some(2));
    assert_eq!(jobs.previous(), Some(1));
    assert_eq!(jobs.find("%%"), Ok(2));
    assert_eq!(jobs.find("%+"), Ok(2));
    assert_eq!(jobs.find("%"), Ok(2));
    assert_eq!(jobs.find("%-"), Ok(1));
    assert_eq!(jobs.find("%1"), Ok(1));
    assert_eq!(jobs.find("%3"), Err(Lookup::NoSuchJob));
    assert_eq!(jobs.find("1"), Err(Lookup::Invalid));
}

#[test]
fn finds_jobs_by_command() {
    let mut jobs = two();
    assert_eq!(jobs.find("%vim"), Ok(2));
    assert_eq!(jobs.find("%?cat"), Ok(1));
    assert_eq!(jobs.find("%ls"), Err(Lookup::NoSuchJob));
    jobs.add(30, vec![30], "vim other");
    assert_eq!(jobs.find("%vim"), Err(Lookup::Ambiguous));
}

#[test]
fn a_stopped_job_becomes_current() {
    let mut jobs = two();
    assert_eq!(jobs.stopped(11, 20), Some(1));
    assert_eq!(jobs.current(), Some(1));
    assert_eq!(jobs.previous(), Some(2));
    assert_eq!(jobs.get(1).unwrap().state, State::Stopped(20));
    jobs.continued(1);
    assert_eq!(jobs.get(1).unwrap().state, State::Running);
}

#[test]
fn a_job_is_done_once_every_process_ended_with_the_last_ones_status() {
    let mut jobs = two();
    jobs.ended(11, Ended::Exited(3), true);
    assert_eq!(jobs.get(1).unwrap().state, State::Running);
    jobs.ended(10, Ended::Signaled(13), false);
    assert_eq!(jobs.get(1).unwrap().state, State::Done(Ended::Exited(3)));
    let done = jobs.remove_done();
    assert_eq!(done.len(), 1);
    assert_eq!(jobs.current(), Some(2));
    assert_eq!(jobs.previous(), None);
    assert_eq!(jobs.add(40, vec![40], "make"), 3);
}

#[test]
fn reports_as_jobs_lists() {
    let mut jobs = two();
    jobs.stopped(20, 20);
    let lines: Vec<String> = jobs.all().map(|j| jobs.report(j).to_string()).collect();
    assert_eq!(lines[0], format!("[1]-  {:<24}sleep 10 | cat", "Running"));
    assert_eq!(lines[1], format!("[2]+  {:<24}vim notes.txt", "Stopped"));
}

#[test]
fn a_removed_job_leaves_the_others_current() {
    let mut jobs = two();
    assert_eq!(jobs.remove(2).map(|j| j.number), Some(2));
    assert_eq!(jobs.current(), Some(1));
    assert!(jobs.get(2).is_none());
    assert!(jobs.remove(2).is_none());
}
