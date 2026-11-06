//! Checking the state the kernel leaves the machine in: once the test
//! init waits, the QEMU monitor shows the interrupt controllers, the
//! task state segment and the page tables, which are checked against the
//! kernel's layout.

use crate::build::{self, Kind};
use crate::image;
use crate::qemu::MachineArgs;
use crate::scenario::{WAITING, monitor_output, monitor_socket, start, wait_for_line};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const PAGE: u64 = 4096;
const LARGE: u64 = 2 << 20;
const HUGE: u64 = 1 << 30;
const DIRECT_MAP: u64 = 0xFFFF_8000_0000_0000;
/// The ranges a mapped address may lie in, from the user range up.
const LAYOUT: [(u64, u64); 4] = [
    (0x0000_0000_0001_0000, 0x0000_7FFF_FFFF_F000),
    (DIRECT_MAP, 0xFFFF_C000_0000_0000),
    (0xFFFF_C000_0000_0000, 0xFFFF_FF80_0000_0000),
    (0xFFFF_FF80_0000_0000, u64::MAX),
];
/// The size of each IST stack the kernel must give.
const IST_STACK_SIZE: u64 = 16 << 10;

/// A translation that `info tlb` lists.
#[derive(Clone, Debug, PartialEq)]
struct Translation {
    phys: u64,
    flags: String,
    /// The bytes it maps.
    size: u64,
}

impl Translation {
    fn writable(&self) -> bool {
        self.flags.ends_with('W')
    }

    fn no_execute(&self) -> bool {
        self.flags.starts_with('X')
    }
}

fn hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
}

/// The translations of `info tlb`, by virtual address. `info tlb` marks
/// 2 MiB and 1 GiB pages alike, so a large page is taken to map 1 GiB
/// only when it starts on a 1 GiB boundary and nothing else is listed
/// within that 1 GiB.
fn translations(tlb: &str) -> BTreeMap<u64, Translation> {
    let mut map: BTreeMap<u64, Translation> = tlb
        .lines()
        .filter_map(|line| {
            let (virt, rest) = line.split_once(':')?;
            let mut words = rest.split_whitespace();
            let phys = hex(words.next()?)?;
            let flags = words.next()?.to_string();
            let size = PAGE;
            Some((hex(virt)?, Translation { phys, flags, size }))
        })
        .collect();
    let starts: Vec<u64> = map.keys().copied().collect();
    for (i, start) in starts.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(u64::MAX);
        let t = map.get_mut(start).expect("listed");
        if t.flags.chars().nth(2) == Some('P') {
            let huge = start % HUGE == 0 && next - start >= HUGE;
            t.size = if huge { HUGE } else { LARGE };
        }
    }
    map
}

/// The translation that maps `addr`, and the address it starts at.
fn covering(map: &BTreeMap<u64, Translation>, addr: u64) -> Option<(u64, &Translation)> {
    let (&start, t) = map.range(..=addr).next_back()?;
    (addr - start < t.size).then_some((start, t))
}

/// The kernel image's sections, from its symbols.
struct Image {
    text: (u64, u64),
    rodata: (u64, u64),
    data: (u64, u64),
}

/// The bounds of the sections in `symbols`, as `llvm-nm` prints them.
fn image(symbols: &str) -> Option<Image> {
    let address = |name: &str| {
        symbols.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            let value = words.next()?;
            (words.nth(1)? == name).then(|| hex(value)).flatten()
        })
    };
    Some(Image {
        text: (address("__kernel_start")?, address("__text_end")?),
        rodata: (address("__rodata_start")?, address("__rodata_end")?),
        data: (address("__data_start")?, address("__data_end")?),
    })
}

