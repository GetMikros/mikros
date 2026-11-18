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
    use mikros_cli::{Paint, complain, start};
    use request::{COMMAND, Request};
    let run = start(&COMMAND, &|_, _| Vec::new());
    let invalid = |message: &str| {
        complain(&COMMAND, &run, message);
        mikros_cli::INVALID
    };
    let code = match request::read(&run) {
        Ok(Request::Pack {
            folder,
            out,
            sysroot: Some(sysroot),
        }) => {
            let libraries = std::path::Path::new(&sysroot).join("libraries");
            pack::run(&folder, out.as_deref(), &libraries, Paint::stderr(&run))
        }
        Ok(Request::Pack { .. }) => invalid("pack needs --sysroot on the build machine"),
        Ok(_) => invalid("only pack runs on the build machine"),
        Err(reason) => invalid(&reason),
    };
    std::process::ExitCode::from(code)
}
