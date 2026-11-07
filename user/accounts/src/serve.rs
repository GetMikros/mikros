//! Serving one client's `Accounts` connection.

use crate::store::Store;
use alloc::rc::Rc;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::{
    Accounts, AccountsAsyncServer, AccountsListUsersResponse, AccountsRequest, User, UserKind,
};
use mikros_userdb::{Kind, Record};

/// Serves the requests on `end` until it closes. Only `ListUsers` is
/// served yet.
pub async fn serve(store: Rc<Store>, end: ServerEnd<Accounts>, reactor: Reactor) {
    let server = AccountsAsyncServer::new(end, reactor);
    while let Ok(request) = server.next().await {
        let s = server.server();
        let _ = match request {
            AccountsRequest::ListUsers(q, r) => r.reply(s, list(&store, q.after)),
            AccountsRequest::Authenticate(_, r) => r.reply(s, Err(Status::NotSupported)),
            AccountsRequest::CreateUser(_, r) => r.reply(s, Err(Status::NotSupported)),
            AccountsRequest::DeleteUser(_, r) => r.reply(s, Err(Status::NotSupported)),
            AccountsRequest::SetPassword(_, r) => r.reply(s, Err(Status::NotSupported)),
            AccountsRequest::SetKind(_, r) => r.reply(s, Err(Status::NotSupported)),
            AccountsRequest::SetFullName(_, r) => r.reply(s, Err(Status::NotSupported)),
        };
    }
}

/// The users with IDs above `after`, one page of them.
fn list(store: &Store, after: u32) -> AccountsListUsersResponse {
    let users = store.users();
    AccountsListUsersResponse {
        users: users.list(after).map(user).collect(),
    }
}

/// A record as the protocol shows it, without its password hash.
fn user(r: &Record) -> User {
    User {
        id: r.id,
        name: r.name.clone(),
        full_name: r.full_name.clone(),
        kind: match r.kind {
            Kind::Standard => UserKind::Standard,
            Kind::Admin => UserKind::Admin,
        },
    }
}
