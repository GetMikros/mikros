//! The service functions of `<mikros/services.h>` (MIKSPEC-0068.R26),
//! through which applications reach the private protocols of the
//! service manager, the log service and the install service: each makes
//! one call, or a few, and returns its status as a `mikros_status_t`.

use crate::lock::Lock;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::ManuallyDrop;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::log::OwnReaderClient;
use mikros_rt::interfaces::mikros::packages::InstallerClient;
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::{
    DirectoryClient, ProviderRequest, ProviderServer, RegistrarClient, ServiceControlClient,
    ServicesClient,
};
use mikros_rt::sys;

const FOREVER: i64 = i64::MAX;
const OK: i64 = 0;
const REGISTRAR: &str = "mikros.service.Registrar";
const SERVICES: &str = "mikros.service.Services";
const SERVICE_CONTROL: &str = "mikros.service.ServiceControl";
const OWN_READER: &str = "mikros.log.OwnReader";
const INSTALLER: &str = "mikros.packages.Installer";
/// The bounds of the strings of the calls.
const PROTOCOL_MAX: usize = 128;
const INSTANCE_MAX: usize = 64;
const NAME_MAX: usize = 64;
const ID_MAX: usize = 128;
const COMPONENT_MAX: usize = 255;
/// The sizes of the string fields of the C structs, with their NULs.
const APP_ID_SIZE: usize = 129;
const MESSAGE_SIZE: usize = 1025;
/// The most records that one `OwnReader.Read` gives.
const RECORDS_MAX: u32 = 32;

/// The calling process's app ID, from its startup header.
static APP_ID: Lock<String> = Lock::new(String::new());

/// Keeps `app_id`, the process's own, for the component functions.
pub fn install(app_id: &str) {
    *APP_ID.lock() = app_id.into();
}

/// The status `status` as a `mikros_status_t`.
fn raw(status: Status) -> i64 {
    i64::from(status.into_raw())
}

/// The status of a reply, or `PEER_CLOSED` when the call itself failed.
fn replied<T>(reply: Result<Result<T, Status>, mikros_rt::interfaces::__midl::Error>) -> i64 {
    match reply {
        Ok(Ok(_)) => OK,
        Ok(Err(status)) => raw(status),
        Err(_) => raw(Status::PeerClosed),
    }
}

/// The NUL-terminated UTF-8 string at `at`, of at most `max` bytes:
/// `INVALID_ARGS` for a null pointer, a longer one or one that is not
/// UTF-8.
///
/// # Safety
///
/// `at` must be null or point to a NUL-terminated string.
unsafe fn string(at: *const u8, max: usize) -> Result<String, i64> {
    if at.is_null() {
        return Err(raw(Status::InvalidArgs));
    }
    let mut bytes = Vec::new();
    loop {
        // SAFETY: the string goes on until its NUL, as the caller ensures.
        let byte = unsafe { *at.add(bytes.len()) };
        if byte == 0 {
            break;
        }
        if bytes.len() == max {
            return Err(raw(Status::InvalidArgs));
        }
        bytes.push(byte);
    }
    String::from_utf8(bytes).map_err(|_| raw(Status::InvalidArgs))
}

/// Copies `text` into the buffer of `size` bytes at `to`, cut to fit,
/// ended by a NUL.
///
/// # Safety
///
/// `to` must point to `size` writable bytes.
unsafe fn copy_out(text: &str, to: *mut u8, size: usize) {
    let len = text.len().min(size - 1);
    // SAFETY: both ranges are valid, as the caller ensures for `to`.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), to, len);
        *to.add(len) = 0;
    }
}

/// The process's directory, which stays open: the client is never
/// dropped.
fn directory() -> ManuallyDrop<DirectoryClient> {
    // SAFETY: the C library keeps the directory open while the process
    // runs, and this client never closes it.
    let handle = unsafe { OwnedHandle::from_raw(crate::startup::directory()) };
    ManuallyDrop::new(DirectoryClient::new(ClientEnd::new(handle)))
}

