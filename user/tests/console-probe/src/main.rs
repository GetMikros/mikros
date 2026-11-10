//! The boot service of `tests/console.img`, which stands in for the
//! session manager: it opens the display and the input devices, starts
//! the text console with a `Seat` and a `TextLogin` channel, gives it the
//! display and the devices through `Seat`, and serves `TextLogin`. A
//! login as `sip` with the password `secret` succeeds; the probe then
//! acts as the session on the terminal the console gave: it asks for
//! the cursor position, which the console's terminal answers, reads a
//! line that the runner types, and ends the session. It logs what
//! happens, and `PASSED` once all of it did.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::exec::{Executor, Reactor};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{Device as Gpu, DeviceAsyncClient as GpuClient, Display};
use mikros_rt::interfaces::mikros::input::{Device, Input, InputAsyncClient};
use mikros_rt::interfaces::mikros::process::{
    HandleRole, LauncherAsyncClient, SpawnRequest, StartupHandle,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::interfaces::mikros::session::{
    Seat, SeatAsyncClient, TextLogin, TextLoginAsyncServer, TextLoginLoginResponse,
    TextLoginNeedsSetupResponse, TextLoginRequest,
};
use mikros_rt::interfaces::mikros::terminal::{Terminal, TerminalAsyncClient};
use mikros_rt::{managed, sys};

mikros_rt::raw_main!(main);

const LOG_ID: u32 = 2;
const TEXT_LOGIN_ID: u32 = 3;
const SEAT_ID: u32 = 7;
const CONSOLE: &str = "/usr/bin/console";
const GPU: &str = "device.gpu";
const INPUT: &str = "device.input";
const TERMINAL: &str = "mikros.terminal.Terminal";
const NAME: &str = "sip";
const PASSWORD: &str = "secret";
const CALLER_USER: u32 = u32::MAX;
const FOREVER: i64 = i64::MAX;
const SECOND: i64 = 1_000_000_000;
/// The input devices of the test machine: the PS/2 keyboard and mouse
/// and the USB keyboard.
const INPUTS: usize = 3;
/// Bits of a terminal's local flags: canonical input and echo.
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

fn failed<T>(what: &str) -> Option<T> {
    mikros_rt::log!(Error, "console-probe: FAILED: {what}");
    None
}

async fn instances(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    service: &str,
    count: usize,
) -> Option<Vec<String>> {
    let deadline = now() + 120 * SECOND;
    loop {
        if let Ok(Ok(listed)) = directory.list(service.into()).await
            && listed.instances.len() >= count
        {
            return Some(listed.instances);
        }
        if now() >= deadline {
            return None;
        }
        let _ = reactor.sleep_until(now() + SECOND / 10).await;
    }
}

async fn connect<P>(
    directory: &DirectoryAsyncClient,
    service: &str,
    instance: Option<String>,
) -> Option<ClientEnd<P>> {
    let (client, server) = endpoints::<P>().ok()?;
    match directory
        .connect(service.into(), instance, server.into_handle())
        .await
    {
        Ok(Ok(_)) => Some(client),
        _ => None,
    }
}

/// A memory object of `arguments`: their count, no environment, and each
/// ended by a zero byte.
fn argument_memory(arguments: &[&str]) -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for a in arguments {
        bytes.extend_from_slice(a.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    let size = (bytes.len() as u64).next_multiple_of(4096);
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(size, 0, &mut mem) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer and length describe `bytes`.
    let written = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    (written == mikros_rt::OK).then_some(mem)
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return 1;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor.clone(), |_| {}));
    let Ok(managed) = received else {
        return 1;
    };
    if let Some(log) = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == LOG_ID)
    {
        mikros_rt::log::set_handle(log.handle.raw());
    }
    let header = managed.header;
    let directory = Rc::new(DirectoryAsyncClient::new(header.directory, reactor.clone()));
    let launcher = LauncherAsyncClient::new(header.launcher, reactor.clone());
    let passed = executor.block_on(probe(directory, launcher, reactor));
    if passed.is_some() {
        mikros_rt::log!(Info, "console-probe: PASSED");
    }
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(FOREVER) };
    }
}

