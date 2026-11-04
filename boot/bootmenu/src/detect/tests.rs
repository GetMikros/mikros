use super::*;
use alloc::vec;

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn node(kind: u8, sub: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![kind, sub];
    out.extend_from_slice(&(data.len() as u16 + 4).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// A hard drive node with arbitrary contents followed by `files`.
fn path(files: &[&str]) -> Vec<u8> {
    let mut out = node(MEDIA, 1, &[7; 38]);
    for f in files {
        out.extend(node(MEDIA, MEDIA_FILE_PATH, &utf16(f)));
    }
    out
}

fn load_option(description: &str, list: &[u8], optional: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_le_bytes().to_vec();
    out.extend_from_slice(&(list.len() as u16).to_le_bytes());
    out.extend(utf16(description));
    out.extend_from_slice(list);
    out.extend_from_slice(optional);
    out
}

#[test]
fn reads_load_options() {
    let mut list = path(&["\\EFI\\fedora\\shimx64.efi"]);
    list.extend_from_slice(&[0x7f, 0x01, 4, 0]);
    list.extend(path(&["\\initrd"]));
    list.extend_from_slice(&END_ENTIRE);
    let option = parse_load_option(&load_option("Fedora", &list, b"opts")).unwrap();
    assert_eq!(option.description, "Fedora");
    let mut first = path(&["\\EFI\\fedora\\shimx64.efi"]);
    first.extend_from_slice(&END_ENTIRE);
    assert_eq!(option.device_path, first, "only the first instance");
}

#[test]
fn rejects_malformed_load_options() {
    assert_eq!(parse_load_option(&[1, 0, 0, 0, 8]), None);
    let mut list = path(&["\\a.efi"]);
    list.extend_from_slice(&END_ENTIRE);
    let mut data = load_option("A", &list, &[]);
    data.truncate(data.len() - 2);
    assert_eq!(parse_load_option(&data), None, "list shorter than stated");
    let bad = node(MEDIA, 1, &[]);
    let mut short = bad.clone();
    short[2] = 2;
    assert_eq!(parse_load_option(&load_option("B", &short, &[])), None);
}

#[test]
fn finds_the_file_a_path_ends_in() {
    let mut p = path(&["/EFI/Microsoft/Boot/bootmgfw.efi"]);
    p.extend_from_slice(&END_ENTIRE);
    assert_eq!(
        file_path(&p).as_deref(),
        Some("\\EFI\\Microsoft\\Boot\\bootmgfw.efi")
    );
    let mut split = path(&["\\EFI", "BOOT", "\\BOOTX64.EFI"]);
    split.extend_from_slice(&END_ENTIRE);
    assert_eq!(
        file_path(&split).as_deref(),
        Some("\\EFI\\BOOT\\BOOTX64.EFI")
    );
    let mut disk = path(&[]);
    disk.extend_from_slice(&END_ENTIRE);
    assert_eq!(file_path(&disk), None, "no file path node");
    let mut firmware_file = node(MEDIA, 6, &[1; 16]);
    firmware_file.extend_from_slice(&END_ENTIRE);
    assert_eq!(file_path(&firmware_file), None);
}

#[test]
fn compares_paths_without_case() {
    assert!(same_file(
        "\\EFI\\BOOT\\BOOTX64.EFI",
        "\\efi\\boot\\bootx64.efi"
    ));
    assert!(!same_file("\\EFI\\a.efi", "\\EFI\\b.efi"));
    assert!(in_mikros_dir("\\efi\\MIKROS\\bootmenu.efi"));
    assert!(!in_mikros_dir("\\EFI\\Mikros"));
    assert!(!in_mikros_dir("\\EFI\\MikrosOld\\x.efi"));
}

/// A minimal PE image with the given section names.
fn pe(sections: &[&[u8; 8]]) -> Vec<u8> {
    let mut image = vec![0u8; 0x40];
    image[..2].copy_from_slice(b"MZ");
    image[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    image.extend_from_slice(b"PE\0\0");
    let mut coff = [0u8; 20];
    coff[2..4].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    coff[16..18].copy_from_slice(&16u16.to_le_bytes());
    image.extend_from_slice(&coff);
    image.extend_from_slice(&[0; 16]);
    for name in sections {
        image.extend_from_slice(*name);
        image.extend_from_slice(&[0; 32]);
    }
    image
}

#[test]
fn finds_pe_sections() {
    const TEXT: &[u8; 8] = b".text\0\0\0";
    assert!(has_pe_section(&pe(&[TEXT, MIKROS_SECTION]), MIKROS_SECTION));
    assert!(!has_pe_section(&pe(&[TEXT]), MIKROS_SECTION));
    let mut truncated = pe(&[MIKROS_SECTION]);
    truncated.truncate(truncated.len() - 40);
    assert!(!has_pe_section(&truncated, MIKROS_SECTION));
    assert!(!has_pe_section(b"not an image", MIKROS_SECTION));
}

#[test]
fn skips_files_already_started_on_the_same_volume() {
    let mut started = Started::new();
    assert!(started.insert(1, "\\EFI\\a.efi"));
    assert!(!started.insert(1, "\\efi\\A.EFI"));
    assert!(started.insert(2, "\\EFI\\a.efi"), "other volume");
    assert!(started.contains(&2, "\\EFI\\A.efi"));
}

#[test]
fn gives_unused_ids() {
    let used = ["detected-2"];
    let ids: Vec<String> = ids(|id| used.contains(&id)).take(2).collect();
    assert_eq!(ids, ["detected-1", "detected-3"]);
}

/// Volumes, load options and files of a fake firmware. Volumes are
/// numbered, and the hard drive node of an option's device path holds the
/// number of the volume it is on.
#[derive(Default)]
struct FakeDisks {
    order: Vec<u16>,
    options: Vec<(u16, LoadOption)>,
    /// The volumes that are present.
    present: Vec<u8>,
    files: Vec<(u8, &'static str, Vec<u8>)>,
}

/// The volume used as the boot volume.
const BOOT: u8 = 1;

/// The volume an invalid device path is on.
const INVALID: u8 = 0xff;

impl FakeDisks {
    fn option(&mut self, number: u16, description: &str, volume: u8, file: Option<&str>) {
        let mut device_path = node(MEDIA, 1, &[volume; 38]);
        if let Some(file) = file {
            device_path.extend(node(MEDIA, MEDIA_FILE_PATH, &utf16(file)));
        }
        device_path.extend_from_slice(&END_ENTIRE);
        self.order.push(number);
        self.options.push((
            number,
            LoadOption {
                description: description.to_string(),
                device_path,
            },
        ));
    }
}

impl Disks for FakeDisks {
    type Volume = u8;
    /// The volume and path of the file.
    type DevicePath = (u8, String);

    fn boot_order(&mut self) -> Vec<u16> {
        self.order.clone()
    }

    fn load_option(&mut self, number: u16) -> Option<LoadOption> {
        let (_, option) = self.options.iter().find(|(n, _)| *n == number)?;
        Some(LoadOption {
            description: option.description.clone(),
            device_path: option.device_path.clone(),
        })
    }

    fn locate(&mut self, device_path: &[u8]) -> Option<((u8, String), Option<u8>)> {
        let volume = device_path[4];
        if volume == INVALID {
            return None;
        }
        let path = file_path(device_path).unwrap();
        let present = self.present.contains(&volume).then_some(volume);
        Some(((volume, path), present))
    }

    fn read(&mut self, volume: u8, path: &str) -> Option<Vec<u8>> {
        self.files
            .iter()
            .find(|(v, p, _)| *v == volume && same_file(p, path))
            .map(|(_, _, data)| data.clone())
    }

    fn exists(&mut self, volume: u8, path: &str) -> bool {
        self.read(volume, path).is_some()
    }

    fn file_device_path(&mut self, path: &str) -> Option<(u8, String)> {
        Some((BOOT, path.to_string()))
    }

    fn folders(&mut self, volume: u8, path: &str) -> Vec<String> {
        let prefix = alloc::format!("{}\\", path.to_lowercase());
        let mut found: Vec<String> = Vec::new();
        for (v, p, _) in &self.files {
            let Some(rest) = p
                .to_lowercase()
                .strip_prefix(&prefix)
                .map(|r| p[p.len() - r.len()..].to_string())
            else {
                continue;
            };
            if *v == volume
                && let Some((folder, _)) = rest.split_once('\\')
                && !found.iter().any(|f| f == folder)
            {
                found.push(folder.to_string());
            }
        }
        found
    }
}

fn config(text: &str) -> Config {
    Config::parse(text.as_bytes()).unwrap()
}

/// The name, path, volume and "Other disk" flag of each detected system.
fn found(disks: &mut FakeDisks, config: &Config) -> Vec<(String, String, u8, bool)> {
    find(disks, config, BOOT)
        .into_iter()
        .map(|d| (d.name, d.path, d.device_path.0, d.other_disk))
        .collect()
}

fn system(name: &str, path: &str, volume: u8, other_disk: bool) -> (String, String, u8, bool) {
    (name.to_string(), path.to_string(), volume, other_disk)
}

#[test]
fn adds_load_options_that_end_in_a_file_path() {
    let mut disks = FakeDisks {
        present: vec![BOOT, 2],
        ..FakeDisks::default()
    };
    disks.option(3, "Fedora", 2, Some("\\EFI\\fedora\\shimx64.efi"));
    disks.option(1, "PXE", 2, None);
    disks.option(2, "Broken", INVALID, Some("\\EFI\\x.efi"));
    disks.option(0, "Debian", BOOT, Some("/EFI/debian/grubx64.efi"));
    disks.order.push(9);
    assert_eq!(
        found(&mut disks, &config("")),
        [
            system("Fedora", "\\EFI\\fedora\\shimx64.efi", 2, true),
            system("Debian", "\\EFI\\debian\\grubx64.efi", BOOT, false),
        ]
    );
}

#[test]
fn skips_mikros_boot_files() {
    let mut disks = FakeDisks {
        present: vec![BOOT, 2],
        ..FakeDisks::default()
    };
    disks.option(0, "Mikros", BOOT, Some("\\EFI\\Mikros\\bootmenu.efi"));
    disks.option(1, "Mikros 2", 2, Some("\\efi\\mikros\\mikrosboot.efi"));
    disks.option(2, "Fallback", BOOT, Some(FALLBACK_PATH));
    disks.option(3, "Other fallback", 2, Some(FALLBACK_PATH));
    disks
        .files
        .push((BOOT, FALLBACK_PATH, pe(&[MIKROS_SECTION])));
    disks.files.push((2, FALLBACK_PATH, pe(&[b".text\0\0\0"])));
    assert_eq!(
        found(&mut disks, &config("")),
        [system("Other fallback", FALLBACK_PATH, 2, true)]
    );
}

#[test]
fn adds_each_file_on_a_volume_once() {
    let mut disks = FakeDisks {
        present: vec![BOOT, 2],
        ..FakeDisks::default()
    };
    disks.option(0, "Shell", BOOT, Some("\\EFI\\tools\\shell.efi"));
    disks.option(1, "Shell 2", 2, Some("\\EFI\\tools\\Shell.efi"));
    disks.option(2, "Shell 3", 2, Some("\\EFI\\TOOLS\\SHELL.EFI"));
    disks.option(3, "Absent", 3, Some("\\EFI\\tools\\Shell.efi"));
    let config =
        config("entries:\n  - id: shell\n    name: Shell\n    path: /EFI/tools/Shell.efi\n");
    assert_eq!(
        found(&mut disks, &config),
        [
            system("Shell 2", "\\EFI\\tools\\Shell.efi", 2, true),
            system("Absent", "\\EFI\\tools\\Shell.efi", 3, true),
        ]
    );
}

#[test]
fn adds_options_with_the_same_device_path_once_when_the_volume_is_absent() {
    let mut disks = FakeDisks {
        present: vec![BOOT],
        ..FakeDisks::default()
    };
    let shim = "\\EFI\\fedora\\shimx64.efi";
    disks.option(0, "Fedora", 3, Some(shim));
    disks.option(1, "Fedora again", 3, Some(shim));
    disks.option(2, "Other disk", 4, Some(shim));
    disks.option(3, "Grub", 3, Some("\\EFI\\fedora\\grubx64.efi"));
    disks.option(4, "Other disk again", 4, Some(shim));
    disks.order.push(0);
    assert_eq!(
        found(&mut disks, &config("")),
        [
            system("Fedora", shim, 3, true),
            system("Other disk", shim, 4, true),
            system("Grub", "\\EFI\\fedora\\grubx64.efi", 3, true),
        ]
    );
}

#[test]
fn adds_known_images_on_the_boot_volume_after_load_options() {
    let mut disks = FakeDisks {
        present: vec![BOOT, 2],
        ..FakeDisks::default()
    };
    let [windows, linux, refind] = KNOWN_IMAGES.map(|(path, _)| path);
    disks.option(
        0,
        "Windows Boot Manager",
        BOOT,
        Some(&windows.to_lowercase()),
    );
    disks.option(1, "Other", 2, Some("\\EFI\\other.efi"));
    disks.files.push((BOOT, windows, Vec::new()));
    disks.files.push((BOOT, refind, Vec::new()));
    disks.files.push((2, linux, Vec::new()));
    assert_eq!(
        found(&mut disks, &config("")),
        [
            system("Windows Boot Manager", &windows.to_lowercase(), BOOT, false),
            system("Other", "\\EFI\\other.efi", 2, true),
            system("rEFInd", refind, BOOT, false),
        ]
    );

    let entry = alloc::format!("entries:\n  - id: r\n    name: R\n    path: '{refind}'\n");
    let mut disks = FakeDisks {
        present: vec![BOOT],
        files: vec![(BOOT, refind, Vec::new()), (BOOT, linux, Vec::new())],
        ..FakeDisks::default()
    };
    assert_eq!(
        found(&mut disks, &config(&entry)),
        [system("Linux", linux, BOOT, false)],
        "already an entry"
    );
}

#[test]
fn gives_detected_systems_ids_the_entries_do_not_use() {
    let mut disks = FakeDisks {
        present: vec![BOOT],
        ..FakeDisks::default()
    };
    disks.option(0, "A", BOOT, Some("\\a.efi"));
    disks.option(1, "B", BOOT, Some("\\b.efi"));
    let config = config("entries:\n  - id: detected-1\n    name: X\n    path: /x.efi\n");
    let ids: Vec<String> = find(&mut disks, &config, BOOT)
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(ids, ["detected-2", "detected-3"]);
}

#[test]
fn adds_shim_or_grub_from_other_folders_by_name() {
    let [shim, grub] = LOADERS;
    let path = |folder: &str, image: &str| alloc::format!("\\EFI\\{folder}\\{image}");
    let leak = |s: String| -> &'static str { alloc::boxed::Box::leak(s.into_boxed_str()) };
    let mut disks = FakeDisks {
        present: vec![BOOT],
        files: vec![
            (BOOT, leak(path("ubuntu", shim)), Vec::new()),
            (BOOT, leak(path("ubuntu", grub)), Vec::new()),
            (BOOT, leak(path("Fedora", grub)), Vec::new()),
            (BOOT, leak(path("arch", "vmlinuz.efi")), Vec::new()),
            // Folders found otherwise, or Mikros's own, are not searched.
            (BOOT, leak(path("BOOT", grub)), Vec::new()),
            (BOOT, leak(path("mikros", grub)), Vec::new()),
            (2, leak(path("debian", shim)), Vec::new()),
        ],
        ..FakeDisks::default()
    };
    assert_eq!(
        found(&mut disks, &config("")),
        [
            system("Fedora", &path("Fedora", grub), BOOT, false),
            system("ubuntu", &path("ubuntu", shim), BOOT, false),
        ]
    );
    let entry = alloc::format!(
        "entries:\n  - id: u\n    name: U\n    path: '{}'\n",
        path("ubuntu", shim)
    );
    assert_eq!(
        found(&mut disks, &config(&entry)),
        [system("Fedora", &path("Fedora", grub), BOOT, false)],
        "already an entry"
    );
}
