//! What the greeter decides without a screen or a service: how the pages
//! of the user list add up, who is selected, and what a failure says.

/// The most pages of the user list that are read, so that a service that
/// never ends its list cannot keep the greeter reading.
const PAGES_MAX: usize = 256;

/// A user, as the `Login` protocol lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub full_name: String,
}

impl User {
    /// What the list shows of the user: the full name, or the name when
    /// the user has no full name.
    pub fn label(&self) -> &str {
        if self.full_name.trim().is_empty() {
            &self.name
        } else {
            &self.full_name
        }
    }

    /// The letter in the avatar: the first one of the label, in capitals.
    pub fn initial(&self) -> String {
        self.label()
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".to_owned())
    }
}

/// Why a login, or the list of users, or a shutdown failed, as far as the
/// screen tells the reasons apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The name or the password is not right.
    Denied,
    /// The user has a desktop already.
    Running,
    /// The user's session could not be started.
    Start,
    /// The login service does not answer.
    Unavailable,
    /// The service lists no user.
    NoUsers,
}

impl Failure {
    /// The line that the screen shows.
    pub fn message(self) -> &'static str {
        match self {
            Failure::Denied => "The password is not right.",
            Failure::Running => "This user has a desktop running already.",
            Failure::Start => "The session could not be started.",
            Failure::Unavailable => "The login service does not answer.",
            Failure::NoUsers => "There is no user to log in.",
        }
    }
}

/// What the screen asks the machine to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
}

/// The users that `page` lists, page after page: `page(after)` gives the
/// users after the one with the ID `after` and the ID of the last of them,
/// or `None` when the service fails. The list ends with an empty page.
pub fn collect_users(
    mut page: impl FnMut(u32) -> Option<(Vec<User>, u32)>,
) -> Result<Vec<User>, Failure> {
    let mut users = Vec::new();
    let mut after = 0;
    for _ in 0..PAGES_MAX {
        let (more, last) = page(after).ok_or(Failure::Unavailable)?;
        // A last ID that does not advance would repeat the page.
        if more.is_empty() || last == after {
            break;
        }
        users.extend(more);
        after = last;
    }
    Ok(users)
}

/// The index of the user selected after `from`, one step on or back in a
/// list of `count` users, wrapping around.
pub fn step(from: usize, count: usize, forward: bool) -> usize {
    match (count, forward) {
        (0, _) => 0,
        (_, true) => (from + 1) % count,
        (_, false) => (from + count - 1) % count,
    }
}

/// Whether the password of the selected user may be sent: a user is
/// selected, a password is typed, and no login is waiting for an answer.
pub fn may_log_in(users: usize, selected: usize, password: &str, busy: bool) -> bool {
    selected < users && !password.is_empty() && !busy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str, full: &str) -> User {
        User {
            name: name.to_owned(),
            full_name: full.to_owned(),
        }
    }

    #[test]
    fn labels_and_initials_come_from_the_full_name_or_the_name() {
        assert_eq!(user("dee", "Dee Ay").label(), "Dee Ay");
        assert_eq!(user("dee", " ").label(), "dee");
        assert_eq!(user("dee", "Dee Ay").initial(), "D");
        assert_eq!(user("eve", "").initial(), "E");
        assert_eq!(user("x", "édith").initial(), "É");
        assert_eq!(user("", "").initial(), "?");
    }

    #[test]
    fn the_pages_of_the_user_list_add_up_until_an_empty_page() {
        let all = [(1, "a"), (2, "b"), (5, "c")];
        let mut asked = Vec::new();
        let users = collect_users(|after| {
            asked.push(after);
            let page: Vec<_> = all.iter().filter(|(id, _)| *id > after).take(2).collect();
            let last = page.last().map_or(0, |(id, _)| *id);
            Some((page.iter().map(|(_, n)| user(n, n)).collect(), last))
        })
        .unwrap();
        assert_eq!(
            users.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(asked, [0, 2, 5]);
    }

    #[test]
    fn a_failing_or_endless_list_ends() {
        assert_eq!(collect_users(|_| None), Err(Failure::Unavailable));
        // The service gives the same page for ever.
        let users = collect_users(|after| Some((vec![user("a", "a")], after))).unwrap();
        assert!(users.is_empty());
        // A page after a page that works, then a failure.
        let mut n = 0;
        let result = collect_users(|_| {
            n += 1;
            (n == 1).then(|| (vec![user("a", "a")], 1))
        });
        assert_eq!(result, Err(Failure::Unavailable));
        // Pages that advance without end stop at the limit.
        let mut pages = 0;
        let users = collect_users(|after| {
            pages += 1;
            Some((vec![user("a", "a")], after + 1))
        })
        .unwrap();
        assert_eq!((pages, users.len()), (PAGES_MAX, PAGES_MAX));
    }

    #[test]
    fn selection_steps_wrap_around() {
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 0);
        assert_eq!(step(0, 3, false), 2);
        assert_eq!(step(0, 0, true), 0);
        assert_eq!(step(0, 1, false), 0);
    }

    #[test]
    fn a_login_needs_a_user_a_password_and_no_login_in_progress() {
        assert!(may_log_in(2, 1, "x", false));
        assert!(!may_log_in(2, 2, "x", false));
        assert!(!may_log_in(0, 0, "x", false));
        assert!(!may_log_in(2, 0, "", false));
        assert!(!may_log_in(2, 0, "x", true));
    }

    #[test]
    fn every_failure_has_a_line() {
        for failure in [
            Failure::Denied,
            Failure::Running,
            Failure::Start,
            Failure::Unavailable,
            Failure::NoUsers,
        ] {
            assert!(failure.message().ends_with('.'));
        }
    }
}
