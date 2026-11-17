use super::*;
use crate::format::Builder;
use alloc::format;
use alloc::string::ToString;

const FULL: &str = "id: org.example.editor
kind: app
version: 1.2.0
name: Editor
description: Edits text.
icon: share/icon.png
checksum: sha256:0000000000000000000000000000000000000000000000000000000000000000
executable: bin/editor
portable: false
install: local
needs:
  org.example.lib: '>=1.0.0,<2.0.0'
components: [plugins/spell.mika]
bins:
  - name: edit
    path: bin/editor
services:
  indexer: services/indexer.yaml
";

#[test]
fn reads_every_key() {
    let m = Manifest::parse(FULL.as_bytes()).unwrap();
    assert_eq!(m.id, "org.example.editor");
    assert_eq!(m.kind, Some(Kind::App));
    assert_eq!(m.version.as_ref().unwrap().to_string(), "1.2.0");
    assert_eq!(m.name.as_deref(), Some("Editor"));
    assert_eq!(m.icon.as_deref(), Some("share/icon.png"));
    assert_eq!(m.executable.as_deref(), Some("bin/editor"));
    assert!(!m.portable && m.installable && !m.global);
    assert_eq!(m.needs.len(), 1);
    assert_eq!(m.needs[0].0, "org.example.lib");
    assert_eq!(m.components, ["plugins/spell.mika"]);
    assert_eq!(m.bins[0].name, "edit");
    assert_eq!(m.services[0].0, "indexer");
    assert!(m.check().is_ok());
}

#[test]
fn needs_only_an_id_to_parse_and_more_to_check() {
    let m = Manifest::parse(b"id: org.example.x\n").unwrap();
    assert!(m.portable && m.installable && m.global);
    assert_eq!(m.check().unwrap_err().key, "kind");
    let packed = "id: org.example.x\nkind: library\nversion: 0.1.0\n";
    let m = Manifest::parse(packed.as_bytes()).unwrap();
    assert_eq!(m.check().unwrap_err().key, "name");
    // An app needs an executable and a driver its manifest.
    let app = FULL.replace("executable: bin/editor\n", "");
    assert_eq!(
        Manifest::parse(app.as_bytes())
            .unwrap()
            .check()
            .unwrap_err()
            .key,
        "executable"
    );
    let driver = FULL
        .replace("kind: app", "kind: driver")
        .replace("install: local", "install: global");
    assert_eq!(
        Manifest::parse(driver.as_bytes())
            .unwrap()
            .check()
            .unwrap_err()
            .key,
        "driver"
    );
    let system = FULL.replace("org.example.editor", "system.editor");
    assert_eq!(
        Manifest::parse(system.as_bytes())
            .unwrap()
            .check()
            .unwrap_err()
            .key,
        "id"
    );
}

#[test]
fn refuses_keys_in_the_wrong_form() {
    for (from, to, key) in [
        ("id: org.example.editor", "id: Editor", "id"),
        ("kind: app", "kind: game", "kind"),
        ("version: 1.2.0", "version: 1.2", "version"),
        ("icon: share/icon.png", "icon: /icon.png", "icon"),
        ("icon: share/icon.png", "icon: ../icon.png", "icon"),
        ("portable: false", "portable: maybe", "portable"),
        ("install: local", "install: everywhere", "install"),
        ("kind: app", "kind: driver", "install"),
        ("'>=1.0.0,<2.0.0'", "'~1.0'", "needs"),
        ("org.example.lib:", "Lib:", "needs"),
        ("name: edit", "name: Edit", "bins"),
        ("checksum: sha256:0000", "checksum: md5:0000", "checksum"),
    ] {
        let text = FULL.replace(from, to);
        assert_eq!(
            Manifest::parse(text.as_bytes()).unwrap_err().key,
            key,
            "{to}"
        );
    }
    assert!(is_command("vim.tiny") && is_command("g++") && is_command("7z"));
    assert!(!is_command("-x") && !is_command("Vim") && !is_command(""));
}

#[test]
fn checks_a_package_against_its_manifest() {
    let mut b = Builder::default();
    b.file("bin/editor", b"program".to_vec(), true);
    let sum = crate::format::checksum_text(&b.checksum(None));
    let text = format!(
        "id: org.example.editor\nkind: app\nversion: 1.0.0\nname: E\ndescription: D\n\
         executable: bin/editor\nchecksum: {sum}\n"
    );
    b.file(MANIFEST, text.as_bytes().to_vec(), false);
    let bytes = b.finish();
    let package = format::Package::open(&bytes[..]).unwrap();
    let m = Manifest::parse(text.as_bytes()).unwrap();
    assert!(m.check_package(&package).is_ok());
    // Another checksum, and an executable the package does not hold.
    let zeros = format!("sha256:{}", "0".repeat(64));
    let other = Manifest::parse(text.replace(&sum, &zeros).as_bytes()).unwrap();
    assert_eq!(other.check_package(&package).unwrap_err().key, "checksum");
    let missing = text.replace("bin/editor", "bin/other");
    let m = Manifest::parse(missing.as_bytes()).unwrap();
    assert_eq!(m.check_package(&package).unwrap_err().key, "manifest");
}
