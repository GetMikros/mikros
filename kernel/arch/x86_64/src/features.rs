//! Checking the processor features the kernel requires.

use core::fmt;

/// The CPUID registers the required features are read from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leaves {
    pub leaf1_ecx: u32,
    pub leaf1_edx: u32,
    /// Leaf 7, subleaf 0; zero when the processor has no leaf 7.
    pub leaf7_ebx: u32,
    /// Leaf 0x8000_0001; zero when the processor has no such leaf.
    pub ext1_ecx: u32,
    pub ext1_edx: u32,
}

impl Leaves {
    /// Reads the leaves with `cpuid`, which gives EAX, EBX, ECX and EDX for
    /// a leaf and subleaf.
    pub fn read(cpuid: impl Fn(u32, u32) -> [u32; 4]) -> Leaves {
        let basic_max = cpuid(0, 0)[0];
        let ext_max = cpuid(0x8000_0000, 0)[0];
        let [_, _, leaf1_ecx, leaf1_edx] = cpuid(1, 0);
        let leaf7_ebx = if basic_max >= 7 { cpuid(7, 0)[1] } else { 0 };
        let [_, _, ext1_ecx, ext1_edx] = if ext_max >= 0x8000_0001 {
            cpuid(0x8000_0001, 0)
        } else {
            [0; 4]
        };
        Leaves {
            leaf1_ecx,
            leaf1_edx,
            leaf7_ebx,
            ext1_ecx,
            ext1_edx,
        }
    }
}

#[derive(Clone, Copy)]
enum Register {
    Leaf1Ecx,
    Leaf1Edx,
    Leaf7Ebx,
    Ext1Ecx,
    Ext1Edx,
}

use Register::*;

/// Each required feature with the register and bit that report it.
const REQUIRED: [(&str, Register, u32); 25] = [
    ("long mode", Ext1Edx, 29),
    ("NX", Ext1Edx, 20),
    ("SYSCALL/SYSRET", Ext1Edx, 11),
    ("APIC", Leaf1Edx, 9),
    ("PAT", Leaf1Edx, 16),
    ("global pages", Leaf1Edx, 13),
    ("TSC", Leaf1Edx, 4),
    ("FXSAVE", Leaf1Edx, 24),
    ("XSAVE", Leaf1Ecx, 26),
    ("CMPXCHG16B", Leaf1Ecx, 13),
    ("LAHF/SAHF", Ext1Ecx, 0),
    ("POPCNT", Leaf1Ecx, 23),
    ("SSE2", Leaf1Edx, 26),
    ("SSE3", Leaf1Ecx, 0),
    ("SSSE3", Leaf1Ecx, 9),
    ("SSE4.1", Leaf1Ecx, 19),
    ("SSE4.2", Leaf1Ecx, 20),
    ("AVX", Leaf1Ecx, 28),
    ("AVX2", Leaf7Ebx, 5),
    ("BMI1", Leaf7Ebx, 3),
    ("BMI2", Leaf7Ebx, 8),
    ("F16C", Leaf1Ecx, 29),
    ("FMA", Leaf1Ecx, 12),
    ("LZCNT", Ext1Ecx, 5),
    ("MOVBE", Leaf1Ecx, 22),
];

impl Leaves {
    fn has(&self, register: Register, bit: u32) -> bool {
        let value = match register {
            Leaf1Ecx => self.leaf1_ecx,
            Leaf1Edx => self.leaf1_edx,
            Leaf7Ebx => self.leaf7_ebx,
            Ext1Ecx => self.ext1_ecx,
            Ext1Edx => self.ext1_edx,
        };
        value & (1 << bit) != 0
    }

    /// The names of the required features the processor lacks.
    pub fn missing(&self) -> impl Iterator<Item = &'static str> + '_ {
        REQUIRED
            .iter()
            .filter(|&&(_, register, bit)| !self.has(register, bit))
            .map(|&(name, _, _)| name)
    }
}

/// The missing features as a comma-separated list.
pub struct Missing<'a>(pub &'a Leaves);

impl fmt::Display for Missing<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, name) in self.0.missing().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            f.write_str(name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Leaves {
        let mut leaves = Leaves::default();
        for &(_, register, bit) in &REQUIRED {
            let r = match register {
                Leaf1Ecx => &mut leaves.leaf1_ecx,
                Leaf1Edx => &mut leaves.leaf1_edx,
                Leaf7Ebx => &mut leaves.leaf7_ebx,
                Ext1Ecx => &mut leaves.ext1_ecx,
                Ext1Edx => &mut leaves.ext1_edx,
            };
            *r |= 1 << bit;
        }
        leaves
    }

    #[test]
    fn accepts_a_processor_with_every_feature() {
        assert_eq!(all().missing().count(), 0);
        assert_eq!(std::format!("{}", Missing(&all())), "");
    }

    #[test]
    fn names_each_missing_feature() {
        let mut leaves = all();
        leaves.leaf7_ebx &= !(1 << 5);
        leaves.ext1_edx &= !(1 << 20);
        assert_eq!(std::format!("{}", Missing(&leaves)), "NX, AVX2");
    }

    #[test]
    fn reads_absent_leaves_as_zero() {
        let leaves = Leaves::read(|leaf, _| match leaf {
            0 => [1, 0, 0, 0],
            0x8000_0000 => [0x8000_0000, 0, 0, 0],
            1 => [0, 0, 0xffff_ffff, 0xffff_ffff],
            _ => [0xffff_ffff; 4],
        });
        assert_eq!((leaves.leaf7_ebx, leaves.ext1_edx), (0, 0));
        let missing: std::vec::Vec<_> = leaves.missing().collect();
        assert!(missing.contains(&"long mode") && missing.contains(&"AVX2"));
    }
}
