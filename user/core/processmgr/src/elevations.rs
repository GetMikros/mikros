//! `mikros.process.Elevations`, which the process manager serves to the
//! session manager: the administrator who elevated each process, which
//! its children and the programs it execs keep.

use crate::processes::Shared;
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{
    Elevations, ElevationsAsyncServer, ElevationsGetAdminResponse, ElevationsRequest,
    ElevationsSetAdminResponse,
};
use mikros_rt::interfaces::mikros::service::{ClientIdentity, Directory};

/// Serves `Elevations` on `end` until it closes.
pub async fn serve(shared: Rc<Shared>, end: ServerEnd<Elevations>) {
    let server = ElevationsAsyncServer::new(end, shared.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            ElevationsRequest::SetAdmin(q, r) => {
                let set = set_admin(&shared, q.pid, q.admin).await;
                r.reply(s, set.map(|()| ElevationsSetAdminResponse {}))
            }
            ElevationsRequest::GetAdmin(q, r) => {
                let records = shared.records.borrow();
                let admin = records.get(q.pid).map(|r| r.admin).ok_or(Status::NotFound);
                r.reply(s, admin.map(|admin| ElevationsGetAdminResponse { admin }))
            }
        };
    }
}

/// Marks running process `pid` as elevated by administrator `admin`,
/// once: it fails with `BAD_STATE` when it is marked already. Before it
/// returns, the process's `Directory` is bound again with the identity
/// that carries `admin`, so that its later connections do.
async fn set_admin(shared: &Shared, pid: u32, admin: u32) -> Result<(), Status> {
    if admin == 0 {
        return Err(Status::InvalidArgs);
    }
    let identity = {
        let mut records = shared.records.borrow_mut();
        let record = records
            .get_mut(pid)
            .filter(|r| r.exit.is_none())
            .ok_or(Status::NotFound)?;
        if record.admin != 0 {
            return Err(Status::BadState);
        }
        record.admin = admin;
        ClientIdentity {
            pid,
            user: record.user,
            app_id: record.app_id.clone(),
            admin,
            koid: record.koid,
        }
    };
    let control = shared.control.borrow().clone().ok_or(Status::BadState)?;
    let (client, server) = endpoints::<Directory>().map_err(|_| Status::NoResources)?;
    crate::processes::bind(&control, identity, server).await?;
    if let Some(record) = shared.records.borrow_mut().get_mut(pid) {
        record.rebound = Some(client.into_handle());
    }
    Ok(())
}
