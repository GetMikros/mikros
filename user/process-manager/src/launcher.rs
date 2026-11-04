//! The `Launcher` endpoint of each process the process manager starts.
//!
//! Each process has its own channel, so the record it is bound to is
//! the one it was created for. No method is implemented yet: each fails
//! with `NOT_SUPPORTED`, and `List`, which cannot fail, closes the
//! channel.

use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{Launcher, LauncherAsyncServer, LauncherRequest};

/// Serves the `Launcher` endpoint `end` of process `_pid` until it
/// closes.
pub async fn serve(end: ServerEnd<Launcher>, _pid: u32, reactor: Reactor) {
    use LauncherRequest as R;
    let server = LauncherAsyncServer::new(end, reactor);
    let s = server.server();
    let no = Status::NotSupported;
    while let Ok(request) = server.next().await {
        let _ = match request {
            R::Spawn(_, r) => r.reply(s, Err(no)),
            R::Exec(_, r) => r.reply(s, Err(no)),
            R::Kill(_, r) => r.reply(s, Err(no)),
            R::Wait(_, r) => r.reply(s, Err(no)),
            R::GetInfo(_, r) => r.reply(s, Err(no)),
            R::CreateSession(_, r) => r.reply(s, Err(no)),
            R::SetProcessGroup(_, r) => r.reply(s, Err(no)),
            R::KillGroup(_, r) => r.reply(s, Err(no)),
            R::Fork(_, r) => r.reply(s, Err(no)),
            R::KillLoginSession(_, r) => r.reply(s, Err(no)),
            // TODO: list the processes once the records are shared with
            // the Launcher endpoints.
            R::List(..) => return,
        };
    }
}
