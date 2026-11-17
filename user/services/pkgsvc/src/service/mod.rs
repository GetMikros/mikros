//! pkgsvc on Mikros (MIKSPEC-0101.R45): it serves `mikros.packages.Installer` on each
//! connection that the service manager forwards, on a thread of its own,
//! and works on the root file system, which it connects to itself
//! (MIKSPEC-0101.R37).

mod commands;
mod drivers;
mod files;
mod install;
mod kept;
mod list;
mod package;
mod public;
mod remove;
mod services;

use files::{Root, Subvolumes};
use mikros_mika::format::{self, Package};
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient};
use mikros_rt::interfaces::mikros::packages::{
    Installer, InstallerCommandOwnerResponse, InstallerCommandsResponse,
    InstallerCreatePublicResponse, InstallerInspectResponse, InstallerInstallResponse,
    InstallerListResponse, InstallerOpenPublicResponse, InstallerOpenResponse,
    InstallerRemoveResponse, InstallerRequest, InstallerServer, InstallerServiceManifestResponse,
    InstallerServicesResponse,
};
use mikros_rt::interfaces::mikros::permissions::{Broker, BrokerClient, Decision};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryClient, Provider, ProviderRequest, ProviderServer,
};
use mikros_rt::sys;
use std::os::mikros::startup;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

/// The role of the provider among the startup handles.
const PROVIDER: u32 = 4;
const INSTALLER: &str = "mikros.packages.Installer";
/// The only caller of `Open`.
const PROCESS_MANAGER: &str = "system.processmgr";
const BROKER: &str = "mikros.permissions.Broker";
pub const BROKER_NUMBERS: &str = "mikros.permissions.AppNumbers";
pub const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
/// The longest manifest that `Inspect` returns.
const MANIFEST_MAX: usize = 32768;

/// What every connection shares: the service directory, and the root
/// file system once it is reached.
pub struct Shared {
    pub directory: Mutex<DirectoryClient>,
    pub root: Mutex<Option<Arc<Root>>>,
    pub subvolumes: Mutex<Option<Arc<Subvolumes>>>,
}

impl Shared {
    /// The root file system, connected to when first needed.
    pub fn root(&self) -> Result<Arc<Root>, Status> {
        let mut root = self.root.lock().map_err(|_| Status::Internal)?;
        if let Some(root) = root.as_ref() {
            return Ok(root.clone());
        }
        let directory = self.directory.lock().map_err(|_| Status::Internal)?;
        let connected = Arc::new(Root::connect(&directory)?);
        *root = Some(connected.clone());
        Ok(connected)
    }

    /// The root file system's subvolume management, connected to when
    /// first needed.
    pub fn subvolumes(&self) -> Result<Arc<Subvolumes>, Status> {
        let mut subvolumes = self.subvolumes.lock().map_err(|_| Status::Internal)?;
        if let Some(subvolumes) = subvolumes.as_ref() {
            return Ok(subvolumes.clone());
        }
        let directory = self.directory.lock().map_err(|_| Status::Internal)?;
        let connected = Arc::new(Subvolumes::connect(&directory)?);
        *subvolumes = Some(connected.clone());
        Ok(connected)
    }
}

/// Whether the broker allows `caller` `permission` for the package of
/// `manifest`, asked for with `Request`; not without an answer.
pub fn allowed(
    shared: &Shared,
    caller: &ClientIdentity,
    permission: &str,
    manifest: &Manifest,
    action: &str,
) -> Result<bool, Status> {
    let name = manifest.name.as_deref().unwrap_or(&manifest.id);
    let reason = format!("{action} {name} ({}) for every user", manifest.id);
    request(shared, caller, permission, &reason)
}

/// Whether the broker allows `caller` `permission`, asked for with
/// `Request` and `reason`; not without an answer.
pub fn request(
    shared: &Shared,
    caller: &ClientIdentity,
    permission: &str,
    reason: &str,
) -> Result<bool, Status> {
    let (client, server) = endpoints::<Broker>().map_err(|_| Status::NoResources)?;
    {
        let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
        match directory.connect(BROKER.into(), None, server.into_handle(), FOREVER) {
            Ok(Ok(_)) => {}
            _ => return Ok(false),
        }
    }
    let broker = BrokerClient::new(client);
    let identity = ClientIdentity {
        pid: caller.pid,
        user: caller.user,
        app_id: caller.app_id.clone(),
        admin: caller.admin,
        koid: caller.koid,
        portable_checksum: caller.portable_checksum.clone(),
    };
    let requested = broker.request(identity, permission.into(), reason.into(), FOREVER);
    Ok(matches!(requested, Ok(Ok(r)) if r.decision == Decision::Allowed))
}

/// A package that a client gave, read through its connection.
pub struct Given(pub NodeClient);

