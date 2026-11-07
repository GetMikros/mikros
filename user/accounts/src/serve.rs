//! Serving one client's `Accounts` connection. Each request but
//! `ListUsers` runs as a task of its own, so that one that waits, such
//! as a delayed attempt, does not hold up the others. Changes are not
//! served yet.

use crate::service::{Secret, Service};
use alloc::rc::Rc;
use mikros_rt::exec::Spawner;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::session::{
    Accounts, AccountsAsyncServer, AccountsAuthenticateResponse, AccountsListUsersResponse,
    AccountsRequest, User, UserKind,
};
use mikros_userdb::{Kind, Record};

/// Serves the requests of `client` on `end` until it closes.
pub async fn serve(
    service: Rc<Service>,
    end: ServerEnd<Accounts>,
    client: ClientIdentity,
    spawner: Spawner,
) {
    let server = Rc::new(AccountsAsyncServer::new(end, service.reactor.clone()));
    let client = Rc::new(client);
    while let Ok(request) = server.next().await {
        if let AccountsRequest::ListUsers(q, r) = request {
            let _ = r.reply(server.server(), list(&service, q.after));
            continue;
        }
        let (service, server, client) = (service.clone(), server.clone(), client.clone());
        spawner.spawn(async move {
            let (v, s, c) = (&service, server.server(), &*client);
            let _ = match request {
                AccountsRequest::ListUsers(..) => Ok(()),
                AccountsRequest::Authenticate(q, r) => {
                    let password = Secret(q.password);
                    let user = v.authenticate(c, &q.name, password).await;
                    r.reply(s, user.map(|user| AccountsAuthenticateResponse { user }))
                }
                AccountsRequest::CreateUser(_, r) => r.reply(s, Err(Status::NotSupported)),
                AccountsRequest::DeleteUser(_, r) => r.reply(s, Err(Status::NotSupported)),
                AccountsRequest::SetPassword(_, r) => r.reply(s, Err(Status::NotSupported)),
                AccountsRequest::SetKind(_, r) => r.reply(s, Err(Status::NotSupported)),
                AccountsRequest::SetFullName(_, r) => r.reply(s, Err(Status::NotSupported)),
            };
        });
    }
}

/// The users with IDs above `after`, one page of them.
fn list(service: &Service, after: u32) -> AccountsListUsersResponse {
    let users = service.store.users();
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
