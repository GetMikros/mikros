//! The seat: the displays and the input devices that the session manager
//! opens and gives through `Seat` to their owner, which is the owner of the
//! screen that is shown: of screen 1, with a desktop installed, the
//! compositor of a user's session while one runs, else the compositor in
//! greeter mode while it runs, else the text console, and of every other
//! screen the text console (MIKSPEC-0066.R43). Each connection is opened
//! with a revocation event of its own; to change the owner, the session
//! manager signals those events, which make the drivers close the
//! connections, and opens and gives new ones to the new owner. Screens are
//! numbered after the function keys: with a desktop installed, screen 1 is
//! the desktop and screens 2 to 7 are the text consoles 1 to 6; otherwise
//! screens 1 to 6 are the text consoles (MIKSPEC-0066.R45).

use crate::{SECOND, now};
use alloc::collections::BTreeSet;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Capabilities, Device as Gpu, DeviceAsyncClient as GpuClient, Display,
};
use mikros_rt::interfaces::mikros::input::{Device, Input, InputAsyncClient};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{SeatAsyncClient, SeatEvent};
use mikros_rt::objects;

const GPU: &str = "device.gpu";
const INPUT: &str = "device.input";
/// How often the devices are looked for, so that those added later, or
/// again after their driver restarted, reach the owner.
const POLL: i64 = SECOND;
/// How long, and how often, a device is opened again while the
/// connection of the previous owner is still being closed.
const REOPEN_FOR: i64 = 2 * SECOND;
const REOPEN_EVERY: i64 = SECOND / 20;
/// The signal that revokes a connection, and the rights that a
/// duplicate keeps.
const SIGNALED: u32 = 1 << 3;
const SAME_RIGHTS: u32 = 1 << 31;

/// Who owns the seat, as the screen that is shown says (MIKSPEC-0066.R43).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Console,
    Greeter,
    Desktop,
}

impl Owner {
    /// The owner's name in the log.
    fn name(self) -> &'static str {
        match self {
            Owner::Console => "console",
            Owner::Greeter => "greeter",
            Owner::Desktop => "desktop",
        }
    }
}

/// The seat and its owners.
pub struct Seats {
    directory: Rc<DirectoryAsyncClient>,
    reactor: Reactor,
    /// Whether a desktop is installed, which gives it screen 1.
    desktop: bool,
    console: Rc<SeatAsyncClient>,
    /// The `Seat` channels of the compositor in greeter mode and of the
    /// compositor of a user's session, while they run.
    greeter: RefCell<Option<Rc<SeatAsyncClient>>>,
    desktop_seat: RefCell<Option<Rc<SeatAsyncClient>>>,
    owner: Cell<Owner>,
    /// The revocation events of the connections given to the owner, and
    /// the instances they are of.
    leases: RefCell<Vec<OwnedHandle>>,
    displays: RefCell<BTreeSet<String>>,
    inputs: RefCell<BTreeSet<String>>,
    /// Counts the changes of owner, so that giving devices to an earlier
    /// owner stops.
    generation: Cell<u64>,
    /// Whether devices are being given, which happens once at a time.
    giving: Cell<bool>,
    /// Whether the console has the seat for a question, which a
    /// compositor that starts meanwhile does not take.
    lent: Cell<bool>,
    /// The screen that is shown, which the session manager sets when it
    /// switches and the owner sets when it changes the screen itself
    /// (MIKSPEC-0066.R43).
    shown: Cell<u8>,
}

impl Seats {
    /// The seat of the text console `console`, its owner until a
    /// compositor runs, on the screen of its console 1.
    pub fn new(
        directory: Rc<DirectoryAsyncClient>,
        reactor: Reactor,
        desktop: bool,
        console: SeatAsyncClient,
    ) -> Rc<Seats> {
        let _ = console.set_screen(1 + u8::from(desktop));
        Rc::new(Seats {
            directory,
            reactor,
            desktop,
            console: Rc::new(console),
            greeter: RefCell::new(None),
            desktop_seat: RefCell::new(None),
            owner: Cell::new(Owner::Console),
            leases: RefCell::new(Vec::new()),
            displays: RefCell::new(BTreeSet::new()),
            inputs: RefCell::new(BTreeSet::new()),
            generation: Cell::new(0),
            giving: Cell::new(false),
            lent: Cell::new(false),
            shown: Cell::new(1),
        })
    }

