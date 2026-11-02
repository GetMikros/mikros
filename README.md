# Mikros

The Mikros operating system: boot manager, bootloader, kernel and user space
in one repository.

## Getting the source

    git clone --recurse-submodules https://github.com/GetMikros/mikros.git

The specifications are in the `spec/` submodule, which is a private
repository. Code and commits refer to them by ID, such as `MIKSPEC-0012.R3`.

## Layout

| Path | Contents |
|---|---|
| `spec/` | The Mikros specifications (submodule) |
| `xtask/` | Development tasks, run with `cargo xtask` |
| `targets/` | Target specifications for Rust and Clang |
| `boot/bootmenu/` | Boot manager |
| `boot/mikrosboot/` | Bootloader |
| `boot/protocol/` | Data passed from the bootloader to the kernel |
| `kernel/core/` | Architecture-independent kernel |
| `kernel/arch/x86_64/` | x86_64 support |
| `kernel/fuzz/` | Host fuzz targets |
| `user/init/` | The initial process |

Folders appear as work on them starts.

## Development environment

All development tasks are subcommands of `cargo xtask`. The Rust toolchain is
pinned in `rust-toolchain.toml` and installed by rustup on first use.

The host needs these tools, which `cargo xtask setup` checks:

| Tool | Minimum version |
|---|---|
| QEMU (`qemu-system-x86_64`) | 8.2.0 |
| GDB | 12.1.0 |
| `mkfs.btrfs` (btrfs-progs) | 6.12.0 |

## Contributing

- Only requirements that are approved in `spec/` are implemented.
- Every commit that implements requirements ends with a trailer listing their
  IDs:

      Spec: MIKSPEC-0012.R4, MIKSPEC-0012.R5

- Keep each commit to one logical change of at most 500 changed lines,
  excluding generated files and lock files.
- AI agents follow [AGENTS.md](AGENTS.md).