/// A new channel: this end, and the end to give away.
fn channel() -> Result<(OwnedHandle, OwnedHandle), i64> {
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    let status = unsafe { sys::channel_create(0, &mut ours, &mut theirs) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the channel was just created for us.
    Ok(unsafe { (OwnedHandle::from_raw(ours), OwnedHandle::from_raw(theirs)) })
}

/// A connection to `protocol` through the directory.
fn connect<P>(protocol: &str) -> Result<ClientEnd<P>, i64> {
    let (ours, theirs) = channel()?;
    match replied(directory().connect(protocol.into(), None, theirs, FOREVER)) {
        OK => Ok(ClientEnd::new(ours)),
        status => Err(status),
    }
}

/// `mikros_service_connect`: `Directory.Connect` of `server` to
/// `protocol`, and `instance` unless it is null.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_connect(
    protocol: *const u8,
    instance: *const u8,
    server: sys::Handle,
) -> i64 {
    // SAFETY: the caller gives the handle over in every case.
    let server = unsafe { OwnedHandle::from_raw(server) };
    let named = || -> Result<(String, Option<String>), i64> {
        // SAFETY: as the caller ensures.
        let protocol = unsafe { string(protocol, PROTOCOL_MAX) }?;
        let instance = match instance.is_null() {
            true => None,
            // SAFETY: as the caller ensures.
            false => Some(unsafe { string(instance, INSTANCE_MAX) }?),
        };
        Ok((protocol, instance))
    };
    match named() {
        Ok((protocol, instance)) => {
            replied(directory().connect(protocol, instance, server, FOREVER))
        }
        Err(status) => status,
    }
}

/// `mikros_service_register`: registers `instance` of `protocol`,
/// without a permission, and gives the provider end of its connections.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_register(
    protocol: *const u8,
    instance: *const u8,
    provider: *mut sys::Handle,
) -> i64 {
    let registered = || -> Result<OwnedHandle, i64> {
        // SAFETY: as the caller ensures.
        let protocol = unsafe { string(protocol, PROTOCOL_MAX) }?;
        // SAFETY: as the caller ensures.
        let instance = unsafe { string(instance, INSTANCE_MAX) }?;
        let registrar = RegistrarClient::new(connect(REGISTRAR)?);
        let (ours, theirs) = channel()?;
        let reply = registrar.register(protocol, instance, None, ClientEnd::new(theirs), FOREVER);
        match replied(reply) {
            OK => Ok(ours),
            status => Err(status),
        }
    };
    if provider.is_null() {
        return raw(Status::InvalidArgs);
    }
    match registered() {
        Ok(ours) => {
            // SAFETY: the out pointer is valid, as the caller ensures.
            unsafe { *provider = ours.into_raw() };
            OK
        }
        Err(status) => status,
    }
}

/// `mikros_service_unregister`: removes the caller's `instance` of
/// `protocol`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_unregister(protocol: *const u8, instance: *const u8) -> i64 {
    let removed = || -> Result<i64, i64> {
        // SAFETY: as the caller ensures.
        let protocol = unsafe { string(protocol, PROTOCOL_MAX) }?;
        // SAFETY: as the caller ensures.
        let instance = unsafe { string(instance, INSTANCE_MAX) }?;
        let registrar = RegistrarClient::new(connect(REGISTRAR)?);
        Ok(replied(registrar.unregister(protocol, instance, FOREVER)))
    };
    removed().unwrap_or_else(|status| status)
}

/// `mikros_service_provider`: the startup handle of role `PROVIDER` of a
/// service, which the caller then owns.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_provider(provider: *mut sys::Handle) -> i64 {
    if provider.is_null() {
        return raw(Status::InvalidArgs);
    }
    match crate::startup::take_role(HandleRole::Provider) {
        Some(handle) => {
            // SAFETY: the out pointer is valid, as the caller ensures.
            unsafe { *provider = handle.into_raw() };
            OK
        }
        None => raw(Status::NotFound),
    }
}

/// The C form of a client identity (MIKSPEC-0036.R41).
#[repr(C)]
pub struct Client {
    pid: u32,
    user: u32,
    app_id: [u8; APP_ID_SIZE],
    koid: u64,
}

