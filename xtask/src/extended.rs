//! Checking that kernel code uses no x87, MMX, SSE, AVX or AVX-512
//! instructions or registers, other than to save and restore thread state.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// The instructions that save and restore a thread's extended state.
const SAVE_RESTORE: [&str; 4] = ["xsave", "xsave64", "xrstor", "xrstor64"];

/// Instructions that use the extended state without naming one of its
/// registers.
const IMPLICIT: [&str; 7] = [
    "emms",
    "ldmxcsr",
    "stmxcsr",
    "vldmxcsr",
    "vstmxcsr",
    "vzeroupper",
    "vzeroall",
];

/// Whether `word` names an MMX, SSE, AVX or AVX-512 vector register.
fn is_vector_register(word: &str) -> bool {
    ["xmm", "ymm", "zmm", "mm"].iter().any(|prefix| {
        word.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Whether `instruction`, the mnemonic and operands of a line of an
/// Intel-syntax disassembly, uses the extended state other than to save
/// or restore it.
fn uses_extended_state(instruction: &str) -> bool {
    let mnemonic = instruction.split_whitespace().next().unwrap_or("");
    if SAVE_RESTORE.contains(&mnemonic) {
        return false;
    }
    // Only x87 instructions start with `f`, and only AVX-512 mask
    // instructions with `k`.
    if mnemonic.starts_with(['f', 'k']) || IMPLICIT.contains(&mnemonic) {
        return true;
    }
    // Symbol names, in angle brackets, may contain anything.
    let mut operands = String::new();
    let mut depth = 0;
    for c in instruction.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => operands.push(c),
            _ => {}
        }
    }
    operands
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(is_vector_register)
}

/// The first instruction line of `disassembly`, the output of
/// `llvm-objdump -d -M intel`, that uses the extended state other than to
/// save or restore it.
fn first_use(disassembly: &str) -> Option<&str> {
    disassembly.lines().find(|line| {
        line.split_once(':').is_some_and(|(address, rest)| {
            !address.is_empty()
                && address.bytes().all(|b| b.is_ascii_hexdigit())
                && uses_extended_state(rest.trim_start())
        })
    })
}

/// Fails when the code of `kernel` uses the extended state other than to
/// save and restore it.
pub fn check(kernel: &Path) -> Result<()> {
    let output = Command::new(crate::debuginfo::llvm_tool("llvm-objdump")?)
        .args(["-d", "--no-show-raw-insn", "-M", "intel"])
        .arg(kernel)
        .output()
        .context("failed to run llvm-objdump")?;
    if !output.status.success() {
        bail!("llvm-objdump failed for {}", kernel.display());
    }
    let disassembly = String::from_utf8_lossy(&output.stdout);
    if let Some(line) = first_use(&disassembly) {
        bail!(
            "{} uses the extended state outside saving and restoring it: {}",
            kernel.display(),
            line.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_saving_and_restoring_and_general_instructions() {
        for instruction in [
            "xsave64\t[rcx]",
            "xrstor64\t[rcx]",
            "push\trbp",
            "mov\trax, qword ptr [rip + 0x10]",
            "call\t0xffffffff80001000 <_RNvNtCs1_6kernel5xmm12fsave_mm1k0>",
            "lea\trdi, [rsp + 0x40]",
        ] {
            assert!(!uses_extended_state(instruction), "{instruction}");
        }
    }

    #[test]
    fn finds_vector_registers_and_x87_and_mask_instructions() {
        for instruction in [
            "movaps\txmmword ptr [rsp], xmm0",
            "vmovdqu\tymm15, ymmword ptr [rax]",
            "vpxord\tzmm1, zmm1, zmm1",
            "movq\tmm0, rax",
            "fld\tst(1)",
            "fninit",
            "fxsave\t[rax]",
            "kmovw\tk1, eax",
            "stmxcsr\tdword ptr [rsp]",
            "vzeroupper",
        ] {
            assert!(uses_extended_state(instruction), "{instruction}");
        }
    }

    #[test]
    fn looks_only_at_instruction_lines() {
        let disassembly = "kernel:\tfile format elf64-x86-64\n\n\
            ffffffff80000000 <__kernel_start>:\n\
            ffffffff80000000:      \tpush\trbp\n\
            ffffffff80000001:      \txsave64\t[rcx]\n";
        assert_eq!(first_use(disassembly), None);
        let disassembly = format!("{disassembly}ffffffff80000005:      \tpxor\txmm0, xmm0\n");
        assert_eq!(
            first_use(&disassembly),
            Some("ffffffff80000005:      \tpxor\txmm0, xmm0")
        );
    }
}
