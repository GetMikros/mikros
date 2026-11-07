//! Serving one client's `Accounts` connection. Each request runs as a
//! task of its own, so that one that waits, such as a delayed attempt
//! or an administrator's authorization, does not hold up the others.

use crate::service::{Secret, Service};
use alloc::rc::Rc;
use mikros_rt::exec::Spawner;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use mikros_rt::interfaces::mikros::session::{
    Accounts, AccountsAsyncServer, AccountsAuthenticateResponse, AccountsCreateUserResponse,
    AccountsDeleteUserResponse, AccountsListUsersResponse, AccountsRequest,
    AccountsSetFullNameResponse, AccountsSetKindResponse, AccountsSetPasswordResponse, User,
    UserKind,
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
                AccountsRequest::CreateUser(q, r) => {
                    let password = Secret(q.password);
                    let created = v
                        .create_user(c, &q.name, &q.full_name, q.kind, password)
                        .await;
                    r.reply(s, created.map(|user| AccountsCreateUserResponse { user }))
                }
                AccountsRequest::DeleteUser(q, r) => {
                    let deleted = v.delete_user(c, q.user, q.delete_home).await;
                    r.reply(s, deleted.map(|()| AccountsDeleteUserResponse {}))
                }
                AccountsRequest::SetPassword(q, r) => {
                    let (old, new) = (Secret(q.old_password), Secret(q.new_password));
                    let set = v.set_password(c, q.user, old, new).await;
                    r.reply(s, set.map(|()| AccountsSetPasswordResponse {}))
                }
                AccountsRequest::SetKind(q, r) => {
                    let set = v.set_kind(c, q.user, q.kind).await;
                    r.reply(s, set.map(|()| AccountsSetKindResponse {}))
                }
                AccountsRequest::SetFullName(q, r) => {
                    let set = v.set_full_name(c, q.user, &q.full_name).await;
                    r.reply(s, set.map(|()| AccountsSetFullNameResponse {}))
                }
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
