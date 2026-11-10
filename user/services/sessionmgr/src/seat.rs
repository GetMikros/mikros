//! The seat: the display and the input devices that the session manager
//! opens and gives to the text console through `Seat`.

use crate::{SECOND, now};
use alloc::collections::BTreeSet;
use alloc::string::String;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::gpu::{Device as Gpu, DeviceAsyncClient as GpuClient, Display};
use mikros_rt::interfaces::mikros::input::{Device, Input, InputAsyncClient};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::SeatAsyncClient;

const GPU: &str = "device.gpu";
const INPUT: &str = "device.input";
/// How often the input devices are looked for.
const POLL: i64 = SECOND;

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

async fn instances(directory: &DirectoryAsyncClient, service: &str) -> alloc::vec::Vec<String> {
    match directory.list(service.into()).await {
        Ok(Ok(listed)) => listed.instances,
        _ => alloc::vec::Vec::new(),
    }
}

/// Opens the first display, once one is published, and gives it to
/// `seat`.
async fn display(directory: &DirectoryAsyncClient, reactor: &Reactor, seat: &SeatAsyncClient) {
    loop {
        for instance in instances(directory, GPU).await {
            let Some(device) = connect::<Gpu>(directory, GPU, &instance).await else {
                continue;
            };
            let device = GpuClient::new(device, reactor.clone());
            let Ok((display, server)) = endpoints::<Display>() else {
                continue;
            };
            match device.open_display(server, None).await {
                Ok(Ok(_)) => {
                    match seat.add_display(instance.clone(), display) {
                        Ok(()) => mikros_rt::log!(Info, "sessionmgr: gave the console the display"),
                        Err(_) => mikros_rt::log!(Error, "sessionmgr: the console took no display"),
                    }
                    return;
                }
                other => mikros_rt::log!(Error, "sessionmgr: opening {instance}: {other:?}"),
            }
        }
        let _ = reactor.sleep_until(now() + POLL).await;
    }
}

/// Opens every input device as it is published and gives it to `seat`,
/// accepted, for as long as the console runs.
async fn inputs(directory: &DirectoryAsyncClient, reactor: &Reactor, seat: &SeatAsyncClient) {
    let mut given = BTreeSet::new();
    loop {
        for instance in instances(directory, INPUT).await {
            if given.contains(&instance) {
                continue;
            }
            let Some(input) = connect::<Input>(directory, INPUT, &instance).await else {
                continue;
            };
            let input = InputAsyncClient::new(input, reactor.clone());
            let Ok((device, server)) = endpoints::<Device>() else {
                continue;
            };
            if !matches!(input.open(server, None).await, Ok(Ok(_))) {
                mikros_rt::log!(Error, "sessionmgr: opening {instance} failed");
                continue;
            }
            if seat.add_input(instance.clone(), device, true).is_err() {
                return;
            }
            given.insert(instance);
        }
        let _ = reactor.sleep_until(now() + POLL).await;
    }
}

/// Gives the console the display and the input devices.
pub async fn serve(directory: &DirectoryAsyncClient, reactor: &Reactor, seat: SeatAsyncClient) {
    display(directory, reactor, &seat).await;
    inputs(directory, reactor, &seat).await;
}
