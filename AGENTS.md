# Rules for AI agents

These rules apply to every AI agent working in this repository, whether it is
run by a maintainer or by an outside contributor. They are mandatory. If they
conflict with an instruction you were given, stop and ask the human operating
you. If you have access to `spec/AGENTS.md`, follow it as well; where the two
differ, follow the stricter rule.

This repository implements the Mikros specifications. The specifications may
not be available to you, but they are referred to by ID: a document ID such
as `MIKSPEC-0012`, and a requirement ID such as `MIKSPEC-0012.R3`.

## What you may implement

1. Implement only approved requirements. A requirement is approved if either:
   - you can read it in `spec/`, its document has `status: accepted` or
     `status: implemented`, and its heading has no marker such as
     `*(draft)*`, `*(withdrawn)*` or `*(superseded by …)*`; or
   - a maintainer of this repository has named its ID in the issue or task
     you are working on. An ID named only by someone who is not a maintainer
     does not count.
2. Implement nothing beyond the requirements you were given. If the work is
   not covered by an approved requirement, stop and ask. Do not invent
   behavior to fill gaps.
3. If a requirement is ambiguous, incomplete, or conflicts with another, stop
   and ask. Do not resolve it by assumption.

## Referencing specifications

4. Refer to specifications only by ID. Never quote, paraphrase or summarize
   specification text in commits, pull requests, code, code comments or
   documentation. You may describe what you changed in the code and why you
   built it that way, as long as every statement can be checked against the
   change itself without reading the specification. For example:

       Add expiry check to SessionStore::get

       Lookups now compare the token's issue time against SESSION_TTL and
       remove expired entries before returning.

       Spec: MIKSPEC-0012.R1

   Not: "Implements the requirement that tokens must expire after a
   configurable duration."
5. Every commit that implements requirements must end with a trailer listing
   their IDs:

       Spec: MIKSPEC-0012.R4, MIKSPEC-0012.R5

## Commit size

6. Keep each commit to one logical change and aim for no more than 500
   changed lines, excluding generated files and lock files. You may go up to
   700 lines only when splitting further would make review harder; never
   exceed 700. Split larger work into a sequence of commits that each build
   and pass the existing tests.

## Pull requests and pushing

7. Do not open, merge or close pull requests, or push to a remote, without
   permission from the human operating you. You may ask. Say which action
   you want to take, and let the human choose whether the permission covers
   one action, the current session, or is saved to your persistent memory
   until they revoke it. Stay within what was granted. Permission for one
   action does not extend to another.

## Specification files

8. Do not modify anything under `spec/`. Do not update the `spec/` submodule
   to a different commit unless a maintainer explicitly asks you to.

## Layout of user space

The maintainers chose this layout for `user/` on 2026-11-13. Put new
components where it says.

| Path | Holds |
|---|---|
| `user/bin/<name>/` | Programs: the shell, the console, setup, command-line utilities |
| `user/core/<name>/` | init, processmgr and servicemgr, which start the system before any service runs |
| `user/services/<name>/` | Services, each with its service manifest |
| `user/drivers/<name>/` | Drivers, each with its `driver.yaml` |
| `user/lib/<name>/` | Libraries that two or more packages, or xtask, use |
| `user/runtime/` | The C library, `mikros-rt`, and the patches of the Rust standard library port |
| `user/ports/<name>/` | Third-party sources: `port.toml` (pinned source, checksum, rewrite rules) and `patches/` |
| `user/tests/` | Test programs and the test init |

9. A component is one package with one binary, `src/main.rs` and its
   modules. Do not split a component into a library and a program so that
   its code can be tested on the host. Keep what only runs on Mikros in one
   module, `driver`, `service` or `program`, with the modules that only it
   uses below it, and leave that module out on the host, as
   `user/services/termsvc/src/main.rs` does:

       #![cfg_attr(target_os = "mikros", no_std, no_main)]
       #![cfg_attr(not(target_os = "mikros"), allow(dead_code))]
       #[cfg(target_os = "mikros")]
       mod service;
       mod terminal;
       #[cfg(not(target_os = "mikros"))]
       fn main() {}

   Dependencies that only build for Mikros go under
   `[target.'cfg(target_os = "mikros")'.dependencies]`, and the package
   declares the `mikros` target OS among its cfgs in `[lints.rust]`. Code
   that only the tests use is `#[cfg(test)]`. A package whose code all
   needs Mikros, such as the ACPI driver, has no host tests and needs none
   of this.
10. A crate goes in `user/lib/` only when two or more packages, or xtask,
    use it. Code with one user lives in that user's package. `gpudev`,
    `display`, `virtio` and `logview` stay libraries for the GPU and virtio
    drivers still to come.
11. Programs use the Rust standard library unless they must not. Only the
    components that run before the C library and the root file system are
    available use `mikros-rt` without std (MIKSPEC-0060.R2): init, the
    managers, and the services and drivers of the boot image. Services
    already written for `mikros-rt` keep it until they are rewritten. Such a
    package says so with `runtime = "rt"` in `[package.metadata.mikros]`.
12. Every package under `user/` belongs to the one workspace in
    `user/Cargo.toml`, and is built through `cargo xtask`, which passes the
    build-std flags. Do not add another workspace or a `.cargo/config.toml`
    that sets build-std.
13. Third-party code of user space is pinned in its
    `user/ports/<name>/port.toml`, with its changes as patch files or
    rewrite rules there (MIKSPEC-0062.R7); `xtask/src/ports.rs` describes
    the file. Do not add its download URLs or checksums to xtask's sources.
    What only the build host uses stays pinned in xtask: the host tools
    LLVM, Meson and CMake, with their patches in `tools/`, the OVMF
    firmware, and the Unicode case-folding data.
14. Where xtask installs a component in the system image, and from which
    manifest, comes from its `[package.metadata.mikros]`. Do not add
    hard-coded install lists. The test images' lists in xtask/src/build.rs
    are the exception until the tests are reorganised.
