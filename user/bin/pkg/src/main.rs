//! pkg, which installs, removes, lists, shows and makes `.mika` packages
//! (MIKSPEC-0101.R28). `request` reads the arguments, `order` puts the
//! packages to install after those they need, `show` makes the output,
//! `pack` makes packages from folders, and `program`, left out of the host's build, calls the install
//! service. On the build machine pkg only packs.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod order;
mod pack;
#[cfg(target_os = "mikros")]
mod program;
mod request;
mod show;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

/// On the build machine, pkg only packs, with the needed libraries'
/// versions taken from the toolchain's sysroot (MIKSPEC-0101.R52).
#[cfg(not(target_os = "mikros"))]
fn main() -> std::process::ExitCode {
    use request::Request;
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let code = match request::read(arguments) {
        Request::Pack {
            folder,
            out,
            sysroot: Some(sysroot),
        } => {
            let libraries = std::path::Path::new(&sysroot).join("libraries");
            pack::run(&folder, out.as_deref(), &libraries)
        }
        Request::Pack { .. } => {
            eprintln!("pkg: pack needs --sysroot on the build machine");
            mikros_cli::INVALID
        }
        _ => {
            eprintln!("pkg: only pack runs on the build machine");
            mikros_cli::INVALID
        }
    };
    std::process::ExitCode::from(code)
}
