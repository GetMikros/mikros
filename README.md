# Mikros

The Mikros operating system: boot manager, bootloader, kernel and user space
in one repository. Mikros is a microkernel-based OS.

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
| `boot/bootmenu/` | Boot manager |
| `boot/mikrosboot/` | Bootloader |
| `boot/protocol/` | Data passed from the bootloader to the kernel |
| `kernel/core/` | Architecture-independent kernel |
| `kernel/arch/x86_64/` | x86_64 support |
| `kernel/fuzz/` | Host fuzz targets |
| `kernel/gdb/` | GDB commands for inspecting the kernel |
| `user/bin/` | Programs: the shell `msh`, the text console, setup |
| `user/core/` | `init`, the initial process, and the process and service managers |
| `user/services/` | System services, each with its manifest |
| `user/drivers/` | Device and file system drivers, each with its `driver.yaml` |
| `user/lib/` | Libraries that two or more packages, or xtask, use |
| `user/runtime/` | C library, Rust runtime, and the patches of the standard library port |
| `user/ports/` | Third-party projects: each one's `port.toml` pin and Mikros patches |
| `user/tests/` | The test init and the programs the integration tests run |

Folders appear as work on them starts.

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

All of user space is one Cargo workspace, `user/`, which `cargo xtask`
builds for Mikros. Its packages test the code that does not need Mikros on
the host: `cargo test` and `cargo clippy` in `user/` run them. For
rust-analyzer to show the code that only Mikros builds, set
`rust-analyzer.cargo.target` to `targets/x86_64-unknown-mikros.json` and
`rust-analyzer.cargo.extraEnv` to
`CARGO_UNSTABLE_BUILD_STD=core,alloc,compiler_builtins`,
`CARGO_UNSTABLE_BUILD_STD_FEATURES=compiler-builtins-mem`,
`CARGO_UNSTABLE_JSON_TARGET_SPEC=true` and `CARGO_PROFILE_DEV_PANIC=abort`,
which is how xtask builds the programs without std.

`cargo xtask debug` boots the disk image in QEMU halted and starts GDB
attached to the kernel, with the `mikros` commands of `kernel/gdb/mikros.py`
loaded; `help mikros` lists them.
`cargo xtask symbolize LOG` prints a serial log with the backtrace addresses
of its kernel panic reports replaced by functions, files and lines.

## License

The kernel, under `kernel/`, is licensed under the Mozilla Public License
2.0; see [kernel/LICENSE](kernel/LICENSE).

## Contributing

- Only requirements that are approved in `spec/` are implemented.
- Every commit that implements requirements ends with a trailer listing their
  IDs:

      Spec: MIKSPEC-0012.R4, MIKSPEC-0012.R5

- Keep each commit to one logical change of at most 500 changed lines,
  excluding generated files and lock files.
- AI agents follow [AGENTS.md](AGENTS.md).
