//! `mikros.service.Control`, which the service manager serves to
//! processes with `power.control`, whom the directory has already checked
//! for it: `Shutdown` stops every service and powers the machine off or
//! restarts it, and replies only when that failed.

use crate::directory::Dir;
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{
    Control, ControlAsyncServer, ControlRequest, ControlShutdownResponse,
};

/// Serves `Control` on `end` until it closes.
pub fn serve(dir: Rc<Dir>, end: ServerEnd<Control>) {
    let spawner = dir.spawner().clone();
    spawner.spawn(async move {
        let server = ControlAsyncServer::new(end, dir.reactor().clone());
        while let Ok(ControlRequest::Shutdown(q, r)) = server.next().await {
            let shut_down = match dir.shutdown() {
                Some(shutdown) => shutdown(q.kind).await,
                None => Err(Status::BadState),
            };
            let _ = r.reply(
                server.server(),
                shut_down.map(|()| ControlShutdownResponse {}),
            );
        }
    });
}
