//! The boot handles that init gives the service manager, and the
//! duplicates of them that it gives the services whose manifests list
//! them; and the `PRIORITY` resource from which services get theirs.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_manifest::BootHandle;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::sys;

/// The `id`s of the boot handles among the `USER` startup handles, in
/// the service manager's startup and in a service's.
const ROOT_RESOURCE_ID: u32 = 1;
const LOG_ID: u32 = 2;
const ROOT_BUDGET_ID: u32 = 3;
const FRAMEBUFFER_ID: u32 = 4;
const PLATFORM_INFO_ID: u32 = 5;

const SAME_RIGHTS: u32 = 1 << 31;
const KIND_PRIORITY: u32 = 5;
/// The priorities above the default, which the service manager's own
/// `PRIORITY` resource covers.
const PRIORITY_FIRST: i64 = 17;
const PRIORITY_LAST: i64 = 31;
const WRITE: u32 = 1 << 3;

/// The handles init gave the service manager, which it keeps for as
/// long as it runs.
pub struct BootHandles {
    root_resource: Option<OwnedHandle>,
    log: Option<OwnedHandle>,
    root_budget: Option<OwnedHandle>,
    framebuffer: Option<OwnedHandle>,
    platform_info: Option<OwnedHandle>,
    /// The `PRIORITY` resource covering 17 to 31, made from the root
    /// resource.
    priority: Option<OwnedHandle>,
}

/// A new `PRIORITY` resource from `parent` covering `first` to `last`.
fn priority_resource(
    parent: &OwnedHandle,
    first: i64,
    last: i64,
) -> Result<OwnedHandle, sys::Status> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::resource_create(
            parent.raw(),
            KIND_PRIORITY,
            first as u64,
            (last - first + 1) as u64,
            0,
            &mut out,
        )
    };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the kernel just created the handle for us.
    Ok(unsafe { OwnedHandle::from_raw(out) })
}

impl BootHandles {
    /// Takes init's boot handles from `handles`.
    pub fn take(handles: &mut Vec<StartupHandle>) -> BootHandles {
        let mut take = |id| {
            let at = handles
                .iter()
                .position(|h| h.role == HandleRole::User && h.id == id)?;
            Some(handles.remove(at).handle)
        };
        BootHandles {
            root_resource: take(ROOT_RESOURCE_ID),
            log: take(LOG_ID),
            root_budget: take(ROOT_BUDGET_ID),
            framebuffer: take(FRAMEBUFFER_ID),
            platform_info: take(PLATFORM_INFO_ID),
            priority: None,
        }
    }

    /// Makes the `PRIORITY` resource covering 17 to 31 from the root
    /// resource, logging why when it cannot.
    pub fn make_priority(&mut self) {
        let Some(root) = &self.root_resource else {
            mikros_rt::log!(
                Warning,
                "without a root resource, no service gets a priority above 16"
            );
            return;
        };
        match priority_resource(root, PRIORITY_FIRST, PRIORITY_LAST) {
            Ok(priority) => self.priority = Some(priority),
            Err(status) => {
                mikros_rt::log!(Error, "creating the PRIORITY resource failed: {status}")
            }
        }
    }

    /// The `PRIORITY` startup handle of a service whose manifest sets
    /// `priority`: none up to 16, and one covering 17 to `priority` up to
    /// 31. Fails above 31, or when there is no resource to make it from.
    pub fn priority(&self, priority: i64) -> Result<Option<StartupHandle>, String> {
        if priority > PRIORITY_LAST {
            return Err(format!("its priority {priority} is above 31"));
        }
        if priority < PRIORITY_FIRST {
            return Ok(None);
        }
        let own = self
            .priority
            .as_ref()
            .ok_or_else(|| String::from("there is no PRIORITY resource for its priority"))?;
        let handle = priority_resource(own, PRIORITY_FIRST, priority)
            .map_err(|status| format!("creating its PRIORITY resource: {status}"))?;
        Ok(Some(StartupHandle {
            role: HandleRole::Priority,
            id: 0,
            handle,
        }))
    }

    /// The log, if init gave one.
    pub fn log(&self) -> Option<&OwnedHandle> {
        self.log.as_ref()
    }

    /// Duplicates of the boot handles in `wanted`, as a service's startup
    /// handles: each with the rights of the service manager's own, but
    /// the platform information without `WRITE`. A missing framebuffer
    /// is left out; any other missing handle fails.
    pub fn give(&self, wanted: &[BootHandle]) -> Result<Vec<StartupHandle>, String> {
        let mut given = Vec::new();
        for &which in wanted {
            let (own, id, name) = match which {
                BootHandle::RootResource => {
                    (&self.root_resource, ROOT_RESOURCE_ID, "root_resource")
                }
                BootHandle::LogManage => (&self.log, LOG_ID, "log_manage"),
                BootHandle::RootBudget => (&self.root_budget, ROOT_BUDGET_ID, "root_budget"),
                BootHandle::Framebuffer => (&self.framebuffer, FRAMEBUFFER_ID, "framebuffer"),
                BootHandle::PlatformInfo => {
                    (&self.platform_info, PLATFORM_INFO_ID, "platform_info")
                }
            };
            let Some(own) = own else {
                if which == BootHandle::Framebuffer {
                    continue;
                }
                return Err(format!("init gave no {name}"));
            };
            let rights = if which == BootHandle::PlatformInfo {
                let (_, rights) = mikros_rt::midl::handle_info(own.raw())
                    .ok_or_else(|| format!("inspecting {name}"))?;
                rights & !WRITE
            } else {
                SAME_RIGHTS
            };
            let mut copy = 0;
            // SAFETY: the out pointer is valid for the call.
            let status = unsafe { sys::handle_duplicate(own.raw(), rights, &mut copy) };
            if status != mikros_rt::OK {
                return Err(format!("duplicating {name}: {status}"));
            }
            given.push(StartupHandle {
                role: HandleRole::User,
                id,
                // SAFETY: the kernel just created the handle for us.
                handle: unsafe { OwnedHandle::from_raw(copy) },
            });
        }
        Ok(given)
    }
}