    fn seat(&self, owner: Owner) -> Option<Rc<SeatAsyncClient>> {
        match owner {
            Owner::Console => Some(self.console.clone()),
            Owner::Greeter => self.greeter.borrow().clone(),
            Owner::Desktop => self.desktop_seat.borrow().clone(),
        }
    }

    /// The owner of `screen` now (MIKSPEC-0066.R43): of screen 1, when a
    /// desktop is installed, the compositor of a user's session while one
    /// runs, else the compositor in greeter mode while it runs, and
    /// otherwise, and for every other screen, the text console.
    fn owner_of(&self, screen: u8) -> Owner {
        if !self.desktop || screen != 1 {
            Owner::Console
        } else if self.desktop_seat.borrow().is_some() {
            Owner::Desktop
        } else if self.greeter.borrow().is_some() {
            Owner::Greeter
        } else {
            Owner::Console
        }
    }

    /// Makes the owner of the screen that is shown the owner, when it is
    /// not yet, unless the console has the seat for a question.
    async fn refresh(&self) {
        let owner = self.owner_of(self.shown.get());
        if self.lent.get() || self.owner.get() == owner {
            return;
        }
        self.give(owner).await;
        if owner == Owner::Console {
            let _ = self.console.set_screen(self.shown.get());
        }
    }

    /// Whether a compositor runs, the one of a user's session or the one in
    /// greeter mode.
    fn compositor_runs(&self) -> bool {
        self.desktop_seat.borrow().is_some() || self.greeter.borrow().is_some()
    }

    /// Makes `owner` the owner: revokes the connections of the previous
    /// one, and gives `owner` new ones.
    pub async fn give(&self, owner: Owner) {
        for lease in self.leases.borrow_mut().drain(..) {
            let _ = objects::signal(lease.raw(), 0, SIGNALED);
        }
        self.displays.borrow_mut().clear();
        self.inputs.borrow_mut().clear();
        self.owner.set(owner);
        self.generation.set(self.generation.get() + 1);
        mikros_rt::log!(Info, "sessionmgr: the seat's owner is the {}", owner.name());
        self.hand_out().await;
    }

    /// Whether a desktop is installed.
    pub fn desktop_installed(&self) -> bool {
        self.desktop
    }

    /// Makes `seat` the seat of the compositor in greeter mode, which owns
    /// the seat while screen 1 is shown and no compositor of a user's
    /// session runs.
    pub async fn set_greeter(&self, seat: Rc<SeatAsyncClient>) {
        *self.greeter.borrow_mut() = Some(seat);
        self.refresh().await;
    }

    /// Makes `seat` the seat of the compositor of a user's session, and
    /// switches to the desktop's screen, which it owns: the compositor in
    /// greeter mode loses its displays and input devices when it runs
    /// (MIKSPEC-0066.R43).
    pub async fn set_desktop(&self, seat: Rc<SeatAsyncClient>) {
        *self.desktop_seat.borrow_mut() = Some(seat);
        self.shown.set(1);
        self.lent.set(false);
        self.refresh().await;
    }

    /// Once the compositor in greeter mode of `seat` has exited, finds the
    /// owner of the screen that is shown again.
    pub async fn release_greeter(&self, seat: &Rc<SeatAsyncClient>) {
        if !same(&self.greeter, seat) {
            return;
        }
        *self.greeter.borrow_mut() = None;
        self.refresh().await;
    }

    /// Once the compositor of a user's session of `seat` has exited, finds
    /// the owner of the screen that is shown again, and when no compositor
    /// runs shows text console `screen`.
    pub async fn release_desktop(&self, seat: &Rc<SeatAsyncClient>, screen: u8) {
        if !same(&self.desktop_seat, seat) {
            return;
        }
        *self.desktop_seat.borrow_mut() = None;
        if self.shown.get() == 1 && !self.compositor_runs() {
            self.shown.set(screen);
        }
        self.refresh().await;
    }

