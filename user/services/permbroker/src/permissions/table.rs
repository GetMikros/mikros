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
    ("admin.accounts.manage", Kind::System),
    ("admin.logs.read", Kind::System),
    ("admin.power.control", Kind::System),
    ("admin.services.control", Kind::System),
    ("admin.time.set", Kind::System),
    ("default.game_controllers.use", Kind::Default),
    ("default.gpu.use", Kind::Default),
    ("default.network.use", Kind::Default),
    ("default.realtime.use", Kind::Default),
    ("session.authorize", Kind::System),
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
    ("user.accessibility.use", Kind::User),
    ("user.autostart.use", Kind::User),
    ("user.background.use", Kind::User),
    ("user.background_controllers.use", Kind::User),
    ("user.bluetooth.use", Kind::User),
    ("user.camera.use", Kind::User),
    ("user.desktop.access", Kind::User),
    ("user.documents.access", Kind::User),
    ("user.downloads.access", Kind::User),
    ("user.full_disk.access", Kind::User),
    ("user.global_shortcuts.use", Kind::User),
    ("user.input.read", Kind::User),
    ("user.local_network.use", Kind::User),
    ("user.location.read", Kind::User),
    ("user.microphone.use", Kind::User),
    ("user.music.access", Kind::User),
    ("user.network_volumes.access", Kind::User),
    ("user.pictures.access", Kind::User),
    ("user.raw_hid.use", Kind::User),
    ("user.screen.read", Kind::User),
    ("user.videos.access", Kind::User),
    ("user.volumes.access", Kind::User),
];

/// The kind of `permission`, or `None` when the broker does not know it.
pub fn kind(permission: &str) -> Option<Kind> {
    PERMISSIONS
        .binary_search_by(|(name, _)| (*name).cmp(permission))
        .ok()
        .map(|at| PERMISSIONS[at].1)
}
