//! The permissions that the broker knows, and their kinds.

/// Who grants a permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Only manifests; never a user.
    System,
    /// The user.
    User,
    /// Every app, unless the user denies it.
    Default,
}

/// Every known permission and its kind, sorted by name.
pub const PERMISSIONS: &[(&str, Kind)] = &[
    ("accessibility", Kind::User),
    ("accounts.manage", Kind::System),
    ("autostart", Kind::User),
    ("background.run", Kind::User),
    ("camera", Kind::User),
    ("files.desktop", Kind::User),
    ("files.documents", Kind::User),
    ("files.downloads", Kind::User),
    ("files.full_disk", Kind::User),
    ("files.music", Kind::User),
    ("files.network_volumes", Kind::User),
    ("files.pictures", Kind::User),
    ("files.videos", Kind::User),
    ("files.volumes", Kind::User),
    ("gpu.render", Kind::Default),
    ("input.game_controllers", Kind::Default),
    ("input.game_controllers_background", Kind::User),
    ("input.hid_raw", Kind::User),
    ("input.monitoring", Kind::User),
    ("location", Kind::User),
    ("logs.read", Kind::System),
    ("microphone", Kind::User),
    ("nearby.bluetooth", Kind::User),
    ("nearby.local_network", Kind::User),
    ("network", Kind::Default),
    ("power.control", Kind::System),
    ("scheduling.realtime", Kind::Default),
    ("screen.capture", Kind::User),
    ("services.control", Kind::System),
    ("session.authorize", Kind::System),
    ("shortcuts.global", Kind::User),
    ("system.boot_state.set", Kind::System),
    ("system.devices.manage", Kind::System),
    ("system.diagnostics.read", Kind::System),
    ("system.display.control", Kind::System),
    ("system.filesystems.mount", Kind::System),
    ("system.gpu_priority.use", Kind::System),
    ("system.grants.manage", Kind::System),
    ("system.identity.set", Kind::System),
    ("system.input.control", Kind::System),
    ("system.log_sources.create", Kind::System),
    ("system.permissions.read", Kind::System),
    ("system.permissions.set", Kind::System),
    ("system.processes.control", Kind::System),
    ("system.processes.debug", Kind::System),
    ("system.processes.read", Kind::System),
    ("system.prompts.use", Kind::System),
    ("system.raw_storage.use", Kind::System),
    ("system.services.create", Kind::System),
    ("system.subvolumes.manage", Kind::System),
    ("system.unions.create", Kind::System),
    ("system.user_id.set", Kind::System),
    ("system.users.authenticate", Kind::System),
    ("time.set", Kind::System),
];

/// The kind of `permission`, or `None` when the broker does not know it.
pub fn kind(permission: &str) -> Option<Kind> {
    PERMISSIONS
        .binary_search_by(|(name, _)| (*name).cmp(permission))
        .ok()
        .map(|at| PERMISSIONS[at].1)
}
