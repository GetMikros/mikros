use super::*;

const MANIFEST_TEXT: &[u8] = b"id: org.example.app\nkind: app\nversion: 1.0.0\n";

fn sample() -> Builder {
    let mut b = Builder::default();
    b.file(MANIFEST, MANIFEST_TEXT.to_vec(), false);
    b.folder("bin");
    b.file("bin/app", b"\x7fELF program".to_vec(), true);
    b.file("share/data.txt", b"some data".to_vec(), false);
    b.symlink("bin/current", "app");
    b.symlink("share/up", "../bin/app");
    b
}

/// The index entry of `path` in the package `bytes`, where it lies.
fn entry_at(bytes: &[u8], path: &str) -> usize {
    let package = Package::open(bytes).unwrap();
    let at = package.entries.iter().position(|e| e.path == path).unwrap();
    HEADER + ENTRY * at
}

#[test]
fn writes_packages_that_read_back() {
    let builder = sample();
    let checksum = builder.checksum(None);
    let bytes = builder.finish();
    let package = Package::open(&bytes[..]).unwrap();
    let paths: Vec<&str> = package.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "bin",
            "bin/app",
            "bin/current",
            "manifest.yaml",
            "share/data.txt",
            "share/up"
        ]
    );
    let app = package.find("bin/app").unwrap();
    assert!(app.executable() && app.data_offset.is_multiple_of(ALIGN));
    assert_eq!(package.read(&bytes[..], app).unwrap(), b"\x7fELF program");
    let link = package.find("bin/current").unwrap();
    assert_eq!(package.read(&bytes[..], link).unwrap(), b"app");
    assert_eq!(package.checksum(None), checksum);
    // The manifest and the signature file are left out of the checksum.
    let mut changed = sample();
    changed.entries.retain(|e| e.0 != MANIFEST);
    changed.file(MANIFEST, b"id: other\n".to_vec(), false);
    assert_eq!(changed.checksum(None), checksum);
    assert_ne!(sample().checksum(Some("share/data.txt")), checksum);
    assert!(checksum_text(&checksum).starts_with("sha256:"));
    assert_eq!(checksum_text(&checksum).len(), 7 + 64);
}

#[test]
fn checksums_each_entry_with_its_path_type_and_flags() {
    // The checksum of one file, worked out by hand.
    let mut b = Builder::default();
    b.file(MANIFEST, MANIFEST_TEXT.to_vec(), false);
    b.file("a", b"x".to_vec(), true);
    let mut sum = Sha256::new();
    sum.update(b"a\0\x01\x01\x00");
    sum.update(Sha256::digest(b"x"));
    assert_eq!(b.checksum(None), <[u8; 32]>::from(sum.finalize()));
}

#[test]
fn reads_compressed_files() {
    let bytes = sample().finish();
    let at = entry_at(&bytes, "share/data.txt");
    let mut bytes = bytes;
    let contents = b"some data".to_vec();
    let frame = ruzstd::encoding::compress_to_vec(
        &contents[..],
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    let offset = bytes.len() as u64;
    bytes.extend_from_slice(&frame);
    bytes[at..at + 8].copy_from_slice(&offset.to_le_bytes());
    bytes[at + 0x10..at + 0x18].copy_from_slice(&(frame.len() as u64).to_le_bytes());
    bytes[at + 0x21] = 1;
    let package = Package::open(&bytes[..]).unwrap();
    let entry = package.find("share/data.txt").unwrap();
    assert_eq!(package.read(&bytes[..], entry).unwrap(), contents);
    // Two frames are not one.
    let mut two = bytes.clone();
    two.extend_from_slice(&frame);
    two[at + 0x10..at + 0x18].copy_from_slice(&(2 * frame.len() as u64).to_le_bytes());
    let package = Package::open(&two[..]).unwrap();
    let entry = package.find("share/data.txt").unwrap();
    assert!(matches!(
        package.read(&two[..], entry),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn refuses_packages_that_break_the_format() {
    let good = sample().finish();
    let refused = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = good.clone();
        change(&mut bytes);
        matches!(Package::open(&bytes[..]), Err(Error::Invalid(_)))
    };
    assert!(refused(&|b| b[0] = b'X'));
    assert!(refused(&|b| b[8] = 2));
    assert!(refused(&|b| b[0xC] = 200));
    // Paths out of order: swap the first two entries.
    assert!(refused(&|b| {
        let (first, second) = (HEADER, HEADER + ENTRY);
        let a = b[first..first + ENTRY].to_vec();
        let c = b[second..second + ENTRY].to_vec();
        b[first..first + ENTRY].copy_from_slice(&c);
        b[second..second + ENTRY].copy_from_slice(&a);
    }));
    let app = entry_at(&good, "bin/app");
    // Data that is not page-aligned, and an unknown type.
    assert!(refused(&|b| b[app] = 1));
    assert!(refused(&|b| b[app + 0x20] = 9));
    assert!(refused(&|b| b[app + 0x21] = 2));
    // A folder with a checksum.
    let bin = entry_at(&good, "bin");
    assert!(refused(&|b| b[bin + 0x28] = 1));
    // A file whose contents do not match its checksum reads as invalid.
    let mut bytes = good.clone();
    let offset = u64::from_le_bytes(bytes[app..app + 8].try_into().unwrap()) as usize;
    bytes[offset] ^= 1;
    let package = Package::open(&bytes[..]).unwrap();
    let entry = package.find("bin/app").unwrap();
    assert!(matches!(
        package.read(&bytes[..], entry),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn refuses_bad_paths_and_links_and_a_missing_manifest() {
    for (path, target) in [
        ("a", "/etc/x"),
        ("a", "../x"),
        ("d/a", "../../x"),
        ("a", ""),
    ] {
        let mut b = Builder::default();
        b.file(MANIFEST, MANIFEST_TEXT.to_vec(), false);
        b.symlink(path, target);
        assert!(
            Package::open(&b.finish()[..]).is_err(),
            "{path} -> {target}"
        );
    }
    for path in ["a//b", "./a", "a/..", "/a"] {
        let mut b = Builder::default();
        b.file(MANIFEST, MANIFEST_TEXT.to_vec(), false);
        b.file(path, Vec::new(), false);
        assert!(Package::open(&b.finish()[..]).is_err(), "{path}");
    }
    let mut b = Builder::default();
    b.file("other.yaml", Vec::new(), false);
    assert_eq!(
        Package::open(&b.finish()[..]).unwrap_err(),
        Error::Invalid("no manifest.yaml at the root")
    );
    let mut b = Builder::default();
    b.file(MANIFEST, MANIFEST_TEXT.to_vec(), false);
    b.file("a", Vec::new(), false);
    b.file("a", Vec::new(), false);
    assert!(Package::open(&b.finish()[..]).is_err());
}
