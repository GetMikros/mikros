//! A storage controller's `mikros.pci.Device` channel, which `Reconnect`
//! replaces when the PCI bus driver restarts.

use alloc::rc::Rc;
use core::cell::RefCell;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::pci::DeviceAsyncClient;

/// The current client of the controller's bus channel.
pub struct PciBus {
    current: RefCell<Rc<DeviceAsyncClient>>,
    reactor: Reactor,
}

impl PciBus {
    /// The bus channel `bus`, served on the executor of `reactor`.
    pub fn new(bus: OwnedHandle, reactor: &Reactor) -> Rc<PciBus> {
        let client = DeviceAsyncClient::new(ClientEnd::new(bus), reactor.clone());
        Rc::new(PciBus {
            current: RefCell::new(Rc::new(client)),
            reactor: reactor.clone(),
        })
    }

    /// The client to send requests on now. Later requests take it again,
    /// to use the channel that a `Reconnect` gave.
    pub fn get(&self) -> Rc<DeviceAsyncClient> {
        self.current.borrow().clone()
    }

    /// Replaces the channel with `bus`, which `Reconnect` gave.
    pub fn reconnect(&self, bus: OwnedHandle) {
        let client = DeviceAsyncClient::new(ClientEnd::new(bus), self.reactor.clone());
        *self.current.borrow_mut() = Rc::new(client);
    }
}
