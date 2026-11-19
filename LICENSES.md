# License boundaries and source distribution

The root [LICENSE](LICENSE) supplies the MPL-2.0 text for Mikros code
already designated MPL-2.0, including the kernel. Include this file when
redistributing the kernel source separately. The root file does not change
file or component licenses. Preserve
third-party copyright notices, license identifiers, and exceptions.

## Linux-based btrfs driver

The [userspace btrfs package](user/drivers/btrfs/Cargo.toml) declares
`GPL-2.0-only` and statically links the Linux btrfs library into its driver
executable. The [full GPL version 2 text](user/drivers/btrfs/COPYING) is
provided here. Linux files retain their individual notices and exceptions.
This boundary is the combined driver executable, not the entire boot image
or repository; the separate [bootloader btrfs reader](boot/mikrosboot/src/btrfs)
is not part of that executable.

The [Linux port manifest](user/ports/linux/port.toml) pins the unmodified
upstream `linux-6.18.62.tar.gz` from kernel.org by SHA-256. The local port
is supplied as a [Mikros architecture and configuration overlay](user/drivers/btrfs/linux)
and [C shim](user/drivers/btrfs/shim). The exact upstream inputs, generated
wrappers and build steps are defined by [xtask](xtask/src/btrfs.rs); the
[package build script](user/drivers/btrfs/build.rs) links the result.
The driver also uses Mikros ACL, runtime and case-folding crates. The
[case-folding table](user/lib/casefold/src/table.rs) derives from Unicode
Character Database 16.0.0 and carries a Unicode-3.0 notice. Retain original
licenses on reusable source files and verify that the terms for each linked
input permit distribution as part of the GPL-2.0-only executable.

## What to ship with a binary release

For each image or download containing the driver executable, publish a
matching complete corresponding-source archive alongside the binary under
GPLv2 section 3(a). A kernel.org URL, a moving branch, or the Linux tarball
alone is not that archive. Record the binary's SHA-256 and exact Mikros
commit, and include:

- The full GPLv2 text and applicable copyright, license and exception
  notices, including notices for linked dependencies and data.
- The full checksum-verified Linux 6.18.62 archive used by the build.
- The exact Mikros driver source, shim, architecture overlay, configuration,
  and any additional patches or local changes used to produce the binary.
- The preferred source for all linked modules, including local crates and
  exact third-party dependency sources, and interface definitions. Include
  source inputs and generators for generated files where needed to modify
  and rebuild the executable; lockfiles alone do not supply these sources.
- The exact compilation and installation scripts, manifests, lockfiles,
  target configuration, toolchain pins, and release build instructions.
  Include the xtask code and build support needed by those instructions.

Test the archive on the supported x86_64 Linux build host: rebuild the
btrfs library with `cargo xtask btrfs`, then rebuild and install the driver
using the same xtask image command and options used for the release. Verify
that the archive does not require private sources or missing build inputs,
and that recipients can download it from the same place as the binary.
This document is a release checklist, not a written source offer or evidence
that an existing release includes complete corresponding source.
