//! Checking the control registers, flags and code segment the kernel is
//! entered with: GDB stops QEMU at the kernel's entry point and reads
//! them.

use crate::debug;
use crate::qemu::MachineArgs;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// A register bit and whether it must be set at the kernel's entry point.
struct Bit {
    register: &'static str,
    name: &'static str,
    bit: u32,
    set: bool,
}

const fn bit(register: &'static str, name: &'static str, bit: u32, set: bool) -> Bit {
    Bit {
        register,
        name,
        bit,
        set,
    }
}

/// The registers that are read, as GDB names them.
const REGISTERS: [&str; 4] = ["cr0", "cr4", "efer", "eflags"];

const BITS: [Bit; 10] = [
    bit("cr0", "PE", 0, true),
    bit("cr0", "WP", 16, true),
    bit("cr0", "PG", 31, true),
    bit("cr4", "PAE", 5, true),
    bit("cr4", "LA57", 12, false),
    bit("efer", "LME", 8, true),
    bit("efer", "LMA", 10, true),
    bit("efer", "NXE", 11, true),
    bit("eflags", "IF", 9, false),
    bit("eflags", "DF", 10, false),
];

/// The entry point in the header of the ELF64 file `elf`.
fn entry_point(elf: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(elf.get(24..32)?.try_into().ok()?))
}

/// The value of each of `REGISTERS`, from GDB's `output`, in which each
/// `p/x $<register>` follows a line `@@ <register>`.
fn values(output: &str) -> Vec<Option<u64>> {
    REGISTERS
        .iter()
        .map(|register| {
            let marker = format!("@@ {register}\n");
            let (_, rest) = output.split_once(&marker)?;
            let line = rest.lines().next()?;
            let (_, value) = line.split_once(" = 0x")?;
            u64::from_str_radix(value.trim(), 16).ok()
        })
        .collect()
}

/// Whether QEMU's `info registers` in GDB's `output`, after a line
/// `@@ cs`, shows a 64-bit code segment.
fn code_segment_64bit(output: &str) -> bool {
    output.split_once("@@ cs\n").is_some_and(|(_, rest)| {
        rest.lines()
            .any(|line| line.starts_with("CS =") && line.contains(" CS64 "))
    })
}

/// The bits of `BITS` that the register values, in the order of
/// `REGISTERS`, get wrong.
fn wrong(values: &[u64]) -> Vec<String> {
    BITS.iter()
        .filter(|b| {
            let i = REGISTERS.iter().position(|r| *r == b.register).unwrap();
            (values[i] >> b.bit & 1 == 1) != b.set
        })
        .map(|b| {
            let state = if b.set { "clear" } else { "set" };
            format!("{}.{} is {state}", b.register.to_uppercase(), b.name)
        })
        .collect()
}

