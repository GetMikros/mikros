//! Splitting the kernel's debug information into a file named by its build
//! ID.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const PT_NOTE: u32 = 4;
const NT_GNU_BUILD_ID: u32 = 3;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(data: &[u8], at: usize) -> Option<usize> {
    let value = u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?);
    usize::try_from(value).ok()
}

/// The GNU build ID in the note segments of the ELF64 image `elf`.
pub fn build_id(elf: &[u8]) -> Option<Vec<u8>> {
    if elf.get(..5)? != b"\x7fELF\x02" {
        return None;
    }
    let ph_offset = u64_at(elf, 32)?;
    let ph_size = usize::from(u16_at(elf, 54)?);
    let ph_count = usize::from(u16_at(elf, 56)?);
    for i in 0..ph_count {
        let ph = ph_offset + i * ph_size;
        if u32_at(elf, ph)? != PT_NOTE {
            continue;
        }
        let (start, size) = (u64_at(elf, ph + 8)?, u64_at(elf, ph + 32)?);
        let mut at = start;
        while at + 12 <= start + size {
            let name_size = u32_at(elf, at)? as usize;
            let desc_size = u32_at(elf, at + 4)? as usize;
            let kind = u32_at(elf, at + 8)?;
            let name = elf.get(at + 12..at + 12 + name_size)?;
            let desc_at = at + 12 + name_size.next_multiple_of(4);
            if kind == NT_GNU_BUILD_ID && name == b"GNU\0" {
                return Some(elf.get(desc_at..desc_at + desc_size)?.to_vec());
            }
            at = desc_at + desc_size.next_multiple_of(4);
        }
    }
    None
}

/// The path of an LLVM tool that comes with the Rust toolchain.
fn llvm_tool(name: &str) -> Result<PathBuf> {
    let rustc = |arg: &str| -> Result<String> {
        let out = Command::new("rustc")
            .current_dir(crate::workspace_root())
            .arg(arg)
            .output()
            .context("failed to run rustc")?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let sysroot = rustc("--print=sysroot")?;
    let version = rustc("-vV")?;
    let host = version
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .context("rustc did not report its host")?;
    let mut path = Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(name);
    path.set_extension(std::env::consts::EXE_EXTENSION);
    if !path.exists() {
        bail!(
            "{} is missing; the llvm-tools component should provide it",
            path.display()
        );
    }
    Ok(path)
}

/// Writes the debug information of `kernel` to
/// `.build-id/<first byte>/<other bytes>.debug` next to it, with the build
/// ID in lowercase hexadecimal, and returns that path.
pub fn split(kernel: &Path) -> Result<PathBuf> {
    let elf = std::fs::read(kernel).with_context(|| format!("reading {}", kernel.display()))?;
    let id = build_id(&elf).context("the kernel has no build ID")?;
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    if hex.len() < 4 {
        bail!("the kernel's build ID {hex} is too short");
    }
    let dir = kernel.with_file_name(".build-id").join(&hex[..2]);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let out = dir.join(format!("{}.debug", &hex[2..]));
    let status = Command::new(llvm_tool("llvm-objcopy")?)
        .arg("--only-keep-debug")
        .arg(kernel)
        .arg(&out)
        .status()
        .context("failed to run llvm-objcopy")?;
    if !status.success() {
        bail!("llvm-objcopy failed for {}", kernel.display());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf_with_notes(notes: &[u8]) -> Vec<u8> {
        let mut elf = vec![0u8; 64 + 56];
        elf[..5].copy_from_slice(b"\x7fELF\x02");
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[64..68].copy_from_slice(&PT_NOTE.to_le_bytes());
        elf[72..80].copy_from_slice(&120u64.to_le_bytes());
        elf[96..104].copy_from_slice(&(notes.len() as u64).to_le_bytes());
        elf.extend_from_slice(notes);
        elf
    }

    fn note(name: &[u8], kind: u32, desc: &[u8]) -> Vec<u8> {
        let mut n = Vec::new();
        n.extend_from_slice(&(name.len() as u32).to_le_bytes());
        n.extend_from_slice(&(desc.len() as u32).to_le_bytes());
        n.extend_from_slice(&kind.to_le_bytes());
        n.extend_from_slice(name);
        n.resize(n.len().next_multiple_of(4), 0);
        n.extend_from_slice(desc);
        n.resize(n.len().next_multiple_of(4), 0);
        n
    }

    #[test]
    fn finds_the_build_id_after_other_notes() {
        let mut notes = note(b"Mikros\0", 1, &[1, 0, 0, 0]);
        notes.extend(note(
            b"GNU\0",
            NT_GNU_BUILD_ID,
            &[0xab, 0xcd, 0xef, 0x01, 0x02],
        ));
        assert_eq!(
            build_id(&elf_with_notes(&notes)),
            Some(vec![0xab, 0xcd, 0xef, 0x01, 0x02])
        );
    }

    #[test]
    fn reports_a_missing_build_id() {
        let notes = note(b"GNU\0", 1, &[0; 16]);
        assert_eq!(build_id(&elf_with_notes(&notes)), None);
        assert_eq!(build_id(b"not an elf"), None);
    }
}
