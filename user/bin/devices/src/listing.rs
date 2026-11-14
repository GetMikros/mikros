//! What devices writes: one line per device, its `<class>/<name>` and its
//! description two spaces apart, or a JSON array with an object per
//! device.

use mikros_cli::json_string;

/// A named device.
#[derive(Debug, PartialEq)]
pub struct Device {
    pub class: String,
    pub name: String,
    pub description: String,
}

impl Device {
    /// Its `<class>/<name>`, by which the device manager orders the
    /// devices and pages through them.
    pub fn key(&self) -> String {
        format!("{}/{}", self.class, self.name)
    }
}

/// The lines of `devices`.
pub fn text(devices: &[Device]) -> String {
    devices
        .iter()
        .map(|d| format!("{}  {}\n", d.key(), d.description))
        .collect()
}

/// `devices` as a JSON array of objects with the members `class`, `name`
/// and `description`, on one line.
pub fn json(devices: &[Device]) -> String {
    let objects: Vec<String> = devices
        .iter()
        .map(|d| {
            format!(
                "{{\"class\":{},\"name\":{},\"description\":{}}}",
                json_string(&d.class),
                json_string(&d.name),
                json_string(&d.description)
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices() -> Vec<Device> {
        let device = |class: &str, name: &str, description: &str| Device {
            class: class.into(),
            name: name.into(),
            description: description.into(),
        };
        vec![
            device("input", "input0", "PS/2 keyboard"),
            device("storage", "storage0", "QEMU \"HARDDISK\""),
        ]
    }

    #[test]
    fn writes_a_line_per_device() {
        assert_eq!(
            text(&devices()),
            "input/input0  PS/2 keyboard\nstorage/storage0  QEMU \"HARDDISK\"\n"
        );
        assert_eq!(text(&[]), "");
    }

    #[test]
    fn writes_an_array_of_objects() {
        assert_eq!(
            json(&devices()),
            "[{\"class\":\"input\",\"name\":\"input0\",\"description\":\"PS/2 keyboard\"},\
             {\"class\":\"storage\",\"name\":\"storage0\",\"description\":\"QEMU \\\"HARDDISK\\\"\"}]\n"
        );
        assert_eq!(json(&[]), "[]\n");
    }
}
