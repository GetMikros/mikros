//! Checking that a debug kernel was built with full debug information,
//! overflow checks, debug assertions and frame pointers.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// The DWARF sections of full debug information.
const DEBUG_SECTIONS: [&str; 3] = [".debug_info", ".debug_line", ".debug_str"];

/// The output of `tool` run with `args` and then `kernel`.
fn run(tool: &str, args: &[&str], kernel: &Path) -> Result<String> {
    let output = Command::new(crate::debuginfo::llvm_tool(tool)?)
        .args(args)
        .arg(kernel)
        .output()
        .with_context(|| format!("failed to run {tool}"))?;
    if !output.status.success() {
        bail!("{tool} failed for {}", kernel.display());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What the kernel lacks, given its section headers, its demangled
/// symbols and the disassembly of `_start`, as `llvm-objdump -h`,
/// `llvm-nm -C` and `llvm-objdump -d -M intel --no-show-raw-insn` print
/// them.
fn missing(sections: &str, symbols: &str, start: &str) -> Vec<&'static str> {
    let mut missing = Vec::new();
    let names: Vec<&str> = sections
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    if !DEBUG_SECTIONS.iter().all(|s| names.contains(s)) {
        missing.push("full debug information");
    }
    // Overflow checks panic through these functions of `core`.
    if !symbols.contains("panic_const_add_overflow") {
        missing.push("overflow checks");
    }
    // `core` checks the preconditions of unsafe functions only with
    // debug assertions.
    if !symbols.contains("precondition_check") {
        missing.push("debug assertions");
    }
    let instructions: Vec<String> = start
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(address, _)| {
            !address.trim().is_empty() && address.trim().bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>().join(" "))
        .take(2)
        .collect();
    if instructions != ["push rbp", "mov rbp, rsp"] {
        missing.push("frame pointers");
    }
    missing
}

/// Fails unless the debug or diagnostic kernel `kernel` has full debug
/// information, overflow checks, debug assertions and frame pointers.
pub fn check(kernel: &Path) -> Result<()> {
    let sections = run("llvm-objdump", &["-h"], kernel)?;
    let symbols = run("llvm-nm", &["-C"], kernel)?;
    let start = run(
        "llvm-objdump",
        &[
            "-d",
            "-M",
            "intel",
            "--no-show-raw-insn",
            "--disassemble-symbols=_start",
        ],
        kernel,
    )?;
    let missing = missing(&sections, &symbols, &start);
    if !missing.is_empty() {
        bail!("{} lacks {}", kernel.display(), missing.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECTIONS: &str = "Sections:\n\
        Idx Name          Size     VMA              Type\n  \
        0               00000000 0000000000000000\n  \
        1 .text         00001000 ffffffff80000000 TEXT\n  \
        2 .debug_info   00002000 0000000000000000 DEBUG\n  \
        3 .debug_line   00000800 0000000000000000 DEBUG\n  \
        4 .debug_str    00000400 0000000000000000 DEBUG\n";
    const SYMBOLS: &str = "ffffffff80001000 T core::panicking::panic_const::panic_const_add_overflow\n\
        ffffffff80002000 t core::ptr::read::precondition_check\n";
    const START: &str = "kernel:\tfile format elf64-x86-64\n\n\
        Disassembly of section .text:\n\n\
        ffffffff80003000 <_start>:\n\
        ffffffff80003000:      \tpush\trbp\n\
        ffffffff80003001:      \tmov\trbp, rsp\n\
        ffffffff80003004:      \tsub\trsp, 0x40\n";

    #[test]
    fn accepts_a_debug_kernel() {
        assert!(missing(SECTIONS, SYMBOLS, START).is_empty());
    }

    #[test]
    fn names_what_a_kernel_lacks() {
        let sections = SECTIONS.replace(".debug_line", ".rodata");
        let start = START.replace("push\trbp", "sub\trsp, 8");
        assert_eq!(
            missing(&sections, "", &start),
            [
                "full debug information",
                "overflow checks",
                "debug assertions",
                "frame pointers",
            ]
        );
    }
}