/// What is wrong with the page tables `tlb` for the kernel `image`.
fn page_table_problems(tlb: &str, image: &Image) -> Vec<String> {
    let map = translations(tlb);
    let mut problems = Vec::new();
    for (&virt, t) in &map {
        if !LAYOUT
            .iter()
            .any(|&(start, end)| (start..end).contains(&virt))
        {
            problems.push(format!("{virt:#x} lies outside the layout"));
        }
        // The bootloader's mappings there were supervisor ones.
        if virt < DIRECT_MAP && t.flags.chars().nth(7) != Some('U') {
            problems.push(format!("{virt:#x} is a kernel mapping in the lower half"));
        }
        if virt >= DIRECT_MAP && t.writable() && !t.no_execute() {
            problems.push(format!("{virt:#x} is writable and executable"));
        }
    }
    let sections = [
        ("code", image.text, false, false),
        ("read-only data", image.rodata, true, false),
        ("writable data", image.data, true, true),
    ];
    for (name, (start, end), no_execute, writable) in sections {
        for page in (start & !(PAGE - 1)..end).step_by(PAGE as usize) {
            let Some((at, t)) = covering(&map, page) else {
                problems.push(format!("the kernel's {name} at {page:#x} is not mapped"));
                break;
            };
            if t.no_execute() != no_execute || t.writable() != writable {
                problems.push(format!("the kernel's {name} at {page:#x} is {}", t.flags));
                break;
            }
            let phys = t.phys + (page - at);
            if covering(&map, DIRECT_MAP + phys).is_some_and(|(_, d)| d.writable()) {
                problems.push(format!(
                    "the kernel's {name} at {page:#x} is writable in the direct map"
                ));
                break;
            }
        }
    }
    problems
}

/// The tops of the IST stacks 1 to 3 in the task state segment `bytes`.
/// Each IST entry points 16 bytes below its stack's top, where the
/// kernel keeps a word for its entry code.
fn ist_tops(bytes: &[u8]) -> Option<[u64; 3]> {
    let top = |n: usize| -> Option<u64> {
        let at = 0x24 + 8 * (n - 1);
        let entry = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
        Some(entry.wrapping_add(16))
    };
    Some([top(1)?, top(2)?, top(3)?])
}

/// What is wrong with the IST stacks whose tops are `tops`, given the
/// page tables `tlb`.
fn ist_problems(tops: [u64; 3], tlb: &str) -> Vec<String> {
    let map = translations(tlb);
    let mut problems = Vec::new();
    for (i, &top) in tops.iter().enumerate() {
        let n = i + 1;
        let bottom = top.wrapping_sub(IST_STACK_SIZE);
        let mapped = (bottom..top)
            .step_by(PAGE as usize)
            .all(|p| covering(&map, p).is_some_and(|(_, t)| t.writable() && t.no_execute()));
        if !mapped {
            problems.push(format!("IST stack {n} is not 16 KiB of writable data"));
        }
        if covering(&map, bottom - PAGE).is_some() {
            problems.push(format!("IST stack {n} has no unmapped page below it"));
        }
        for (j, &other) in tops.iter().enumerate().skip(i + 1) {
            if other > bottom - PAGE && other - IST_STACK_SIZE < top {
                problems.push(format!("IST stacks {n} and {} overlap", j + 1));
            }
        }
    }
    problems
}

/// The bytes that `x /Nbx` prints.
fn dumped_bytes(dump: &str) -> Vec<u8> {
    dump.lines()
        .filter_map(|line| line.split_once(':'))
        .flat_map(|(_, rest)| rest.split_whitespace().map(|b| hex(b).unwrap_or(0) as u8))
        .collect()
}

/// The base of the TSS in `info registers`.
fn tss_base(registers: &str) -> Option<u64> {
    let line = registers.lines().find(|l| l.starts_with("TR "))?;
    hex(line.split_whitespace().nth(2)?)
}

/// What is wrong with the PICs, the I/O APIC and the local APIC, given
/// `info pic` and `info lapic`.
fn interrupt_controller_problems(pic: &str, lapic: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, base) in [("pic0:", "irq_base=20"), ("pic1:", "irq_base=28")] {
        let ok = pic
            .lines()
            .find(|l| l.starts_with(name))
            .is_some_and(|l| l.contains("imr=ff") && l.contains(base));
        if !ok {
            problems.push(format!("{name} is not masked at {base}"));
        }
    }
    let pins: Vec<&str> = pic
        .lines()
        .filter(|l| l.trim_start().starts_with("pin "))
        .collect();
    if pins.is_empty() || pins.iter().any(|l| !l.contains("masked")) {
        problems.push("an I/O APIC entry is not masked".into());
    }
    let entry = |name: &str| {
        lapic
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
    };
    for name in ["LVT0", "LVT1", "LVTPC", "LVTERR", "LVTTHMR"] {
        if !entry(name).is_some_and(|l| l.contains("masked") || l.contains("NMI")) {
            problems.push(format!("{name} is neither masked nor an NMI source"));
        }
    }
    if !entry("LVTT").is_some_and(|l| l.contains("(vec 240)")) {
        problems.push("the timer does not use vector 240".into());
    }
    if !entry("SPIV").is_some_and(|l| l.contains("APIC enabled") && l.contains("spurious vec 255"))
    {
        problems.push("the local APIC is not enabled with spurious vector 255".into());
    }
    problems
}

