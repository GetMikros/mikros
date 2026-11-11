//! The boot service of `tests/input.img`, the client of the display and
//! input drivers in the input run of `cargo xtask test`. It owns the
//! basic display driver's display and fills it with `display::PAINT`,
//! logging `input-probe: painted` once that commit completed, and then
//! opens the input devices of the PS/2 and USB HID drivers and waits for
//! the keys and mouse movement that the runner sends. Then it gives the
//! display up, so that the driver shows the log again, logs
//! `input-probe: closed the display`, and waits for B, which the runner
//! presses once it has looked at the screen. It logs `PASSED`, or
//! `FAILED` with what failed.

#![no_std]
#![no_main]

extern crate alloc;

mod display;
mod input;

use alloc::string::String;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

/// The `id` of the `log_manage` boot handle among the `USER` ones.
const LOG_ID: u32 = 2;
/// How long the drivers may take to publish their services.
const PUBLISH_LIMIT: i64 = 120_000_000_000;

static mut PROCESS: sys::Handle = 0;

/// The probe's own process.
pub fn process() -> sys::Handle {
    // SAFETY: written once in `main` before anything reads it.
    unsafe { PROCESS }
}

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

pub fn failed<T>(what: &str) -> Option<T> {
    mikros_rt::log!(Error, "input-probe: FAILED: {what}");
    None
}

/// The output of `a` if it completes before `b`.
pub async fn first<A: Future, B: Future>(a: A, b: B) -> Option<A::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(out) = a.as_mut().poll(cx) {
            return Poll::Ready(Some(out));
        }
        if b.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// The instances of `service` once there are at least `count`.
pub async fn instances(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    service: &str,
    count: usize,
) -> Option<Vec<String>> {
    let deadline = now() + PUBLISH_LIMIT;
    loop {
        if let Ok(Ok(listed)) = directory.list(service.into()).await
            && listed.instances.len() >= count
        {
            return Some(listed.instances);
        }
        if now() >= deadline {
            return None;
        }
        let _ = reactor.sleep_until(now() + 100_000_000).await;
    }
}

/// A connection to instance `instance` of `service`.
pub async fn connect<P>(
    directory: &DirectoryAsyncClient,
    service: &str,
    instance: String,
) -> Option<ClientEnd<P>> {
    let (client, server) = endpoints::<P>().ok()?;
    let connected = directory
        .connect(service.into(), Some(instance), server.into_handle())
        .await;
    match connected {
        Ok(Ok(_)) => Some(client),
        _ => failed("connecting to a service"),
    }
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let spawner = executor.spawner().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    if let Some(log) = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID)
    {
        mikros_rt::log::set_handle(log.handle.raw());
    }
    let header = managed.header;
    // SAFETY: nothing reads it yet.
    unsafe { PROCESS = header.process.raw() };
    let directory = DirectoryAsyncClient::new(header.directory, reactor.clone());
    let passed = executor.block_on(async {
        let display = display::paint(&directory, &reactor).await?;
        mikros_rt::log!(Info, "input-probe: painted");
        let watched = input::works(&directory, &reactor, &spawner).await?;
        drop(display);
        mikros_rt::log!(Info, "input-probe: closed the display");
        watched.pressed_b(&reactor).await
    });
    if passed.is_some() {
        mikros_rt::log!(Info, "input-probe: PASSED");
    }
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}
