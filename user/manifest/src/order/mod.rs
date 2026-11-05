//! The order in which the boot services start.

use crate::{Service, Start};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// The services to start at boot, in an order that starts each one
/// after those it waits for, and the services that never start, with
/// why. Both name services by their index in the slice given to
/// `order`. The start list holds the boot services and the on-demand
/// services that one of them waits for; other on-demand services are
/// in neither list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Order {
    pub start: Vec<usize>,
    pub blocked: Vec<(usize, Blocked)>,
}

/// Why a service never starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Another manifest has the same name.
    DuplicateName,
    /// `after` names a service that no manifest has.
    UnknownAfter(String),
    /// A service it waits for never starts.
    Dependency(String),
    /// It waits for itself through `after`.
    Cycle,
    /// The service that every other one waits for has no manifest of
    /// its own.
    NoFirst,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Started,
    Blocked,
    /// An on-demand service that no boot service waits for, so far.
    Idle,
}

/// Orders the boot services of `services`, and the on-demand services
/// they wait for, with the service named `first` before every other
/// one. Services that are equally ready start in the order of the
/// slice.
pub fn order(services: &[Service], first: &str) -> Order {
    let mut count = BTreeMap::<&str, usize>::new();
    for s in services {
        *count.entry(&s.name).or_default() += 1;
    }
    let index: BTreeMap<&str, usize> = services
        .iter()
        .enumerate()
        .filter(|(_, s)| count[s.name.as_str()] == 1)
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    let mut out = Order::default();
    let mut state = vec![State::Pending; services.len()];
    let mut deps = vec![Vec::new(); services.len()];
    let mut idle = vec![None; services.len()];
    let mut needed = Vec::new();
    for (i, s) in services.iter().enumerate() {
        if count[s.name.as_str()] > 1 {
            state[i] = State::Blocked;
            out.blocked.push((i, Blocked::DuplicateName));
            continue;
        }
        let resolved = resolve(&index, &count, s, first);
        if s.start == Start::OnDemand {
            state[i] = State::Idle;
            idle[i] = Some(resolved);
            continue;
        }
        match resolved {
            Ok(d) => {
                deps[i] = d;
                needed.push(i);
            }
            Err(why) => {
                state[i] = State::Blocked;
                out.blocked.push((i, why));
            }
        }
    }

    // An on-demand service that a service starting at boot waits for
    // starts at boot too.
    while let Some(i) = needed.pop() {
        for k in 0..deps[i].len() {
            let j = deps[i][k];
            match idle[j].take() {
                Some(Ok(d)) => {
                    state[j] = State::Pending;
                    deps[j] = d;
                    needed.push(j);
                }
                Some(Err(why)) => {
                    state[j] = State::Blocked;
                    out.blocked.push((j, why));
                }
                None => {}
            }
        }
    }

    // Start the first ready service, then look again from the start of
    // the slice, so that ties keep the slice's order.
    'scan: loop {
        for i in 0..services.len() {
            if state[i] != State::Pending {
                continue;
            }
            if let Some(&j) = deps[i].iter().find(|&&j| state[j] == State::Blocked) {
                state[i] = State::Blocked;
                out.blocked
                    .push((i, Blocked::Dependency(services[j].name.clone())));
                continue 'scan;
            }
            if deps[i].iter().all(|&j| state[j] == State::Started) {
                state[i] = State::Started;
                out.start.push(i);
                continue 'scan;
            }
        }
        break;
    }

    // What is still pending waits, directly or not, on a cycle.
    for i in 0..services.len() {
        if state[i] != State::Pending {
            continue;
        }
        let why = if reaches(&deps, &state, i, i) {
            Blocked::Cycle
        } else {
            let j = deps[i]
                .iter()
                .copied()
                .find(|&j| state[j] == State::Pending)
                .unwrap_or(i);
            Blocked::Dependency(services[j].name.clone())
        };
        out.blocked.push((i, why));
    }
    out.blocked.sort_by_key(|&(i, _)| i);
    out
}

/// The indices of the services that `s` waits for.
fn resolve(
    index: &BTreeMap<&str, usize>,
    count: &BTreeMap<&str, usize>,
    s: &Service,
    first: &str,
) -> Result<Vec<usize>, Blocked> {
    let mut deps = Vec::new();
    if s.name != first {
        match index.get(first) {
            Some(&j) => deps.push(j),
            None => return Err(Blocked::NoFirst),
        }
    }
    for name in &s.after {
        let Some(&j) = index.get(name.as_str()) else {
            return Err(if count.contains_key(name.as_str()) {
                Blocked::Dependency(name.clone())
            } else {
                Blocked::UnknownAfter(name.clone())
            });
        };
        if !deps.contains(&j) {
            deps.push(j);
        }
    }
    Ok(deps)
}

/// Whether `to` can be reached from `from` through the dependencies of
/// pending services, in one step or more.
fn reaches(deps: &[Vec<usize>], state: &[State], from: usize, to: usize) -> bool {
    let mut seen = vec![false; deps.len()];
    let mut stack: Vec<usize> = deps[from].clone();
    while let Some(i) = stack.pop() {
        if i == to {
            return true;
        }
        if seen[i] || state[i] != State::Pending {
            continue;
        }
        seen[i] = true;
        stack.extend(&deps[i]);
    }
    false
}
