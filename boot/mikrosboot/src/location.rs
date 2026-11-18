//! The location of the boot disk that `boot_disk=` gives the system
//! (MIKSPEC-0004.R40): the PCI function its device path leads to, then a
//! part for each node of the path after the last PCI node.

use crate::display::PciLocation;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Device path types and subtypes: hardware PCI nodes, and the messaging
/// nodes of USB devices, SATA ports and NVMe namespaces.
const HARDWARE: u8 = 1;
const PCI: u8 = 1;
const MESSAGING: u8 = 3;
const USB: u8 = 5;
const SATA: u8 = 0x12;
const NVME: u8 = 0x17;

/// Why a disk has no location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unlocated {
    /// The type and subtype of the first node that no part stands for.
    pub kind: u8,
    pub sub: u8,
}

/// The location of a disk whose device path has `nodes`, as type, subtype
/// and the data after each node's header, and leads to the PCI function
/// `pci`: `pci-<segment>:<bus>:<device>.<function>` in hexadecimal, then
/// `/nvme-<namespace>`, `/sata-<port>` or `/usb-<port>` for each node
/// after the last PCI node, the USB port one more than the node's parent
/// port, all in decimal. Fails on a node of another kind there, or one
/// whose data is too short.
pub fn location(pci: PciLocation, nodes: &[(u8, u8, Vec<u8>)]) -> Result<String, Unlocated> {
    let after = nodes
        .iter()
        .rposition(|&(kind, sub, _)| (kind, sub) == (HARDWARE, PCI))
        .map_or(0, |last| last + 1);
    let mut text = format!(
        "pci-{:04x}:{:02x}:{:02x}.{:x}",
        pci.segment,
        pci.bus,
        pci.devfn >> 3,
        pci.devfn & 7
    );
    for &(kind, sub, ref data) in &nodes[after..] {
        let unlocated = Unlocated { kind, sub };
        let u16_at = |data: &[u8]| Some(u16::from_le_bytes(data.get(..2)?.try_into().ok()?));
        let u32_at = |data: &[u8]| Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?));
        let part = match (kind, sub) {
            (MESSAGING, NVME) => u32_at(data).map(|namespace| format!("nvme-{namespace}")),
            (MESSAGING, SATA) => u16_at(data).map(|port| format!("sata-{port}")),
            (MESSAGING, USB) => data
                .first()
                .map(|&parent| format!("usb-{}", u32::from(parent) + 1)),
            _ => None,
        };
        text.push('/');
        text.push_str(&part.ok_or(unlocated)?);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pci(segment: u16, bus: u8, device: u8, function: u8) -> PciLocation {
        PciLocation {
            segment,
            bus,
            devfn: device * 8 + function,
        }
    }

    /// The ACPI root and PCI nodes of a path to the function at `device`
    /// of bus 0, behind a bridge at 1c.0 when `bridged`.
    fn to_pci(device: u8, bridged: bool) -> Vec<(u8, u8, Vec<u8>)> {
        let mut nodes = vec![(2, 1, vec![0x41, 0xD0, 0x0A, 0x08, 0, 0, 0, 0])];
        if bridged {
            nodes.push((HARDWARE, PCI, vec![0, 0x1C]));
        }
        nodes.push((HARDWARE, PCI, vec![0, device]));
        nodes
    }

    #[test]
    fn names_the_function_and_the_nodes_after_it() {
        let mut nvme = to_pci(4, true);
        nvme.push((MESSAGING, NVME, vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(
            location(pci(0, 3, 0, 0), &nvme),
            Ok("pci-0000:03:00.0/nvme-1".into())
        );
        let mut sata = to_pci(0x1F, false);
        sata.push((MESSAGING, SATA, vec![2, 0, 0xFF, 0xFF, 0, 0]));
        assert_eq!(
            location(pci(0, 0, 0x1F, 2), &sata),
            Ok("pci-0000:00:1f.2/sata-2".into())
        );
        // A stick on port 3 of a hub on root port 1 (parent ports 0 and 2).
        let mut usb = to_pci(0x14, false);
        usb.push((MESSAGING, USB, vec![0, 0]));
        usb.push((MESSAGING, USB, vec![2, 0]));
        assert_eq!(
            location(pci(1, 0, 0x14, 0), &usb),
            Ok("pci-0001:00:14.0/usb-1/usb-3".into())
        );
        // A virtio-blk disk is the PCI function itself.
        assert_eq!(
            location(pci(0, 0, 3, 0), &to_pci(3, false)),
            Ok("pci-0000:00:03.0".into())
        );
    }

    #[test]
    fn leaves_paths_with_other_nodes_without_a_location() {
        // A logical unit after a USB node, as multi-LUN sticks have.
        let mut unit = to_pci(0x14, false);
        unit.push((MESSAGING, USB, vec![0, 0]));
        unit.push((MESSAGING, 0x11, vec![1]));
        assert_eq!(
            location(pci(0, 0, 0x14, 0), &unit),
            Err(Unlocated {
                kind: MESSAGING,
                sub: 0x11
            })
        );
        let mut short = to_pci(4, false);
        short.push((MESSAGING, NVME, vec![1, 0]));
        assert!(location(pci(0, 0, 4, 0), &short).is_err());
    }
}