/// Boots the image in `dir`, waits for the test init, and returns what
/// is wrong with the machine's state for the kernel `image`.
fn inspect(
    machine: &MachineArgs,
    dir: &Path,
    image: &Image,
    timeout: Duration,
) -> Result<Vec<String>> {
    let socket = monitor_socket(dir);
    let mut qemu = start(machine, dir, false)?;
    let result = (|| {
        wait_for_line(
            &dir.join("serial.log"),
            WAITING,
            &mut qemu,
            Instant::now() + timeout,
        )?;
        let tlb = monitor_output(&socket, "info tlb")?;
        fs::write(dir.join("tlb.txt"), &tlb)?;
        let registers = monitor_output(&socket, "info registers")?;
        let Some(base) = tss_base(&registers) else {
            bail!("info registers shows no TR");
        };
        let tss = dumped_bytes(&monitor_output(&socket, &format!("x /104bx {base:#x}"))?);
        let Some(tops) = ist_tops(&tss) else {
            bail!("the TSS dump is short");
        };
        let pic = monitor_output(&socket, "info pic")?;
        let lapic = monitor_output(&socket, "info lapic")?;
        let mut problems = page_table_problems(&tlb, image);
        problems.extend(ist_problems(tops, &tlb));
        problems.extend(interrupt_controller_problems(&pic, &lapic));
        Ok(problems)
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    result
}

/// Boots the test image of the components `built` with `test=wait`, and
/// fails unless the machine's state is as the kernel must leave it.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let symbols = Command::new(crate::debuginfo::llvm_tool("llvm-nm")?)
        .arg(&built.kernel)
        .output()
        .context("failed to run llvm-nm")?;
    let Some(image) = image(&String::from_utf8_lossy(&symbols.stdout)) else {
        bail!("{} lacks its section symbols", built.kernel.display());
    };
    // As for the debugger, the kernel runs at its linked addresses.
    let cmdline = ["test=wait", image::KASLR_OFF].join(" ");
    image::create_with(kind, built, &dir.join("disk.img"), &cmdline, &[])?;
    let problems = inspect(machine, dir, &image, timeout)
        .with_context(|| format!("inspecting the machine failed; output in {}", dir.display()))?;
    if !problems.is_empty() {
        bail!("the machine's state is wrong: {}", problems.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: Image = Image {
        text: (0xffffffff80000000, 0xffffffff80001000),
        rodata: (0xffffffff80001000, 0xffffffff80002000),
        data: (0xffffffff80002000, 0xffffffff80003000),
    };

    const TLB: &str = "\
0000000000202000: 00000000001ce000 X---A--U-
ffff800000000000: 0000000000000000 XG-DA---W
ffff800000200000: 0000000000200000 XGP-----W
ffffc00000011000: 0000000000011000 XG-DA---W
ffffc00000012000: 0000000000012000 XG-DA---W
ffffc00000013000: 0000000000013000 XG-DA---W
ffffc00000014000: 0000000000014000 XG-DA---W
ffffffff80000000: 0000000000400000 -G--A----
ffffffff80001000: 0000000000401000 XG--A----
ffffffff80002000: 0000000000402000 XG-DA---W
";

    #[test]
    fn accepts_a_kernel_laid_out_as_it_must_be() {
        assert!(page_table_problems(TLB, &IMAGE).is_empty());
        assert!(ist_problems([0xffffc00000015000; 3], TLB).len() == 3);
        assert!(
            ist_problems(
                [0xffffc00000015000, 0xffffc00000035000, 0xffffc00000055000],
                TLB
            )
            .iter()
            .all(|p| !p.starts_with("IST stack 1"))
        );
    }

    #[test]
    fn finds_bad_permissions_and_addresses() {
        let tlb = TLB.replace(
            "ffffffff80000000: 0000000000400000 -G--A----",
            "ffffffff80000000: 0000000000400000 -G--A---W",
        );
        assert_eq!(
            page_table_problems(&tlb, &IMAGE),
            [
                "0xffffffff80000000 is writable and executable",
                "the kernel's code at 0xffffffff80000000 is -G--A---W"
            ]
        );
        let tlb = format!("{TLB}ffff900000000000: 0000000000400000 XG-DA---W\n");
        assert!(
            page_table_problems(&tlb, &IMAGE).is_empty(),
            "inside the direct map"
        );
        let tlb = format!("{TLB}0000800000000000: 0000000000400000 XG-DA--UW\n");
        assert_eq!(
            page_table_problems(&tlb, &IMAGE),
            ["0x800000000000 lies outside the layout"]
        );
        let tlb = format!("{TLB}0000000000100000: 0000000000100000 XG-DA---W\n");
        assert_eq!(
            page_table_problems(&tlb, &IMAGE),
            ["0x100000 is a kernel mapping in the lower half"]
        );
        // The large page maps the image's frames writable.
        let tlb = TLB.replace("0000000000400000 -G", "0000000000200000 -G");
        assert_eq!(
            page_table_problems(&tlb, &IMAGE),
            ["the kernel's code at 0xffffffff80000000 is writable in the direct map"]
        );
    }

    #[test]
    fn sizes_large_pages_by_what_follows() {
        let tlb = "\
ffff800040000000: 0000000040000000 XGP-----W
ffff800040200000: 0000000040200000 XGP-----W
ffff800080000000: 0000000080000000 XGP-----W
";
        let map = translations(tlb);
        assert!(covering(&map, 0xffff8000403fffff).is_some());
        assert!(covering(&map, 0xffff800040400000).is_none());
        assert!(covering(&map, 0xffff8000bfffffff).is_some());
    }

    #[test]
    fn checks_the_ist_stacks() {
        let tlb = format!("{TLB}ffffc00000010000: 0000000000010000 XG-DA---W\n");
        assert_eq!(
            ist_problems([0xffffc00000015000; 3], &tlb)[0],
            "IST stack 1 has no unmapped page below it"
        );
        let mut tss = vec![0u8; 0x68];
        tss[0x24..0x2C].copy_from_slice(&7u64.to_le_bytes());
        tss[0x34..0x3C].copy_from_slice(&9u64.to_le_bytes());
        assert_eq!(ist_tops(&tss), Some([23, 16, 25]));
        let dump = "ffffffff801842b8: 0x01 0x02\nffffffff801842ba: 0x03";
        assert_eq!(dumped_bytes(dump), [1, 2, 3]);
        let registers =
            "GS =0000 ffffffff801840a0 00000000 00000000\nTR =0030 ffffffff801842b8 00002068";
        assert_eq!(tss_base(registers), Some(0xffffffff801842b8));
    }

    #[test]
    fn checks_the_interrupt_controllers() {
        let pic = "ioapic0: ver=0x20\n  pin 0  0x0000000000010000 dest=0 vec=0 masked fixed\n\
                   pic1: irr=00 imr=ff isr=00 irq_base=28\npic0: irr=01 imr=ff isr=00 irq_base=20";
        let lapic = "LVT0\t 0x00010000 masked\nLVT1\t 0x00000400 NMI\nLVTPC\t masked\n\
                     LVTERR\t masked\nLVTTHMR\t masked\nLVTT\t 0x000000f0 one-shot Fixed  (vec 240)\n\
                     SPIV\t 0x000001ff APIC enabled, focus=off, spurious vec 255";
        assert!(interrupt_controller_problems(pic, lapic).is_empty());
        let unmasked = pic.replace("masked fixed", "fixed");
        assert_eq!(
            interrupt_controller_problems(&unmasked, lapic),
            ["an I/O APIC entry is not masked"]
        );
        let open = pic.replace("imr=ff isr=00 irq_base=20", "imr=fe isr=00 irq_base=20");
        assert_eq!(
            interrupt_controller_problems(&open, lapic),
            ["pic0: is not masked at irq_base=20"]
        );
    }
}
