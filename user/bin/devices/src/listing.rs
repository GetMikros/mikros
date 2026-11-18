//! What devices takes, and what it writes: one line per device, its
//! `<class>/<name>` and its description two spaces apart, or a JSON array
//! with an object per device (MIKSPEC-0096.R5).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};

/// The classes whose services drivers may publish, as the device manager
/// knows them, which completion offers.
const CLASSES: &[&str] = &[
    "acpi",
    "filesystem",
    "gpu",
    "hid",
    "input",
    "rtc",
    "storage",
    "subvolumes",
];

pub static COMMAND: Spec = Spec {
    operands: &[Operand::optional(
        "class",
        Kind::Choice(CLASSES),
        "the class of the devices to list",
    )],
    options: &[Opt::long(
        "json",
        "write the devices as a JSON array of objects",
    )],
    ..Spec::new(
        "devices",
        "Lists the named devices, of every class or of class, each as\n\
         <class>/<name> followed by its description.",
    )
};

/// The longest class that `List` takes, in bytes.
const CLASS_MAX: usize = 32;

/// The class to list, none for every class, or why the arguments are
/// invalid.
pub fn class(run: &Run) -> Result<Option<String>, String> {
    match run.operands.first() {
        Some(class) if class.len() > CLASS_MAX => {
            Err(format!("a class has at most {CLASS_MAX} bytes"))
        }
        class => Ok(class.cloned()),
    }
}

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

/// The lines of `devices`, with the descriptions coloured as free text.
pub fn text(devices: &[Device], paint: Paint) -> String {
    devices
        .iter()
        .map(|d| {
            let description = paint.paint(Role::String, &d.description);
            format!("{}  {description}\n", d.key())
        })
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
    use mikros_cli::Parsed;

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

    fn run(arguments: &[&str]) -> Result<Run, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Ok(run),
            Parsed::Invalid(reason) => Err(reason),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_the_class() {
        assert_eq!(class(&run(&[]).unwrap()), Ok(None));
        let r = run(&["storage", "--json"]).unwrap();
        assert!(r.has("--json"));
        assert_eq!(class(&r), Ok(Some("storage".into())));
        let long = "c".repeat(33);
        assert_eq!(
            class(&run(&[&long]).unwrap()),
            Err("a class has at most 32 bytes".into())
        );
        assert_eq!(run(&["a", "b"]).err(), Some("unexpected argument b".into()));
    }

    #[test]
    fn writes_a_line_per_device() {
        assert_eq!(
            text(&devices(), Paint::never()),
            "input/input0  PS/2 keyboard\nstorage/storage0  QEMU \"HARDDISK\"\n"
        );
        assert_eq!(text(&[], Paint::never()), "");
        let on = Paint::new(Some("always"), false, None);
        assert_eq!(
            text(&devices()[..1], on),
            "input/input0  \x1b[38;2;255;134;26mPS/2 keyboard\x1b[0m\n"
        );
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

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/devices.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
