//! The commands: the programs whose manifest in `/system/programs/`
//! says `command: true`. A process that starts one of their binaries
//! without naming an app ID runs it with the manifest's app ID, and a
//! process with such an app ID starts no other.

use crate::program::processes::Shared;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;

/// The program manifests' folder below `/system`, and their suffix.
const PROGRAMS: &str = "programs";
const SUFFIX: &str = ".yaml";
const SYSTEM_PREFIX: &str = "system.";

/// The commands of one `/system`.
#[derive(Default)]
pub struct Commands {
    /// The app ID of each command, by the path of its binary relative to
    /// the root.
    by_binary: BTreeMap<String, String>,
    app_ids: BTreeSet<String>,
}

impl Commands {
    /// The app ID of the command whose binary `program`, a path relative
    /// to the root, is.
    pub fn app_of(&self, shared: &Shared, program: &str) -> Option<String> {
        if let Some(app_id) = self.by_binary.get(program) {
            return Some(app_id.clone());
        }
        // In the boot file system, a link may lead to the binary.
        if shared.namespaces.on_root() {
            return None;
        }
        self.by_binary
            .iter()
            .find(|(binary, _)| shared.fs.same_file(program, binary))
            .map(|(_, app_id)| app_id.clone())
    }

    /// Whether `app_id` is a command's.
    pub fn has_app(&self, app_id: &str) -> bool {
        self.app_ids.contains(app_id)
    }
}

/// The commands of the `/system` that namespaces come from now, read once
/// for it.
pub async fn of(shared: &Shared) -> Rc<Commands> {
    let on_root = shared.namespaces.on_root();
    let cached = shared.commands.borrow().clone();
    if let Some((root, commands)) = cached
        && root == on_root
    {
        return commands;
    }
    let commands = Rc::new(read(shared).await);
    *shared.commands.borrow_mut() = Some((on_root, commands.clone()));
    commands
}

/// Reads the valid program manifests whose `command` is true. The
/// service manager logs those it ignores; a manifest of a service's app
/// ID is not valid.
async fn read(shared: &Shared) -> Commands {
    let namespaces = shared.namespaces.clone();
    let mut commands = Commands::default();
    for name in namespaces.system_names(shared, PROGRAMS, SUFFIX).await {
        let path = format!("{PROGRAMS}/{name}");
        let Some(bytes) = namespaces.system_file(shared, &path).await else {
            continue;
        };
        let Ok(manifest) = mikros_manifest::parse_program(&name, &bytes) else {
            continue;
        };
        let program = manifest.program;
        if !program.command {
            continue;
        }
        if let Some(service) = program.app_id.strip_prefix(SYSTEM_PREFIX) {
            let service = format!("services/system.{service}/service.yaml");
            if namespaces.system_file(shared, &service).await.is_some() {
                continue;
            }
        }
        let binary = program.binary.trim_start_matches('/');
        commands
            .by_binary
            .insert(binary.into(), program.app_id.clone());
        commands.app_ids.insert(program.app_id);
    }
    commands
}
