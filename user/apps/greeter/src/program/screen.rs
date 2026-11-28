//! The screen, drawn with Elegance: the users, the password of the
//! selected one, what went wrong, and the buttons for restart and shutdown.

use super::login::{Event, Request, Service};
use crate::model::{self, Failure, Power, User};
use elegance::prelude::*;
use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;

/// What the screen shows, as states of the UI thread.
struct Ui {
    users: State<Vec<User>>,
    selected: State<usize>,
    password: State<String>,
    message: State<String>,
    busy: State<bool>,
    service: Option<Service>,
}

thread_local! {
    /// The state that events from the login thread change, on the UI
    /// thread.
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

pub fn run() -> ExitCode {
    let result = App::new()
        .window(
            WindowDesc::new("Greeter")
                .size(1280, 800)
                .fullscreen(true)
                .title_bar(TitleBar::Hidden),
            Screen,
        )
        .run();
    result.unwrap_or(ExitCode::FAILURE)
}

impl Ui {
    fn new() -> Rc<Ui> {
        let service = Service::start(|event| {
            elegance::post_to_ui_thread(move || deliver(event));
        });
        let ui = Rc::new(Ui {
            users: State::new(Vec::new()),
            selected: State::new(0),
            password: State::new(String::new()),
            message: State::new(String::new()),
            busy: State::new(false),
            service,
        });
        UI.with(|slot| *slot.borrow_mut() = Some(ui.clone()));
        ui
    }

    /// Asks the service for `request`, or says that there is none.
    fn ask(&self, request: Request) -> bool {
        let sent = self.service.as_ref().is_some_and(|s| s.send(request));
        if !sent {
            self.message.set(Failure::Unavailable.message().to_owned());
        }
        sent
    }

    /// Sends the password of the selected user and empties the field, so
    /// that this thread keeps none. The field's own text editor still holds
    /// what was typed, which only Elegance can wipe (MIKSPEC-0075.R14).
    fn log_in(&self) {
        let users = self.users.get();
        let index = self.selected.get();
        let password = self.password.get();
        if !model::may_log_in(users.len(), index, &password, self.busy.get()) {
            return;
        }
        self.password.set(String::new());
        self.message.set(String::new());
        self.busy.set(true);
        let sent = self.ask(Request::Login {
            name: users[index].name.clone(),
            password,
        });
        if !sent {
            self.busy.set(false);
        }
    }

    fn power(&self, power: Power) {
        self.message.set(String::new());
        self.ask(Request::Power(power));
    }
}

/// Shows what the login thread found.
fn deliver(event: Event) {
    let Some(ui) = UI.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let failed = |result: Result<(), Failure>| match result {
        Ok(()) => String::new(),
        Err(failure) => failure.message().to_owned(),
    };
    match event {
        Event::Users(Ok(users)) => {
            ui.selected.set(0);
            let none = users.is_empty();
            ui.users.set(users);
            ui.message.set(if none {
                Failure::NoUsers.message().to_owned()
            } else {
                String::new()
            });
        }
        Event::Users(Err(failure)) => ui.message.set(failure.message().to_owned()),
        Event::Login(result) => {
            ui.busy.set(false);
            ui.message.set(failed(result));
        }
        Event::Power(result) => ui.message.set(failed(result)),
    }
}

/// The avatar of a user and the name under it; the selected one has a ring.
#[component]
fn Chip(index: usize, user: User, selected: State<usize>) -> impl IntoView {
    let name = user.label().to_owned();
    let initial = user.initial();
    elg! {
        VStack(spacing: 10) {
            ZStack {
                Circle().fill(Color::rgb(66, 74, 92))
                Text(initial).font(Font::system(34.0, .semibold)).foreground_style(Color::WHITE)
                Circle()
                    .stroke(Color::WHITE, 3)
                    .opacity(move || if selected.get() == index { 1.0 } else { 0.0 })
            }
            .frame(84, 84)
            Text(name).font(Font::system(15.0, .medium)).foreground_style(Color::rgba(255, 255, 255, 0.9))
        }
        .padding(8)
        .on_activate(move || selected.set(index))
    }
}

#[component]
fn Screen() -> impl IntoView {
    let ui = Ui::new();
    let (users, selected, password, message) = (ui.users, ui.selected, ui.password, ui.message);
    let focus = State::new(true);
    let submit = ui.clone();
    let restart = ui.clone();
    let shut_down = ui.clone();
    let start = ui.clone();
    elg! {
        ZStack(alignment: .center) {
            VStack(spacing: 0) {
                Spacer().frame(height: 72)
                Text("Thursday, June 29").font(Font::system(24.0, .medium)).foreground_style(Color::rgba(255, 255, 255, 0.85))
                Text("9:19").font(Font::system(128.0, .bold)).foreground_style(Color::WHITE)
                Spacer()
                HStack(spacing: 16) {
                    ForEach(move || users.get().into_iter().enumerate().collect::<Vec<_>>(), id: |entry: &(usize, User)| entry.0) { entry in
                        Chip(index: entry.0, user: entry.1.clone(), selected: selected)
                    }
                }
                TextField("Password", password, secure: true, on_submit: move || submit.log_in())
                    .focused(focus)
                    .on_key_press_capture(.up, move || selected.set(model::step(selected.get(), users.get().len(), false)))
                    .on_key_press_capture(.down, move || selected.set(model::step(selected.get(), users.get().len(), true)))
                    .frame(width: 280)
                    .padding(top: 12)
                Text(move || message.get())
                    .font(Font::system(15.0, .regular))
                    .foreground_style(Color::hex(0xFF8A80))
                    .frame(height: 28)
                    .padding(top: 8)
                Spacer().frame(height: 72)
            }
            VStack {
                Spacer()
                HStack(spacing: 12) {
                    Spacer()
                    Button("Restart", move || restart.power(Power::Restart))
                    Button("Shut down", move || shut_down.power(Power::ShutDown))
                }
                .padding(24)
            }
        }
        .frame(max_width: .infinity, max_height: .infinity)
        .background(Color::hex(0x1B2230))
        .on_appear(move || {
            start.ask(Request::Users);
        })
    }
}