    /// Makes the text console the owner while it asks a question, when a
    /// compositor owns the seat: whether it did. The screen that is shown
    /// does not change (MIKSPEC-0066.R43, MIKSPEC-0057.R37).
    pub async fn lend_to_console(&self) -> bool {
        if self.owner.get() == Owner::Console {
            return false;
        }
        self.lent.set(true);
        self.give(Owner::Console).await;
        true
    }

    /// Gives the seat back to the owner of the screen that is shown after
    /// `lend_to_console`, unless there is none.
    pub async fn give_back(&self) {
        self.lent.set(false);
        self.refresh().await;
    }

    /// The screen of text console `console`.
    pub fn console_screen(&self, console: u8) -> u8 {
        console + u8::from(self.desktop)
    }

    /// Switches to `screen`, ignoring one that does not exist: makes it the
    /// screen that is shown, which makes its owner the owner, and tells the
    /// text console which screen to show when it is the owner
    /// (MIKSPEC-0057.R45).
    pub async fn switch(&self, screen: u8) {
        let first = self.console_screen(1);
        if self.desktop && screen == 1 {
            if !self.compositor_runs() {
                mikros_rt::log!(Info, "sessionmgr: no desktop runs for screen 1");
                return;
            }
        } else if !(first..first + 6).contains(&screen) {
            mikros_rt::log!(Info, "sessionmgr: there is no screen {screen}");
            return;
        }
        // A switch that the user asks for while the text console asks a
        // question moves the devices to the owner of the screen
        // (MIKSPEC-0057.R45); the question stays open.
        self.shown.set(screen);
        self.lent.set(false);
        if self.owner_of(screen) == Owner::Console {
            if self.owner.get() != Owner::Console {
                self.give(Owner::Console).await;
            }
            let _ = self.console.set_screen(screen);
        } else {
            self.refresh().await;
        }
    }

    /// Opens the displays and input devices that the owner has not been
    /// given yet, and gives them to it, until the owner changes.
    async fn hand_out(&self) {
        while self.giving.replace(true) {
            let _ = self.reactor.sleep_until(now() + REOPEN_EVERY).await;
        }
        let generation = self.generation.get();
        let owner = self.owner.get();
        if let Some(seat) = self.seat(owner) {
            self.hand_out_displays(&seat, generation).await;
            self.hand_out_inputs(&seat, generation).await;
        }
        self.giving.set(false);
    }

    /// A revocation event, kept as a lease, and a duplicate for a driver.
    fn lease(&self) -> Option<OwnedHandle> {
        let event = objects::event(0).ok()?;
        let copy = objects::duplicate(event.raw(), SAME_RIGHTS).ok()?;
        self.leases.borrow_mut().push(event);
        Some(copy)
    }

    /// Gives the owner the first display that it can open, which is the
    /// one that the owners draw on.
    async fn hand_out_displays(&self, seat: &SeatAsyncClient, generation: u64) {
        for instance in instances(&self.directory, GPU).await {
            if !self.displays.borrow().is_empty() {
                return;
            }
            let Some(device) = connect::<Gpu>(&self.directory, GPU, &instance).await else {
                continue;
            };
            let device = GpuClient::new(device, self.reactor.clone());
            match device.get_info().await {
                Ok(r) if r.info.capabilities.contains(Capabilities::DISPLAY) => {}
                _ => continue,
            }
            let until = now() + REOPEN_FOR;
            let display = loop {
                if self.generation.get() != generation {
                    return;
                }
                let (Ok((display, server)), Some(revoke)) = (endpoints::<Display>(), self.lease())
                else {
                    return;
                };
                match device.open_display(server, Some(revoke)).await {
                    Ok(Ok(_)) => break Some(display),
                    Ok(Err(Status::AlreadyExists)) if now() < until => {
                        let _ = self.reactor.sleep_until(now() + REOPEN_EVERY).await;
                    }
                    other => {
                        mikros_rt::log!(Error, "sessionmgr: opening {instance}: {other:?}");
                        break None;
                    }
                }
            };
            let Some(display) = display else { continue };
            if self.generation.get() != generation {
                return;
            }
            match seat.add_display(instance.clone(), display) {
                Ok(()) => mikros_rt::log!(
                    Info,
                    "sessionmgr: gave the {} the display",
                    self.owner.get().name()
                ),
                Err(_) => mikros_rt::log!(Error, "sessionmgr: the owner took no display"),
            }
            self.displays.borrow_mut().insert(instance);
        }
    }

