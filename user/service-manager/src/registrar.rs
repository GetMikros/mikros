//! The `Registrar`, through which processes add named instances of
//! protocols to the directory.

use crate::directory::Dir;
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Registrar, RegistrarAsyncServer, RegistrarRegisterResponse, RegistrarRequest,
    RegistrarUnregisterResponse,
};

/// Serves `Registrar` on `end` for `caller`, whom the directory has
/// already checked for the permission, until it closes.
pub fn serve(dir: Rc<Dir>, end: ServerEnd<Registrar>, caller: ClientIdentity) {
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        let server = RegistrarAsyncServer::new(end, dir.reactor().clone());
        while let Ok(request) = server.next().await {
            let s = server.server();
            let _ = match request {
                RegistrarRequest::Register(q, r) => {
                    let registered = dir
                        .register(&caller, q.protocol, q.instance, q.permission, q.provider)
                        .await;
                    r.reply(s, registered.map(|()| RegistrarRegisterResponse {}))
                }
                RegistrarRequest::Unregister(q, r) => {
                    let removed = dir.unregister(&caller, &q.protocol, &q.instance);
                    r.reply(s, removed.map(|()| RegistrarUnregisterResponse {}))
                }
            };
        }
    });
}
