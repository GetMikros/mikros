//! The budgets of directories, which the memory of the files below them
//! is charged to, and the `TemporaryFilesystem` service that sets them.

use crate::memfs::{Id, ROOT, Resolved};
use crate::service::fs::Fs;
use alloc::rc::Rc;
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::{
    Error, TemporaryFilesystem, TemporaryFilesystemAsyncServer, TemporaryFilesystemRequest,
    TemporaryFilesystemSetBudgetResponse,
};

const WRITE: u32 = 1 << 3;

impl Fs {
    /// The budget of the nearest directory above `id` that has one.
    pub fn budget_of(&self, id: Id) -> Option<Rc<OwnedHandle>> {
        let budgets = self.budgets.borrow();
        if budgets.is_empty() {
            return None;
        }
        let ancestors = self.tree.borrow().ancestors(id);
        ancestors.iter().find_map(|dir| budgets.get(dir).cloned())
    }

    /// Charges the memory of the files below the directory at `path`,
    /// from the root, to `budget`, which needs `WRITE`.
    fn set_budget(&self, path: &str, budget: OwnedHandle) -> Result<(), Error> {
        let (_, rights) = mikros_rt::midl::handle_info(budget.raw()).ok_or(Error::Invalid)?;
        if rights & WRITE == 0 {
            return Err(Error::AccessDenied);
        }
        let tree = self.tree.borrow();
        let dir = match tree
            .resolve(ROOT, path, false)
            .map_err(crate::service::fs::error)?
        {
            Resolved::Node(id) => id,
            // A link that leads out of the file system, or nothing.
            Resolved::Redirect(_) | Resolved::Missing { .. } => return Err(Error::NotFound),
        };
        if !tree.get(dir).is_some_and(|n| n.is_directory()) {
            return Err(Error::NotDirectory);
        }
        drop(tree);
        self.budgets.borrow_mut().insert(dir, Rc::new(budget));
        Ok(())
    }
}

/// Serves `TemporaryFilesystem` on `end`.
pub async fn serve(fs: Rc<Fs>, end: ServerEnd<TemporaryFilesystem>) {
    let server = TemporaryFilesystemAsyncServer::new(end, fs.reactor.clone());
    while let Ok(TemporaryFilesystemRequest::SetBudget(q, r)) = server.next().await {
        let set = fs.set_budget(&q.path, q.budget);
        let _ = r.reply(
            server.server(),
            set.map(|()| TemporaryFilesystemSetBudgetResponse {}),
        );
    }
}