    async fn hand_out_inputs(&self, seat: &SeatAsyncClient, generation: u64) {
        for instance in instances(&self.directory, INPUT).await {
            let name = instance.starts_with(|c: char| c.is_ascii_lowercase());
            if !name || self.inputs.borrow().contains(&instance) {
                continue;
            }
            let Some(input) = connect::<Input>(&self.directory, INPUT, &instance).await else {
                continue;
            };
            let input = InputAsyncClient::new(input, self.reactor.clone());
            let until = now() + REOPEN_FOR;
            let device = loop {
                if self.generation.get() != generation {
                    return;
                }
                let (Ok((device, server)), Some(revoke)) = (endpoints::<Device>(), self.lease())
                else {
                    return;
                };
                match input.open(server, Some(revoke)).await {
                    Ok(Ok(_)) => break Some(device),
                    Ok(Err(Status::AlreadyExists)) if now() < until => {
                        let _ = self.reactor.sleep_until(now() + REOPEN_EVERY).await;
                    }
                    other => {
                        mikros_rt::log!(Error, "sessionmgr: opening {instance}: {other:?}");
                        break None;
                    }
                }
            };
            let Some(device) = device else { continue };
            if self.generation.get() != generation {
                return;
            }
            if seat.add_input(instance.clone(), device, true).is_err() {
                return;
            }
            self.inputs.borrow_mut().insert(instance);
        }
    }

    /// Gives the owner the devices as they come, for as long as the
    /// session manager runs.
    pub async fn keep_giving(self: Rc<Self>) {
        loop {
            self.hand_out().await;
            let _ = self.reactor.sleep_until(now() + POLL).await;
        }
    }

    /// Switches screens as the owner of `seat` asks, until it closes it.
    pub async fn switches(self: Rc<Self>, seat: Rc<SeatAsyncClient>) {
        while let Ok(event) = seat.next_event().await {
            match event {
                SeatEvent::OnSwitchRequested(e) => self.switch(e.screen).await,
                // The owner changed the screen by its own doing, such as
                // the console moving among its consoles.
                SeatEvent::OnScreenShown(e) if self.owns(&seat) => {
                    self.shown.set(e.screen);
                    mikros_rt::log!(Info, "sessionmgr: screen {} is shown", self.shown.get());
                }
                _ => {}
            }
        }
    }

    /// Whether `seat` is the seat of the owner now.
    fn owns(&self, seat: &Rc<SeatAsyncClient>) -> bool {
        self.seat(self.owner.get())
            .is_some_and(|owner| Rc::ptr_eq(&owner, seat))
    }

    /// The console's seat, for its requests to switch.
    pub fn console(&self) -> Rc<SeatAsyncClient> {
        self.console.clone()
    }
}

/// Whether the slot `current` holds `seat`.
fn same(current: &RefCell<Option<Rc<SeatAsyncClient>>>, seat: &Rc<SeatAsyncClient>) -> bool {
    current
        .borrow()
        .as_ref()
        .is_some_and(|current| Rc::ptr_eq(current, seat))
}

/// A new connection to the instance `instance` of `service`.
async fn connect<P>(
    directory: &DirectoryAsyncClient,
    service: &str,
    instance: &str,
) -> Option<ClientEnd<P>> {
    let (client, server) = endpoints::<P>().ok()?;
    let connected = directory
        .connect(service.into(), Some(instance.into()), server.into_handle())
        .await;
    matches!(connected, Ok(Ok(_))).then_some(client)
}

/// The instances of `service` now.
async fn instances(directory: &DirectoryAsyncClient, service: &str) -> Vec<String> {
    match directory.list(service.into()).await {
        Ok(Ok(listed)) => listed.instances,
        _ => Vec::new(),
    }
}