/// `mikros_service_accept`: waits for the next connection on `provider`
/// and gives its protocol, instance, server end and client.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_accept(
    provider: sys::Handle,
    protocol: *mut u8,
    instance: *mut u8,
    server: *mut sys::Handle,
    client: *mut Client,
) -> i64 {
    if protocol.is_null() || instance.is_null() || server.is_null() || client.is_null() {
        return raw(Status::InvalidArgs);
    }
    // SAFETY: the caller keeps owning `provider`; this server never
    // closes it.
    let end = unsafe { OwnedHandle::from_raw(provider) };
    let provider = ManuallyDrop::new(ProviderServer::new(ServerEnd::new(end)));
    let request = match provider.next(FOREVER) {
        Ok(request) => request,
        Err(_) => return raw(Status::PeerClosed),
    };
    let ProviderRequest::Connect(c) = request;
    let instance_name = c.instance.unwrap_or_default();
    // SAFETY: the buffers have the sizes of the header, as the caller
    // ensures, and the out pointers are valid.
    unsafe {
        copy_out(&c.protocol, protocol, PROTOCOL_MAX + 1);
        copy_out(&instance_name, instance, INSTANCE_MAX + 1);
        *server = c.server.into_raw();
        (*client).pid = c.client.pid;
        (*client).user = c.client.user;
        copy_out(&c.client.app_id, (*client).app_id.as_mut_ptr(), APP_ID_SIZE);
        (*client).koid = c.client.koid;
    }
    OK
}

/// The C form of a service's entry of `Services.List`.
#[repr(C)]
pub struct ServiceStatus {
    state: u32,
    pid: u32,
    restarts: u32,
    exit_status: i32,
    enabled: bool,
}

/// `mikros_service_status`: the entry of `Services.List` of the service
/// `name`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_status(name: *const u8, status: *mut ServiceStatus) -> i64 {
    if status.is_null() {
        return raw(Status::InvalidArgs);
    }
    let found = || -> Result<ServiceStatus, i64> {
        // SAFETY: as the caller ensures.
        let name = unsafe { string(name, NAME_MAX) }?;
        let services = ServicesClient::new(connect(SERVICES)?);
        let mut after = None;
        loop {
            let page = services
                .list(after.clone(), FOREVER)
                .map_err(|_| raw(Status::PeerClosed))?;
            if let Some(e) = page.entries.iter().find(|e| e.name == name) {
                return Ok(ServiceStatus {
                    state: e.state.into_raw(),
                    pid: e.pid,
                    restarts: e.restarts,
                    exit_status: e.exit_status,
                    enabled: e.enabled,
                });
            }
            match (page.more, page.entries.last()) {
                (true, Some(last)) => after = Some(last.name.clone()),
                _ => return Err(raw(Status::NotFound)),
            }
        }
    };
    match found() {
        Ok(found) => {
            // SAFETY: the out pointer is valid, as the caller ensures.
            unsafe { status.write(found) };
            OK
        }
        Err(status) => status,
    }
}

/// A call on `ServiceControl` for the service `name`.
fn control(name: *const u8, call: impl FnOnce(&ServiceControlClient, String) -> i64) -> i64 {
    let called = || -> Result<i64, i64> {
        // SAFETY: as the caller of the function ensures.
        let name = unsafe { string(name, NAME_MAX) }?;
        let control = ServiceControlClient::new(connect(SERVICE_CONTROL)?);
        Ok(call(&control, name))
    };
    called().unwrap_or_else(|status| status)
}

/// `mikros_service_start`: `ServiceControl.Start` of `name`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_start(name: *const u8) -> i64 {
    control(name, |c, name| replied(c.start(name, FOREVER)))
}

/// `mikros_service_stop`: `ServiceControl.Stop` of `name`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_stop(name: *const u8) -> i64 {
    control(name, |c, name| replied(c.stop(name, FOREVER)))
}

/// `mikros_service_restart`: `ServiceControl.Restart` of `name`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_service_restart(name: *const u8) -> i64 {
    control(name, |c, name| replied(c.restart(name, FOREVER)))
}

