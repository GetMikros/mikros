//! `mikros.session.Elevation`, which the session manager serves to the
//! processes of login sessions: `Elevate` asks an administrator, as
//! `Authorize` does for the action `admin.run`, and marks the caller as
//! elevated by that administrator through the process manager's
//! `Elevations`. Each `Elevate` is logged.

use crate::authorize::Requests;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::exec::Spawner;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::session::{
    Elevation, ElevationAsyncServer, ElevationElevateResponse, ElevationRequest,
};

/// The action that `Elevate` asks for.
const ADMIN_RUN: &str = "admin.run";

/// Serves `Elevation` on `end` for `client`, each `Elevate` in a task of
/// its own, which replies nothing when the client terminated meanwhile.
pub async fn serve(
    requests: Rc<Requests>,
    end: ServerEnd<Elevation>,
    client: ClientIdentity,
    spawner: Spawner,
) {
    let server = Rc::new(ElevationAsyncServer::new(end, requests.reactor.clone()));
    let client = Rc::new(client);
    while let Ok(ElevationRequest::Elevate(q, r)) = server.next().await {
        let (requests, server, client) = (requests.clone(), server.clone(), client.clone());
        spawner.spawn(async move {
            if let Some(elevated) = elevate(&requests, &client, q.reason).await {
                let reply = elevated.map(|admin| ElevationElevateResponse { admin });
                let _ = r.reply(server.server(), reply);
            }
        });
    }
}

/// `Elevate` of `client` giving `reason`: the administrator who allowed
/// it, ACCESS_DENIED when none did or the client is in no login
/// session, or `None` when the client terminated meanwhile.
async fn elevate(
    requests: &Requests,
    client: &ClientIdentity,
    reason: String,
) -> Option<Result<u32, Status>> {
    let pid = client.pid;
    let info = match requests.launcher.get_info(pid).await {
        Ok(Ok(info)) => info.info,
        _ => return Some(Err(Status::AccessDenied)),
    };
    let path = info.path;
    if info.login_session == 0 {
        mikros_rt::log!(
            Info,
            "sessionmgr: Elevate of process {pid} ({path}) for {reason}: refused, no login session"
        );
        return Some(Err(Status::AccessDenied));
    }
    let answer = requests
        .authorize(client, ADMIN_RUN.into(), reason.clone())
        .await;
    let elevated = match answer {
        None => return None,
        Some((false, _)) => Err(Status::AccessDenied),
        Some((true, admin)) => mark(requests, client.koid, admin).await,
    };
    match &elevated {
        Ok(admin) => mikros_rt::log!(
            Info,
            "sessionmgr: Elevate of process {pid} ({path}) for {reason}: allowed by user {admin}"
        ),
        Err(status) => mikros_rt::log!(
            Info,
            "sessionmgr: Elevate of process {pid} ({path}) for {reason}: refused ({status:?})"
        ),
    }
    Some(elevated)
}

/// Marks the process with the kernel object ID `koid`, the caller's, as
/// elevated by `admin`, which a process elevated before keeps: then the
/// administrator who did. A process with the caller's process ID that
/// started after the caller ended is never marked.
async fn mark(requests: &Requests, koid: u64, admin: u32) -> Result<u32, Status> {
    let elevations = requests.elevations.borrow().clone();
    let elevations = elevations.ok_or(Status::BadState)?;
    match elevations.set_admin(koid, admin).await {
        Ok(Ok(_)) => Ok(admin),
        Ok(Err(Status::BadState)) => match requests.admin_of(koid).await {
            0 => Err(Status::BadState),
            earlier => Ok(earlier),
        },
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
}