/// Boots the test image `disk` halted, lets it run until the kernel
/// `kernel` is entered, and fails unless the registers have the values
/// of `BITS` there and the code segment is a 64-bit one, or when GDB has
/// not finished after `timeout`. The
/// serial output goes to `log`, and GDB's output to `gdb-entry.log` next
/// to it.
pub fn check(
    machine: &MachineArgs,
    kernel: &Path,
    disk: &Path,
    log: &Path,
    timeout: Duration,
) -> Result<()> {
    let elf = fs::read(kernel).with_context(|| format!("reading {}", kernel.display()))?;
    let Some(entry) = entry_point(&elf) else {
        bail!("{} has no ELF header", kernel.display());
    };
    let port = debug::free_port()?;
    let mut qemu = debug::start_halted(machine, disk, log, port, &["-display", "none"])?;
    let mut args: Vec<String> = ["-nx", "-batch", "-ex"].map(String::from).into();
    args.push(format!("target remote 127.0.0.1:{port}"));
    args.extend(["-ex".into(), format!("hbreak *{entry:#x}")]);
    args.extend(["-ex".into(), "continue".into()]);
    for register in REGISTERS {
        args.extend(["-ex".into(), format!("echo @@ {register}\\n")]);
        args.extend(["-ex".into(), format!("p/x ${register}")]);
    }
    args.extend(["-ex".into(), "echo @@ cs\\n".into()]);
    args.extend(["-ex".into(), "monitor info registers".into()]);
    args.extend(["-ex".into(), "kill".into()]);
    let output_path = log.with_file_name("gdb-entry.log");
    let output = fs::File::create(&output_path)
        .with_context(|| format!("creating {}", output_path.display()))?;
    let errors = output.try_clone()?;
    let finished = Command::new(debug::gdb())
        .args(args)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(errors)
        .spawn()
        .with_context(|| format!("failed to start {}", debug::gdb()))
        .and_then(|mut gdb| debug::wait_for(&mut gdb, timeout));
    let _ = qemu.kill();
    let _ = qemu.wait();
    finished.with_context(|| {
        format!(
            "GDB did not stop at the kernel's entry point; serial output in {}",
            log.display()
        )
    })?;
    let output = fs::read_to_string(&output_path)
        .with_context(|| format!("reading {}", output_path.display()))?;
    let Some(values) = values(&output).into_iter().collect::<Option<Vec<u64>>>() else {
        bail!("GDB did not print every register at the kernel's entry point\n{output}");
    };
    let mut wrong = wrong(&values);
    if !code_segment_64bit(&output) {
        wrong.push("the code segment is not a 64-bit one".into());
    }
    if !wrong.is_empty() {
        bail!("at the kernel's entry point, {}", wrong.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values QEMU shows at the kernel's entry point.
    const GOOD: [u64; 4] = [0x8001_0033, 0x668, 0xd00, 0x46];

    #[test]
    fn reads_the_entry_point() {
        let mut elf = vec![0u8; 64];
        elf[24..32].copy_from_slice(&0xffff_ffff_8000_1230u64.to_le_bytes());
        assert_eq!(entry_point(&elf), Some(0xffff_ffff_8000_1230));
        assert_eq!(entry_point(&elf[..30]), None);
    }

    #[test]
    fn reads_the_printed_values() {
        let output = "Breakpoint 1, 0xffffffff80001230 in ?? ()\n\
            @@ cr0\n$1 = 0x80010033\n@@ cr4\n$2 = 0x668\n\
            @@ efer\n$3 = 0xd00\n@@ eflags\n$4 = 0x46\n";
        assert_eq!(values(output), GOOD.map(Some));
        let missing = output.replace("$3 = 0xd00", "No registers.");
        assert_eq!(values(&missing)[2], None);
    }

    #[test]
    fn accepts_the_entry_state() {
        assert!(wrong(&GOOD).is_empty());
    }

    #[test]
    fn finds_the_64bit_code_segment() {
        let output = "@@ cs\n\nCPU#0\n\
            SS =0030 0000000000000000 ffffffff 00cf9300 DPL=0 DS   [-WA]\n\
            CS =0038 0000000000000000 ffffffff 00af9a00 DPL=0 CS64 [-R-]\n";
        assert!(code_segment_64bit(output));
        assert!(!code_segment_64bit(&output.replace("CS64", "CS32")));
        assert!(!code_segment_64bit(&output.replace("@@ cs", "@@ ds")));
    }

    #[test]
    fn names_each_wrong_bit() {
        let [cr0, cr4, efer, eflags] = GOOD;
        assert_eq!(
            wrong(&[cr0 & !(1 << 16), cr4 | 1 << 12, efer & !(1 << 11), eflags]),
            ["CR0.WP is clear", "CR4.LA57 is set", "EFER.NXE is clear"]
        );
        assert_eq!(
            wrong(&[cr0 & !1, cr4 & !(1 << 5), efer & !0x500, eflags | 0x600]),
            [
                "CR0.PE is clear",
                "CR4.PAE is clear",
                "EFER.LME is clear",
                "EFER.LMA is clear",
                "EFLAGS.IF is set",
                "EFLAGS.DF is set"
            ]
        );
        assert_eq!(
            wrong(&[cr0 & !(1 << 31), cr4, efer, eflags]),
            ["CR0.PG is clear"]
        );
    }
}
