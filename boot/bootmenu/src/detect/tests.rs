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