/// The C form of a log record (MIKSPEC-0063.R2).
#[repr(C)]
pub struct LogRecord {
    sequence: u64,
    timestamp: i64,
    utc: i64,
    pid: u32,
    app_id: [u8; APP_ID_SIZE],
    severity: u32,
    message: [u8; MESSAGE_SIZE],
}

/// `mikros_own_log_read`: the records of `OwnReader.Read` after `after`, at
/// most `max`, and how many.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_own_log_read(
    after: u64,
    records: *mut LogRecord,
    max: u32,
    count: *mut u32,
) -> i64 {
    if count.is_null() || (records.is_null() && max > 0) {
        return raw(Status::InvalidArgs);
    }
    let read = || -> Result<u32, i64> {
        let reader = OwnReaderClient::new(connect(OWN_READER)?);
        let got = reader
            .read(after, max.min(RECORDS_MAX), FOREVER)
            .map_err(|_| raw(Status::PeerClosed))?
            .records;
        let mut n = 0;
        for r in got.iter().take(max as usize) {
            // SAFETY: `records` holds `max` records, as the caller
            // ensures, and `n` stays below it.
            unsafe {
                let to = records.add(n as usize);
                (*to).sequence = r.sequence;
                (*to).timestamp = r.timestamp;
                (*to).utc = r.utc;
                (*to).pid = r.pid;
                copy_out(&r.app_id, (*to).app_id.as_mut_ptr(), APP_ID_SIZE);
                (*to).severity = u32::from(r.severity.into_raw());
                copy_out(&r.message, (*to).message.as_mut_ptr(), MESSAGE_SIZE);
            }
            n += 1;
        }
        Ok(n)
    };
    match read() {
        Ok(n) => {
            // SAFETY: the out pointer is valid, as the caller ensures.
            unsafe { *count = n };
            OK
        }
        Err(status) => status,
    }
}

/// A connection to the install service.
fn installer() -> Result<InstallerClient, i64> {
    Ok(InstallerClient::new(connect(INSTALLER)?))
}

/// `mikros_public_create`: `CreatePublic` for the caller's app.
#[unsafe(no_mangle)]
extern "C" fn mikros_public_create() -> i64 {
    match installer() {
        Ok(installer) => replied(installer.create_public(FOREVER)),
        Err(status) => status,
    }
}

/// `mikros_public_open`: `OpenPublic` of the app `id`'s public folder
/// on `server`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_public_open(id: *const u8, server: sys::Handle) -> i64 {
    // SAFETY: the caller gives the handle over.
    let server = unsafe { OwnedHandle::from_raw(server) };
    let opened = || -> Result<i64, i64> {
        // SAFETY: as the caller ensures.
        let id = unsafe { string(id, ID_MAX) }?;
        let reply = installer()?.open_public(id, ServerEnd::new(server), FOREVER);
        Ok(replied(reply))
    };
    opened().unwrap_or_else(|status| status)
}

/// `mikros_component_add`: `AddComponent` of the kept component
/// `component` of the caller's app.
#[unsafe(no_mangle)]
unsafe extern "C" fn mikros_component_add(component: *const u8) -> i64 {
    let added = || -> Result<i64, i64> {
        // SAFETY: as the caller ensures.
        let component = unsafe { string(component, COMPONENT_MAX) }?;
        let id = APP_ID.lock().clone();
        Ok(replied(installer()?.add_component(id, component, FOREVER)))
    };
    added().unwrap_or_else(|status| status)
}

/// `mikros_component_install`: `InstallComponent` of the package file
/// `package` for the caller's app.
#[unsafe(no_mangle)]
extern "C" fn mikros_component_install(package: sys::Handle) -> i64 {
    // SAFETY: the caller gives the handle over.
    let package = unsafe { OwnedHandle::from_raw(package) };
    let id = APP_ID.lock().clone();
    match installer() {
        Ok(installer) => replied(installer.install_component(id, ClientEnd::new(package), FOREVER)),
        Err(status) => status,
    }
}
