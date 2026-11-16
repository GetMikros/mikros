//! The permissions that the broker knows, and their scopes.

/// Who grants a permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only manifests; never a user.
    System,
    /// Manifests, elevation, and an administrator for one process; never
    /// stored.
    Admin,
    /// The user.
    User,
    /// Every app, unless the user denies it.
    Default,
}

/// Every known permission and its scope, sorted by name.
pub const PERMISSIONS: &[(&str, Scope)] = &[
    ("admin.accounts.manage", Scope::Admin),
    ("admin.grants.manage", Scope::Admin),
    ("admin.logs.read", Scope::Admin),
    ("admin.power.control", Scope::Admin),
    ("admin.services.control", Scope::Admin),
    ("admin.time.set", Scope::Admin),
    ("admin.volumes.mount", Scope::Admin),
    ("default.game_controllers.use", Scope::Default),
    ("default.gpu.use", Scope::Default),
    ("default.network.use", Scope::Default),
    ("default.realtime.use", Scope::Default),
    ("system.boot_state.set", Scope::System),
    ("system.devices.manage", Scope::System),
    ("system.diagnostics.read", Scope::System),
    ("system.display.control", Scope::System),
    ("system.filesystems.mount", Scope::System),
    ("system.gpu_priority.use", Scope::System),
    ("system.grants.manage", Scope::System),
    ("system.identity.set", Scope::System),
    ("system.input.control", Scope::System),
    ("system.log_sources.create", Scope::System),
    ("system.permissions.read", Scope::System),
    ("system.permissions.set", Scope::System),
    ("system.processes.control", Scope::System),
    ("system.processes.debug", Scope::System),
    ("system.processes.read", Scope::System),
    ("system.prompts.use", Scope::System),
    ("system.raw_storage.use", Scope::System),
    ("system.services.create", Scope::System),
    ("system.subvolumes.manage", Scope::System),
    ("system.unions.create", Scope::System),
    ("system.user_id.set", Scope::System),
    ("system.users.authenticate", Scope::System),
    ("user.accessibility.use", Scope::User),
    ("user.autostart.use", Scope::User),
    ("user.background.use", Scope::User),
    ("user.background_controllers.use", Scope::User),
    ("user.bluetooth.use", Scope::User),
    ("user.camera.use", Scope::User),
    ("user.desktop.access", Scope::User),
    ("user.developer.access", Scope::User),
    ("user.documents.access", Scope::User),
    ("user.downloads.access", Scope::User),
    ("user.full_disk.access", Scope::User),
    ("user.global_shortcuts.use", Scope::User),
    ("user.input.read", Scope::User),
    ("user.local_network.use", Scope::User),
    ("user.location.read", Scope::User),
    ("user.microphone.use", Scope::User),
    ("user.music.access", Scope::User),
    ("user.network_volumes.access", Scope::User),
    ("user.pictures.access", Scope::User),
    ("user.raw_hid.use", Scope::User),
    ("user.screen.read", Scope::User),
    ("user.videos.access", Scope::User),
    ("user.volumes.access", Scope::User),
];

/// The scope of `permission`, or `None` when the broker does not know it.
pub fn scope(permission: &str) -> Option<Scope> {
    PERMISSIONS
        .binary_search_by(|(name, _)| (*name).cmp(permission))
        .ok()
        .map(|at| PERMISSIONS[at].1)
}