impl format::Source for Given {
    fn size(&self) -> u64 {
        match self.0.get_attributes(FOREVER) {
            Ok(Ok(a)) => a.attributes.size,
            _ => 0,
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> format::Result<()> {
        let mut at = 0;
        while at < buf.len() {
            let count = (buf.len() - at).min(32768) as u32;
            let data = match self.0.read(offset + at as u64, count, FOREVER) {
                Ok(Ok(r)) => r.data,
                _ => return Err(format::Error::Io),
            };
            if data.is_empty() {
                return Err(format::Error::Io);
            }
            buf[at..at + data.len()].copy_from_slice(&data);
            at += data.len();
        }
        Ok(())
    }
}

/// The package that `package` gives, checked, with its manifest, checked
/// against it: `INVALID_ARGS` for one that breaks the format or whose
/// manifest is not valid.
pub fn open(package: &Given) -> Result<(Package, Manifest, Vec<u8>), Status> {
    let checked = Package::open(package).map_err(|e| match e {
        format::Error::Io => Status::Io,
        format::Error::Invalid(_) => Status::InvalidArgs,
    })?;
    let entry = checked.find(format::MANIFEST).ok_or(Status::InvalidArgs)?;
    let bytes = checked
        .read(package, entry)
        .map_err(|_| Status::InvalidArgs)?;
    let manifest = Manifest::parse(&bytes).map_err(|_| Status::InvalidArgs)?;
    manifest
        .check_package(&checked)
        .map_err(|_| Status::InvalidArgs)?;
    Ok((checked, manifest, bytes))
}

/// Checks the package that `package` gives, serves its files on `server`
/// and returns its manifest, for the process manager only
/// (MIKSPEC-0101.R45).
fn open_package(
    caller: &ClientIdentity,
    package: ClientEnd<Node>,
    server: ServerEnd<Node>,
) -> Result<InstallerOpenResponse, Status> {
    if caller.app_id != PROCESS_MANAGER {
        return Err(Status::AccessDenied);
    }
    let given = Given(NodeClient::new(package));
    let (checked, _, manifest) = open(&given)?;
    if manifest.len() > MANIFEST_MAX {
        return Err(Status::InvalidArgs);
    }
    package::serve_package(checked, given, server);
    Ok(InstallerOpenResponse { manifest })
}

fn inspect(package: ClientEnd<Node>) -> Result<InstallerInspectResponse, Status> {
    let (_, _, manifest) = open(&Given(NodeClient::new(package)))?;
    if manifest.len() > MANIFEST_MAX {
        return Err(Status::InvalidArgs);
    }
    Ok(InstallerInspectResponse { manifest })
}

/// Serves `Installer` on `end` for `caller` until it closes.
fn serve(shared: Arc<Shared>, end: ServerEnd<Installer>, caller: ClientIdentity) {
    let server = InstallerServer::new(end);
    while let Ok(request) = server.next(FOREVER) {
        use InstallerRequest as R;
        let s = &server;
        let _ = match request {
            R::Inspect(q, r) => r.reply(s, inspect(q.package)),
            R::List(q, r) => r.reply(
                s,
                list::list(&shared, &caller, &q.after)
                    .map(|packages| InstallerListResponse { packages }),
            ),
            R::Install(q, r) => r.reply(
                s,
                install::install(&shared, &caller, q).map(|()| InstallerInstallResponse {}),
            ),
            R::Remove(q, r) => r.reply(
                s,
                remove::remove(&shared, &caller, &q.id, q.global, q.keep_data)
                    .map(|()| InstallerRemoveResponse {}),
            ),
            R::CreatePublic(_, r) => r.reply(
                s,
                public::create(&shared, &caller).map(|()| InstallerCreatePublicResponse {}),
            ),
            R::OpenPublic(q, r) => r.reply(
                s,
                public::open(&shared, &caller, &q.id, q.server)
                    .map(|()| InstallerOpenPublicResponse {}),
            ),
            R::CommandOwner(q, r) => r.reply(
                s,
                commands::command_owner(&shared, &caller, &q.name, q.user)
                    .map(|id| InstallerCommandOwnerResponse { id }),
            ),
            R::Open(q, r) => r.reply(s, open_package(&caller, q.package, q.server)),
            R::Services(q, r) => r.reply(
                s,
                services::list(&shared, &caller, &q.after)
                    .map(|services| InstallerServicesResponse { services }),
            ),
            R::ServiceManifest(q, r) => r.reply(
                s,
                services::manifest(&shared, &caller, &q.name)
                    .map(|manifest| InstallerServiceManifestResponse { manifest }),
            ),
            R::Commands(q, r) => r.reply(
                s,
                commands::commands(&shared, &caller, q.server)
                    .map(|()| InstallerCommandsResponse {}),
            ),
        };
    }
}

fn duplicate(handle: sys::Handle) -> Option<OwnedHandle> {
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle, SAME_RIGHTS, &mut raw) };
    // SAFETY: the kernel just made the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(raw) })
}

pub fn main() -> ExitCode {
    let Some(provider) = startup::take_handle(PROVIDER, 0) else {
        eprintln!("pkgsvc: the provider is missing");
        return ExitCode::FAILURE;
    };
    // SAFETY: the handle is ours, and is handed over once.
    let provider = unsafe { OwnedHandle::from_raw(provider.into_raw_handle()) };
    let Some(directory) = duplicate(std::os::mikros::directory().as_raw_handle()) else {
        eprintln!("pkgsvc: the service directory is missing");
        return ExitCode::FAILURE;
    };
    let shared = Arc::new(Shared {
        directory: Mutex::new(DirectoryClient::new(ClientEnd::new(directory))),
        root: Mutex::new(None),
        subvolumes: Mutex::new(None),
    });
    let provider = ProviderServer::new(ServerEnd::<Provider>::new(provider));
    while let Ok(ProviderRequest::Connect(c)) = provider.next(FOREVER) {
        // Other protocols and named instances are not ours; dropping the
        // server end closes the connection.
        if c.instance.is_none() && c.protocol == INSTALLER {
            let shared = shared.clone();
            let end = ServerEnd::new(c.server);
            std::thread::spawn(move || serve(shared, end, c.client));
        }
    }
    ExitCode::SUCCESS
}
