//! The QEMU virtual machines that `cargo xtask` starts.

use crate::firmware::Firmware;
use clap::ValueEnum;
use std::path::Path;

pub const QEMU: &str = "qemu-system-x86_64";

/// The graphics device of the virtual machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Gpu {
    /// QEMU's standard VGA device.
    Basic,
    /// A virtio-gpu-pci device.
    Virtio,
    /// A virtio-gpu-gl-pci device with Venus, blob resources and host
    /// memory.
    Venus,
}

/// The controller the disk image is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Disk {
    Virtio,
    Nvme,
    Ahci,
}

/// The options for a virtual machine, shared by the commands that start
/// one.
#[derive(clap::Args, Clone, Debug)]
pub struct MachineArgs {
    /// Memory size, in QEMU's notation such as 2G.
    #[arg(long, default_value = "2G")]
    pub memory: String,
    /// Number of processors.
    #[arg(long, default_value_t = 2)]
    pub cpus: u32,
    /// The graphics device.
    #[arg(long, value_enum, default_value_t = Gpu::Virtio)]
    pub gpu: Gpu,
    /// The disk controller.
    #[arg(long, value_enum, default_value_t = Disk::Virtio)]
    pub disk: Disk,
    /// Leave out the IOMMU.
    #[arg(long)]
    pub no_iommu: bool,
}

/// Whether KVM can be used: `/dev/kvm` opens for reading and writing.
pub fn kvm_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

/// The processor that a virtual machine has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processor {
    /// The host's, under KVM.
    Host,
    /// QEMU's emulated `max` processor.
    Emulated,
    /// The emulated processor with the vendor `GenuineIntel` and without
    /// `RDCL_NO`, for which the kernel isolates its page tables.
    EmulatedIntel,
}

impl Processor {
    /// The host's processor when KVM can be used, and otherwise the
    /// emulated one.
    pub fn available() -> Processor {
        if kvm_available() {
            Processor::Host
        } else {
            Processor::Emulated
        }
    }
}