async fn probe(
    directory: Rc<DirectoryAsyncClient>,
    launcher: LauncherAsyncClient,
    reactor: Reactor,
) -> Option<()> {
    let (seat, seat_server) = endpoints::<Seat>().ok()?;
    let (login_client, login_server) = endpoints::<TextLogin>().ok()?;
    let request = SpawnRequest {
        path: CONSOLE.into(),
        arguments: argument_memory(&[CONSOLE])?,
        handles: vec![
            StartupHandle {
                role: HandleRole::User,
                id: TEXT_LOGIN_ID,
                handle: login_client.into_handle(),
            },
            StartupHandle {
                role: HandleRole::User,
                id: SEAT_ID,
                handle: seat_server.into_handle(),
            },
        ],
        app_id: None,
        user: CALLER_USER,
        login_session: false,
    };
    match launcher.spawn(request).await {
        Ok(Ok(_)) => mikros_rt::log!(Info, "console-probe: started the console"),
        other => return failed(&alloc::format!("starting the console: {other:?}")),
    }
    let seat = SeatAsyncClient::new(seat, reactor.clone());
    // The display.
    let gpu = instances(&directory, &reactor, GPU, 1)
        .await?
        .into_iter()
        .next()?;
    let device = GpuClient::new(
        connect::<Gpu>(&directory, GPU, Some(gpu.clone())).await?,
        reactor.clone(),
    );
    let (display, server) = endpoints::<Display>().ok()?;
    if !matches!(device.open_display(server, None).await, Ok(Ok(_))) {
        return failed("OpenDisplay");
    }
    seat.add_display(gpu, display).ok()?;
    // The input devices, all accepted.
    for instance in instances(&directory, &reactor, INPUT, INPUTS).await? {
        let input = InputAsyncClient::new(
            connect::<Input>(&directory, INPUT, Some(instance.clone())).await?,
            reactor.clone(),
        );
        let (device, server) = endpoints::<Device>().ok()?;
        if !matches!(input.open(server, None).await, Ok(Ok(_))) {
            return failed("Open");
        }
        seat.add_input(instance, device, true).ok()?;
    }
    mikros_rt::log!(Info, "console-probe: gave the seat");
    serve_login(
        &directory,
        &reactor,
        ServerEnd::new(login_server.into_handle()),
    )
    .await
}

/// Serves `TextLogin` until a session has run and ended.
async fn serve_login(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    end: ServerEnd<TextLogin>,
) -> Option<()> {
    let server = TextLoginAsyncServer::new(end, reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        match request {
            TextLoginRequest::NeedsSetup(_, r) => {
                let _ = r.reply(s, TextLoginNeedsSetupResponse { needed: false });
            }
            TextLoginRequest::RunSetup(_, r) => {
                let _ = r.reply(s, Err(Status::BadState));
            }
            TextLoginRequest::Login(q, r) => {
                mikros_rt::log!(Info, "console-probe: Login as {}", q.name);
                if q.name != NAME || q.password != PASSWORD {
                    let _ = r.reply(s, Err(Status::AccessDenied));
                    continue;
                }
                let _ = r.reply(s, Ok(TextLoginLoginResponse {}));
                session(directory, reactor, q.terminal).await?;
                let _ = s.send_on_session_ended();
                mikros_rt::log!(Info, "console-probe: ended the session");
                return Some(());
            }
        }
    }
    failed("the console closed TextLogin")
}

/// Acts as the session on the terminal of `token`.
async fn session(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    token: OwnedHandle,
) -> Option<()> {
    let terminal = TerminalAsyncClient::new(
        connect::<Terminal>(directory, TERMINAL, None).await?,
        reactor.clone(),
    );
    let t = || duplicate(&token);
    // Without canonical input, the console's answer to the cursor position
    // request is readable as it comes.
    let mut attributes = terminal.get_attributes(t()?).await.ok()?.ok()?.attributes;
    let canonical = attributes.local_flags;
    attributes.local_flags &= !(ICANON | ECHO);
    let raw = mikros_rt::interfaces::mikros::terminal::When::Now;
    terminal
        .set_attributes(t()?, copy(&attributes), raw)
        .await
        .ok()?
        .ok()?;
    terminal
        .write(t()?, b"Welcome, sip.\r\n\x1b[6n".to_vec(), FOREVER)
        .await
        .ok()?
        .ok()?;
    let mut answer = Vec::new();
    let deadline = now() + 10 * SECOND;
    while !answer.ends_with(b"R") {
        let read = terminal.read(t()?, 32, deadline).await.ok()?;
        match read {
            Ok(r) => answer.extend(r.data),
            Err(_) => return failed("reading the cursor position report"),
        }
    }
    if answer != b"\x1b[2;1R" {
        return failed(&alloc::format!("the cursor position report {answer:?}"));
    }
    mikros_rt::log!(Info, "console-probe: the console reported the cursor");
    attributes.local_flags = canonical;
    terminal
        .set_attributes(t()?, attributes, raw)
        .await
        .ok()?
        .ok()?;
    terminal
        .write(t()?, b"$ ".to_vec(), FOREVER)
        .await
        .ok()?
        .ok()?;
    mikros_rt::log!(Info, "console-probe: waiting for a line");
    let line = terminal
        .read(t()?, 64, now() + 120 * SECOND)
        .await
        .ok()?
        .ok()?
        .data;
    if line != b"ls\n" {
        return failed(&alloc::format!("the typed line {line:?}"));
    }
    mikros_rt::log!(Info, "console-probe: read the typed line");
    Some(())
}

fn copy(
    a: &mikros_rt::interfaces::mikros::terminal::Attributes,
) -> mikros_rt::interfaces::mikros::terminal::Attributes {
    mikros_rt::interfaces::mikros::terminal::Attributes {
        input_flags: a.input_flags,
        output_flags: a.output_flags,
        control_flags: a.control_flags,
        local_flags: a.local_flags,
        control_chars: a.control_chars,
        input_speed: a.input_speed,
        output_speed: a.output_speed,
    }
}

fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), 1 << 31, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}
