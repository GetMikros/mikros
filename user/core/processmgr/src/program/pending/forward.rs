//! The two things a gate does with a call of the file system protocol
//! once it has the call (MIKSPEC-0094.R8): forward it to the folder's
//! connection, or fail it.

use mikros_rt::interfaces::mikros::fs::*;

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

/// A reply of the folder's connection, or `IO` when that connection
/// closed.
fn flat<T>(reply: Reply<T>) -> Result<T, Error> {
    reply.unwrap_or(Err(Error::Io))
}

/// Fails `request` with `error`, whatever it is.
pub fn refuse(request: NodeRequest, s: &NodeServer, error: Error) {
    use NodeRequest as R;
    // A reply that cannot be sent means that the client went away,
    // which the next read finds.
    macro_rules! fail {
        ($($variant:ident),*) => {
            match request {
                $(R::$variant(_, r) => { let _ = r.reply(s, Err(error)); })*
            }
        };
    }
    fail!(
        GetAttributes,
        SetAttributes,
        Clone,
        Sync,
        QueryFilesystem,
        Read,
        Write,
        Append,
        Truncate,
        GetMemoryObject,
        GetRevocableMemoryObject,
        SetOwner,
        GetAcl,
        Access,
        SetAcl,
        Restrict,
        Exchange,
        CloneRange,
        Lock,
        Unlock,
        ReadLink,
        Seek,
        ReadNext,
        WriteNext,
        Open,
        ReadDirectory,
        Unlink,
        GetToken,
        Rename,
        Link,
        CreateSymlink,
        Watch,
        Unwatch,
        BindSocket,
        ConnectSocket
    );
}

/// Forwards `request` to `b`, the connection that the gate bound to the
/// subject, and replies with its answer. The `server` ends of `Open` and
/// `Clone` go to it as they are, so that the folder's file system serves
/// the nodes itself; `Restrict` is the subject's alone to bind, and
/// fails with `BAD_STATE`.
pub async fn forward(b: &NodeAsyncClient, request: NodeRequest, s: &NodeServer) {
    use NodeRequest as R;
    let _ = match request {
        R::GetAttributes(_, r) => r.reply(s, flat(b.get_attributes().await)),
        R::SetAttributes(q, r) => r.reply(
            s,
            flat(
                b.set_attributes(q.mask, q.mode, q.accessed, q.modified)
                    .await,
            ),
        ),
        R::Clone(q, r) => r.reply(s, flat(b.clone(q.node, q.rights).await)),
        R::Sync(_, r) => r.reply(s, flat(b.sync().await)),
        R::QueryFilesystem(_, r) => r.reply(s, flat(b.query_filesystem().await)),
        R::Read(q, r) => r.reply(s, flat(b.read(q.offset, q.count).await)),
        R::Write(q, r) => r.reply(s, flat(b.write(q.offset, q.data).await)),
        R::Append(q, r) => r.reply(s, flat(b.append(q.data).await)),
        R::Truncate(q, r) => r.reply(s, flat(b.truncate(q.size).await)),
        R::GetMemoryObject(q, r) => r.reply(s, flat(b.get_memory_object(q.flags).await)),
        R::GetRevocableMemoryObject(q, r) => {
            r.reply(s, flat(b.get_revocable_memory_object(q.flags).await))
        }
        R::SetOwner(q, r) => r.reply(s, flat(b.set_owner(q.owner).await)),
        R::GetAcl(q, r) => r.reply(s, flat(b.get_acl(q.default).await)),
        R::Access(_, r) => r.reply(s, flat(b.access().await)),
        R::SetAcl(q, r) => r.reply(s, flat(b.set_acl(q.default, q.entries).await)),
        R::Restrict(_, r) => r.reply(s, Err(Error::BadState)),
        R::Exchange(q, r) => r.reply(s, flat(b.exchange(q.name, q.target, q.other_name).await)),
        R::CloneRange(q, r) => r.reply(
            s,
            flat(
                b.clone_range(q.source, q.source_offset, q.offset, q.length)
                    .await,
            ),
        ),
        R::Lock(q, r) => r.reply(s, flat(b.lock(q.kind, q.start, q.length, q.wait).await)),
        R::Unlock(q, r) => r.reply(s, flat(b.unlock(q.start, q.length).await)),
        R::ReadLink(_, r) => r.reply(s, flat(b.read_link().await)),
        R::Seek(q, r) => r.reply(s, flat(b.seek(q.offset, q.origin).await)),
        R::ReadNext(q, r) => r.reply(s, flat(b.read_next(q.count).await)),
        R::WriteNext(q, r) => r.reply(s, flat(b.write_next(q.data).await)),
        R::Open(q, r) => r.reply(
            s,
            flat(b.open(q.path, q.flags, q.mode, q.rights, q.node).await),
        ),
        R::ReadDirectory(q, r) => r.reply(s, flat(b.read_directory(q.cookie, q.attributes).await)),
        R::Unlink(q, r) => r.reply(s, flat(b.unlink(q.name, q.directory).await)),
        R::GetToken(_, r) => r.reply(s, flat(b.get_token().await)),
        R::Rename(q, r) => r.reply(s, flat(b.rename(q.name, q.target, q.new_name).await)),
        R::Link(q, r) => r.reply(s, flat(b.link(q.name, q.target, q.new_name).await)),
        R::CreateSymlink(q, r) => r.reply(s, flat(b.create_symlink(q.name, q.target).await)),
        R::Watch(q, r) => r.reply(s, flat(b.watch(q.mask, q.cookie, q.watcher).await)),
        R::Unwatch(q, r) => r.reply(s, flat(b.unwatch(q.cookie).await)),
        R::BindSocket(q, r) => r.reply(s, flat(b.bind_socket(q.name, q.mode, q.listener).await)),
        R::ConnectSocket(q, r) => r.reply(
            s,
            flat(b.connect_socket(q.name, q.r#type, q.connection).await),
        ),
    };
}
