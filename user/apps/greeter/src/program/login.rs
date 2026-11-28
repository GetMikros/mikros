//! The `Login` channel (MIKSPEC-0075.R30): a thread of its own makes the
//! calls, which may take seconds, as a login waits for the user's desktop
//! to start, and hands what comes back to the screen.

use crate::model::{Failure, Power, User, collect_users};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ShutdownKind;
use mikros_rt::interfaces::mikros::session::{Login, LoginClient};
use std::os::mikros::startup;
use std::sync::mpsc;
use std::thread;

/// The `USER` startup handles of a part in greeter mode: the `Login`
/// channel.
const USER: u32 = 16;
const LOGIN: u32 = 3;
/// Calls wait for their reply without a deadline.
const NO_DEADLINE: i64 = i64::MAX;

/// What the screen asks the service.
pub enum Request {
    /// The list of the users.
    Users,
    /// A login of the user `name`, with the password `password`, which the
    /// request takes and the call drops.
    Login { name: String, password: String },
    /// A restart or a shutdown of the machine.
    Power(Power),
}

/// What comes back.
pub enum Event {
    Users(Result<Vec<User>, Failure>),
    Login(Result<(), Failure>),
    Power(Result<(), Failure>),
}

/// The thread that serves the requests.
pub struct Service {
    requests: mpsc::Sender<Request>,
}

impl Service {
    /// Starts the thread, which gives each event to `deliver` on that
    /// thread. `None` if the part has no `Login` channel.
    pub fn start(deliver: impl Fn(Event) + Send + 'static) -> Option<Service> {
        let handle = startup::take_handle(USER, LOGIN)?;
        // SAFETY: the handle is ours, and is handed over once.
        let handle = unsafe { OwnedHandle::from_raw(handle.into_raw_handle()) };
        let client = LoginClient::new(ClientEnd::<Login>::new(handle));
        let (requests, queue) = mpsc::channel();
        thread::Builder::new()
            .name("login".to_owned())
            .spawn(move || {
                for request in queue {
                    deliver(serve(&client, request));
                }
            })
            .ok()?;
        Some(Service { requests })
    }

    /// Queues `request`; false when the thread is gone.
    pub fn send(&self, request: Request) -> bool {
        self.requests.send(request).is_ok()
    }
}

/// The failure that a status of the service stands for.
fn failure(status: &Status) -> Failure {
    match status {
        Status::AccessDenied => Failure::Denied,
        Status::AlreadyExists => Failure::Running,
        Status::Io | Status::BadState | Status::NotFound => Failure::Start,
        _ => Failure::Unavailable,
    }
}

/// Makes the call that `request` is, and returns what came of it.
fn serve(client: &LoginClient, request: Request) -> Event {
    match request {
        Request::Users => Event::Users(collect_users(|after| {
            let page = client.list_users(after, NO_DEADLINE).ok()?;
            let users = page
                .users
                .into_iter()
                .map(|u| User {
                    name: u.name,
                    full_name: u.full_name,
                })
                .collect();
            Some((users, page.last))
        })),
        Request::Login { name, password } => {
            Event::Login(match client.login(name, password, NO_DEADLINE) {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(status)) => Err(failure(&status)),
                Err(_) => Err(Failure::Unavailable),
            })
        }
        Request::Power(power) => {
            let kind = match power {
                Power::Restart => ShutdownKind::Restart,
                Power::ShutDown => ShutdownKind::PowerOff,
            };
            Event::Power(match client.shutdown(kind, NO_DEADLINE) {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(status)) => Err(failure(&status)),
                Err(_) => Err(Failure::Unavailable),
            })
        }
    }
}