/// The QEMU arguments for a virtual machine booting `image` with the
/// firmware code of `firmware` and the variable store `vars`.
pub fn args(
    machine: &MachineArgs,
    processor: Processor,
    firmware: &Firmware,
    vars: &Path,
    image: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| args.extend(items.iter().map(|s| s.to_string()));

    let accel = if processor == Processor::Host {
        "kvm"
    } else {
        "tcg"
    };
    // Interrupt remapping needs the I/O APIC to be emulated by QEMU.
    let irqchip = if machine.no_iommu {
        ""
    } else {
        ",kernel-irqchip=split"
    };
    let venus = machine.gpu == Gpu::Venus;
    let memory_backend = if venus { ",memory-backend=ram" } else { "" };
    push(&[
        "-machine",
        &format!("q35,accel={accel}{irqchip}{memory_backend}"),
    ]);
    push(&[
        "-cpu",
        match processor {
            Processor::Host => "host,migratable=no,+invtsc",
            Processor::Emulated => "max",
            Processor::EmulatedIntel => "max,vendor=GenuineIntel",
        },
    ]);
    push(&["-smp", &machine.cpus.to_string(), "-m", &machine.memory]);
    if venus {
        // Blob resources need guest memory that can be shared with the host.
        let backend = format!(
            "memory-backend-memfd,id=ram,size={},share=on",
            machine.memory
        );
        push(&["-object", &backend]);
    }
    if !machine.no_iommu {
        // The IOMMU must come before the devices it translates for.
        push(&["-device", "intel-iommu,intremap=on"]);
    }

    let code = format!(
        "if=pflash,format=raw,readonly=on,file={}",
        firmware.code.display()
    );
    let vars = format!("if=pflash,format=raw,file={}", vars.display());
    push(&["-drive", &code, "-drive", &vars]);
    let disk = format!("if=none,id=disk,format=raw,file={}", image.display());
    push(&["-drive", &disk]);
    push(&[
        "-device",
        match machine.disk {
            Disk::Virtio => "virtio-blk-pci,drive=disk",
            Disk::Nvme => "nvme,drive=disk,serial=mikros",
            Disk::Ahci => "ide-hd,drive=disk,bus=ide.0",
        },
    ]);

    push(&["-vga", "none"]);
    match machine.gpu {
        Gpu::Basic => push(&["-device", "VGA"]),
        Gpu::Virtio => push(&["-device", "virtio-gpu-pci"]),
        Gpu::Venus => {
            let hostmem = format!(
                "virtio-gpu-gl-pci,venus=on,blob=on,hostmem={}",
                machine.memory
            );
            push(&["-device", &hostmem, "-display", "gtk,gl=on"]);
        }
    }
    push(&["-serial", "stdio"]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn machine() -> MachineArgs {
        MachineArgs {
            memory: "2G".into(),
            cpus: 4,
            gpu: Gpu::Virtio,
            disk: Disk::Virtio,
            no_iommu: false,
        }
    }

    fn firmware() -> Firmware {
        Firmware {
            code: PathBuf::from("code.fd"),
            vars: PathBuf::from("vars.fd"),
        }
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn uses_the_host_processor_with_kvm_and_max_otherwise() {
        let image = Path::new("disk.img");
        let with_kvm = args(
            &machine(),
            Processor::Host,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        assert_eq!(after(&with_kvm, "-cpu"), ["host,migratable=no,+invtsc"]);
        assert_eq!(
            after(&with_kvm, "-machine"),
            ["q35,accel=kvm,kernel-irqchip=split"]
        );
        let without = args(
            &machine(),
            Processor::Emulated,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        assert_eq!(after(&without, "-cpu"), ["max"]);
        let intel = args(
            &machine(),
            Processor::EmulatedIntel,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        assert_eq!(after(&intel, "-cpu"), ["max,vendor=GenuineIntel"]);
        assert_eq!(
            after(&intel, "-machine"),
            ["q35,accel=tcg,kernel-irqchip=split"]
        );
        assert_eq!(after(&without, "-smp"), ["4"]);
        assert_eq!(after(&without, "-serial"), ["stdio"]);
    }

    #[test]
    fn puts_the_iommu_before_other_devices_unless_disabled() {
        let image = Path::new("disk.img");
        let a = args(
            &machine(),
            Processor::Emulated,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        let devices = after(&a, "-device");
        assert_eq!(
            devices,
            [
                "intel-iommu,intremap=on",
                "virtio-blk-pci,drive=disk",
                "virtio-gpu-pci"
            ]
        );
        let mut m = machine();
        m.no_iommu = true;
        let a = args(
            &m,
            Processor::Emulated,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        assert!(
            !after(&a, "-device")
                .iter()
                .any(|d| d.starts_with("intel-iommu"))
        );
        assert_eq!(after(&a, "-machine"), ["q35,accel=tcg"]);
    }

    #[test]
    fn attaches_the_chosen_disk_and_gpu() {
        let image = Path::new("disk.img");
        let mut m = machine();
        m.disk = Disk::Nvme;
        m.gpu = Gpu::Basic;
        let a = args(
            &m,
            Processor::Emulated,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        assert_eq!(
            after(&a, "-device")[1..],
            ["nvme,drive=disk,serial=mikros", "VGA"]
        );
        m.disk = Disk::Ahci;
        m.gpu = Gpu::Venus;
        let a = args(
            &m,
            Processor::Emulated,
            &firmware(),
            Path::new("v.fd"),
            image,
        );
        assert_eq!(
            after(&a, "-device")[1..],
            [
                "ide-hd,drive=disk,bus=ide.0",
                "virtio-gpu-gl-pci,venus=on,blob=on,hostmem=2G"
            ]
        );
        assert_eq!(
            after(&a, "-machine"),
            ["q35,accel=tcg,kernel-irqchip=split,memory-backend=ram"]
        );
    }
}
