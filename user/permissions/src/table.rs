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
    ("accounts.authenticate", Kind::System),
    ("accounts.manage", Kind::System),
    ("autostart", Kind::User),
    ("background.run", Kind::User),
    ("camera", Kind::User),
    ("device.registry", Kind::System),
    ("files.desktop", Kind::User),
    ("files.documents", Kind::User),
    ("files.downloads", Kind::User),
    ("files.full_disk", Kind::User),
    ("files.music", Kind::User),
    ("files.network_volumes", Kind::User),
    ("files.pictures", Kind::User),
    ("files.removable", Kind::User),
    ("files.videos", Kind::User),
    ("filesystem.compose", Kind::System),
    ("filesystem.manage", Kind::System),
    ("filesystem.mount", Kind::System),
    ("gpu.display", Kind::System),
    ("gpu.high_priority", Kind::System),
    ("gpu.render", Kind::Default),
    ("input.game_controllers", Kind::Default),
    ("input.game_controllers_background", Kind::User),
    ("input.hid_raw", Kind::User),
    ("input.monitoring", Kind::User),
    ("input.seat", Kind::System),
    ("location", Kind::User),
    ("logs.add_source", Kind::System),
    ("logs.read", Kind::System),
    ("microphone", Kind::User),
    ("nearby.bluetooth", Kind::User),
    ("nearby.local_network", Kind::User),
    ("network", Kind::Default),
    ("permissions.check", Kind::System),
    ("permissions.grant_system", Kind::System),
    ("permissions.manage", Kind::System),
    ("permissions.prompt", Kind::System),
    ("power.control", Kind::System),
    ("process.debug", Kind::System),
    ("process.list_all", Kind::System),
    ("process.set_identity", Kind::System),
    ("process.set_user", Kind::System),
    ("process.signal_any", Kind::System),
    ("scheduling.realtime", Kind::Default),
    ("screen.capture", Kind::User),
    ("services.register", Kind::System),
    ("session.authorize", Kind::System),
    ("shortcuts.global", Kind::User),
    ("storage.raw", Kind::System),
    ("time.set", Kind::System),
];

/// The kind of `permission`, or `None` when the broker does not know it.
pub fn kind(permission: &str) -> Option<Kind> {
    PERMISSIONS
        .binary_search_by(|(name, _)| (*name).cmp(permission))
        .ok()
        .map(|at| PERMISSIONS[at].1)
}
