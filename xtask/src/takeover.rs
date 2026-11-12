//! The takeover run: the boot framebuffer's PCI function, QEMU's VGA, is
//! run by the basic display driver as its fallback driver until the root
//! file system brings a normal driver for it, which takes it over.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PASSED: &str = "SCENARIO takeover PASSED";
const BOUND: &str = "bound driver basicdisplay to device ";
const AS_BOOT_FRAMEBUFFER: &str = ", as display:boot-framebuffer";
const RELEASED: &str = "device-manager: released the kernel console";

/// Why the serial output `serial` does not show the boot framebuffer's
/// PCI function handed over, if it does not:
/// - the basic display driver is bound to a PCI function as
///   `display:boot-framebuffer`;
/// - that function was added with its BAR sizes, after the kernel
///   console was released, so that nothing drew on the framebuffer
///   while it was sized;
/// - the basic display driver started on it, `test-takeover` took it
///   over, the basic display driver stopped, and `test-takeover` started,
///   in that order.
fn handed_over(serial: &str) -> Result<(), String> {
    let lines: Vec<&str> = serial.lines().collect();
    let find = |text: &str| lines.iter().position(|l| l.contains(text));
    let bound = lines
        .iter()
        .find(|l| l.contains(BOUND) && l.contains(AS_BOOT_FRAMEBUFFER))
        .ok_or("the basic display driver was not bound to a PCI function")?;
    let rest = bound.split_once(BOUND).map_or("", |(_, rest)| rest);
    let (id, rest) = rest.split_once(", ").ok_or("the bound line has no name")?;
    let name = rest
        .split_once(AS_BOOT_FRAMEBUFFER)
        .map_or(rest, |(n, _)| n);
    let added = find(&format!("added device {id}, {name}, as "))
        .ok_or_else(|| format!("device {id}, {name}, was not added"))?;
    if !lines[added].contains(", bar0 ") {
        return Err(format!("{name} was added without its BAR sizes"));
    }
    let released = find(RELEASED).ok_or("the kernel console was not released")?;
    if released > added {
        return Err(format!(
            "the kernel console was released after {name} was added"
        ));
    }
    let steps = [
        format!("started driver basicdisplay for device {id} as pid"),
        format!("driver test-takeover takes over device {id}, {name}, from driver basicdisplay"),
        format!("stopped driver basicdisplay for device {id}, which another driver takes over"),
        format!("started driver test-takeover for device {id} as pid"),
    ];
    let mut after = 0;
    for step in &steps {
        let at = lines[after..]
            .iter()
            .position(|l| l.contains(step.as_str()))
            .ok_or_else(|| format!("{step:?} is missing or out of order"))?;
        after += at + 1;
    }
    Ok(())
}

/// Boots the takeover run of the components `built` of the build kind
/// `kind` in `dir`, with the takeover archive as the root partition.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let system = built
        .takeover_image
        .as_deref()
        .context("the test build has no takeover archive")?;
    image::create_with_system(kind, built, system, &dir.join("disk.img"), "test=takeover")?;
    let log = dir.join("serial.log");
    let mut qemu = scenario::start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = qemu.try_wait()? {
            break Some(status);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = qemu.kill();
    let _ = qemu.wait();
    let Some(status) = status else {
        bail!(
            "the takeover run timed out; serial output in {}",
            log.display()
        );
    };
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    let checked = crate::test::verdict(status.code(), &serial)
        .and_then(|()| match serial.contains(PASSED) {
            true => Ok(()),
            false => Err(format!("{PASSED:?} is missing")),
        })
        .and_then(|()| handed_over(&serial));
    if let Err(why) = checked {
        bail!(
            "the takeover run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
[0.80] I p750: device-manager: released the kernel console
[0.85] I p750: device-manager: added device 5, pci-0000:00:03.0, as pci:v1234d1111sv1af4sd1100r02, under 1, with 0 resources, bar0 0x1000000, bar2 0x1000
[0.85] I p750: device-manager: bound driver basicdisplay to device 5, pci-0000:00:03.0, as display:boot-framebuffer
[0.86] I p750: device-manager: started driver basicdisplay for device 5 as pid 20
[2.10] I p750: device-manager: driver test-takeover takes over device 5, pci-0000:00:03.0, from driver basicdisplay
[2.11] I p750: device-manager: stopped driver basicdisplay for device 5, which another driver takes over
[2.12] I p750: device-manager: started driver test-takeover for device 5 as pid 31";

    #[test]
    fn needs_the_function_sized_quietly_and_handed_over_in_order() {
        assert_eq!(handed_over(LOG), Ok(()));
        let unsized_function = LOG.replace(", bar0 0x1000000, bar2 0x1000", "");
        assert!(handed_over(&unsized_function).is_err());
        let late_release: String = {
            let mut lines: Vec<&str> = LOG.lines().collect();
            let released = lines.remove(0);
            lines.insert(2, released);
            lines.join("\n")
        };
        assert!(handed_over(&late_release).is_err());
        let swapped = LOG.replace("started driver test-takeover", "started driver test-x");
        assert!(handed_over(&swapped).is_err());
        let platform = LOG.replace(AS_BOOT_FRAMEBUFFER, ", as platform:boot-framebuffer");
        assert!(handed_over(&platform).is_err());
    }
}
