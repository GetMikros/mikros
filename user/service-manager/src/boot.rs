//! The boot handles that init gives the service manager, and the
//! duplicates of them that it gives the services whose manifests list
//! them.

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
const WRITE: u32 = 1 << 3;

/// The handles init gave the service manager, which it keeps for as
/// long as it runs.
pub struct BootHandles {
    root_resource: Option<OwnedHandle>,
    log: Option<OwnedHandle>,
    root_budget: Option<OwnedHandle>,
    framebuffer: Option<OwnedHandle>,
    platform_info: Option<OwnedHandle>,
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
        }
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
