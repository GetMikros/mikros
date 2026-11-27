# Mikros

The Mikros operating system: boot manager, bootloader, kernel and user space
in one repository. Mikros is a microkernel-based OS. Services and drivers
are separate programs under `user/` that talk through interfaces defined in
MIDL under `midl/`, and the system is built and tested from this repository
with `cargo xtask`.

What runs today: the kernel, the process, service and session managers, drivers
for storage, USB, PS/2 and graphics, btrfs, exFAT and FAT file systems, a text
console with login, a shell and dozens of command-line programs, and,
in the desktop edition, a Wayland compositor with a prototype greeter and
shell. It is under heavy development; nothing here is stable.

[![CI](https://github.com/GetMikros/mikros/actions/workflows/ci.yml/badge.svg)](https://github.com/GetMikros/mikros/actions/workflows/ci.yml)

## Getting the source

    git clone --recurse-submodules https://github.com/GetMikros/mikros.git

The specifications are in the `spec/` submodule, which is a private
repository. Code and commits refer to them by ID, such as `MIKSPEC-0012.R3`.

## Layout

| Path | Contents |
|---|---|
| `spec/` | The Mikros specifications (submodule) |
| `xtask/` | Development tasks, run with `cargo xtask` |
| `targets/` | Target specification for Rust |
| `midl/` | Interface definitions in MIDL, one folder per library |
| `tools/` | Host tools and patches: `midlc` (the MIDL compiler), `midl-tests`, `fuzz`, and the patches of the LLVM, CMake and Meson that build for Mikros |
| `boot/bootmenu/` | Boot manager |
| `boot/mikrosboot/` | Bootloader |
| `boot/protocol/` | Data passed from the bootloader to the kernel |
| `kernel/core/` | Architecture-independent kernel |
| `kernel/arch/x86_64/` | x86_64 support |
| `kernel/syscalls/`, `kernel/syscalls.yaml` | The system call interface |
| `kernel/fuzz/` | Host fuzz targets |
| `kernel/gdb/` | GDB commands for inspecting the kernel |
| `user/bin/` | Programs: the shell `msh`, the text console, the compositor, setup and the command-line utilities |
| `user/core/` | `init`, the initial process, and the process and service managers |
| `user/apps/` | System apps, each a package with `kind = "app"`, installed as `/system/apps/system.mikros.<name>/` |
| `user/services/` | System services, each with its manifest |
| `user/drivers/` | Device and file system drivers, each with its `driver.yaml` |
| `user/lib/` | Libraries that two or more packages, or xtask, use |
| `user/runtime/` | C library, Rust runtime, and the patches of the standard library port |
| `user/ports/` | Third-party projects: each one's `port.toml` pin and Mikros patches |
| `user/data/` | Data files that `cargo xtask build` installs in the image, now the font configuration |
| `user/tests/` | The test init and the programs the integration tests run |

## Development environment

> [!IMPORTANT]
> Mikros can only be built on a Linux computer on x86_64 as of right now.

All development tasks are subcommands of `cargo xtask`. The Rust toolchain is
pinned in `rust-toolchain.toml` and installed by rustup on first use.

The host needs these tools, which `cargo xtask setup` checks:

| Tool | Minimum version |
|---|---|
| QEMU (`qemu-system-x86_64`) | 8.2.0 |
| GDB | 12.1.0 |
| `mkfs.btrfs` (btrfs-progs) | 6.12.0 |
| `mkfs.exfat` (exfatprogs) | 1.2.0 |
| `cargo-fuzz`, for the fuzz targets | 0.13.0 |
| Python `mako`, `packaging` and `yaml` (PyYAML), for Mesa | mako 0.8.0 |
| `flex` and `bison`, for Mesa | any |
| `pkg-config` and expat, for Wayland's `wayland-scanner` | any |

All of user space is one Cargo workspace, `user/`, which `cargo xtask`
builds for Mikros. Most of its packages test the code that does not need Mikros
on the host: `cargo test` and `cargo clippy` in `user/` run them. For
rust-analyzer to show the code that only Mikros builds, set
`rust-analyzer.cargo.target` to `targets/x86_64-unknown-mikros.json` and
`rust-analyzer.cargo.extraEnv` to
`CARGO_UNSTABLE_BUILD_STD=core,alloc,compiler_builtins`,
`CARGO_UNSTABLE_BUILD_STD_FEATURES=compiler-builtins-mem`,
`CARGO_UNSTABLE_JSON_TARGET_SPEC=true` and `CARGO_PROFILE_DEV_PANIC=abort`,
which is how xtask builds the programs without std.

## Building, running and testing

Run `cargo xtask --help` for every subcommand and `cargo xtask <command>
--help` for its options. The ones you use most:

| Command | What it does |
|---|---|
| `cargo xtask setup` | Checks the host tools and fetches the pinned firmware; `--install` offers to install missing tools |
| `cargo xtask build` | Builds the system components |
| `cargo xtask image` | Creates a bootable disk image |
| `cargo xtask run` | Boots that image in QEMU |
| `cargo xtask test` | Runs the host unit tests and the integration tests in QEMU |
| `cargo xtask check` | Checks formatting, lints and the commits since the base branch |
| `cargo xtask sysroot` | Builds the sysroot that C and C++ programs are built against |

Build kinds (`--kind`): `debug` (the default), `production`, `diag` (the
production optimization with checks and diagnostic records), `staging`
(production's build with the stand-ins of its edition, for development) and
`fuzz`.

Editions (`--edition`, for `image` and `run`):

- `desktop` (the default) has the compositor. Every kind except `production`
  also has a stand-in greeter and shell and the administrator `dee`, whom the
  greeter logs in on text console 1; the password of `dee` is made for each
  image, printed, and written to a file next to the image. Type `desktop`
  there to start the desktop, and switch with Ctrl-Alt-F1 (desktop) and
  Ctrl-Alt-F2 (console). The `production` kind has no stand-ins and keeps the
  desktop off. A desktop run gets 3 GiB of memory by default.
- `server` has no compositor; users log in on the text console.

To try a program or library that is built outside this repository, give
`cargo xtask image --demos DIRECTORY` a folder laid out as `/system`: `bin/`,
`libraries/*.dlib` and `share/`.

A full `cargo xtask test` boots many test images and takes tens of minutes. `--host`
runs only the unit tests without QEMU, `--jobs N` sets how many QEMU lanes run
at once (4 by default), and `--kind` tests another build kind. CI runs
`cargo xtask check --ci`, which checks formatting, lints and the commit
trailers; builds the `production`, `debug`, `fuzz` and `diag` kinds with
`cargo xtask build --kind`; and runs `cargo xtask test --host`.

Some libraries are built from source into static libraries first, with their
own subcommands: `cargo xtask dawn` (WebGPU), `cargo xtask acpica` and
`cargo xtask btrfs`. The first sysroot build also builds LLVM and a Mikros
toolchain, which takes a long time and a lot of disk space.

`cargo xtask debug` boots the disk image in QEMU halted and starts GDB
attached to the kernel, with the `mikros` commands of `kernel/gdb/mikros.py`
loaded; `help mikros` lists them.
`cargo xtask symbolize LOG` prints a serial log with the backtrace addresses
of its kernel panic reports replaced by functions, files and lines.

## License

The root [LICENSE](LICENSE) contains the Mozilla Public License 2.0 text
for Mikros code designated MPL-2.0, including the kernel.
Component and file notices identify exceptions. The Linux-based userspace
btrfs driver is GPL-2.0-only; see [LICENSES.md](LICENSES.md) for the license
boundaries, third-party provenance, and corresponding-source requirements.

## Contributing

- Only requirements that are approved in `spec/` are implemented.
- Every commit that implements requirements ends with a trailer listing their
  IDs:

      Spec: MIKSPEC-0012.R4, MIKSPEC-0012.R5

- The head commit of a push also carries `Tested: cargo xtask test` in the
  same trailer block; `cargo xtask check --ci` checks for it.
- Keep each commit to one logical change, aiming for at most 500 changed
  lines and never more than 700, excluding generated files and lock files.
- AI agents follow [AGENTS.md](AGENTS.md).
