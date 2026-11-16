//! pkgsvc on Mikros: it serves `mikros.packages.Installer` on each
//! connection that the service manager forwards, on a thread of its own.

use mikros_mika::format::{self, Package};
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{Node, NodeClient};
use mikros_rt::interfaces::mikros::packages::{
    Installer, InstallerInspectResponse, InstallerRequest, InstallerServer,
};
use mikros_rt::interfaces::mikros::service::{Provider, ProviderRequest, ProviderServer};
use std::os::mikros::startup;
use std::process::ExitCode;

/// The role of the provider among the startup handles.
const PROVIDER: u32 = 4;
const INSTALLER: &str = "mikros.packages.Installer";
pub const FOREVER: i64 = i64::MAX;
/// The longest manifest that `Inspect` returns.
const MANIFEST_MAX: usize = 32768;

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

fn inspect(package: ClientEnd<Node>) -> Result<InstallerInspectResponse, Status> {
    let (_, _, manifest) = open(&Given(NodeClient::new(package)))?;
    if manifest.len() > MANIFEST_MAX {
        return Err(Status::InvalidArgs);
    }
    Ok(InstallerInspectResponse { manifest })
}

/// Serves `Installer` on `end` until it closes.
fn serve(end: ServerEnd<Installer>) {
    let server = InstallerServer::new(end);
    while let Ok(request) = server.next(FOREVER) {
        use InstallerRequest as R;
        let s = &server;
        let _ = match request {
            R::Inspect(q, r) => r.reply(s, inspect(q.package)),
            R::List(_, r) => r.reply(s, Err(Status::NotSupported)),
            R::Install(_, r) => r.reply(s, Err(Status::NotSupported)),
            R::Remove(_, r) => r.reply(s, Err(Status::NotSupported)),
            R::CreatePublic(_, r) => r.reply(s, Err(Status::NotSupported)),
            R::CommandOwner(_, r) => r.reply(s, Err(Status::NotSupported)),
        };
    }
}

pub fn main() -> ExitCode {
    let Some(provider) = startup::take_handle(PROVIDER, 0) else {
        eprintln!("pkgsvc: the provider is missing");
        return ExitCode::FAILURE;
    };
    // SAFETY: the handle is ours, and is handed over once.
    let provider = unsafe { OwnedHandle::from_raw(provider.into_raw_handle()) };
    let provider = ProviderServer::new(ServerEnd::<Provider>::new(provider));
    while let Ok(ProviderRequest::Connect(c)) = provider.next(FOREVER) {
        // Other protocols and named instances are not ours; dropping the
        // server end closes the connection.
        if c.instance.is_none() && c.protocol == INSTALLER {
            let end = ServerEnd::new(c.server);
            std::thread::spawn(move || serve(end));
        }
    }
    ExitCode::SUCCESS
}
