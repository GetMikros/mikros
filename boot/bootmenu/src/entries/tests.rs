use super::*;

fn parse(text: &str) -> Result<Config, Error> {
    Config::parse(text.as_bytes())
}

fn kind_at(text: &str) -> (Option<usize>, ErrorKind) {
    let err = parse(text).unwrap_err();
    (err.line, err.kind)
}

#[test]
fn reads_a_full_file() {
    let text = "\
# Boot entries
default: second
timeout: 3
detect: false
entries:
  - id: first
    name: First system
    path: /EFI/first/boot.efi
  - id: second   # comment after a value
    name: \"Second \\\"quoted\\\" \\/ system\"
    detail: 'it''s here'
    color: \"#a0B1c2\"
    icon: EFI\\second\\icon.bmp
    path: \\EFI\\second\\boot.efi
    args: one two
";
    let config = parse(text).unwrap();
    assert_eq!(config.default.as_deref(), Some("second"));
    assert_eq!(config.timeout, Some(3));
    assert!(!config.detect);
    assert_eq!(config.default_index(), 1);
    assert_eq!(config.entries.len(), 2);

    let first = &config.entries[0];
    assert_eq!(first.path, "\\EFI\\first\\boot.efi");
    assert_eq!(first.color, Color::DEFAULT_TILE);
    assert_eq!(first.detail, None);

    let second = &config.entries[1];
    assert_eq!(second.id, "second");
    assert_eq!(second.name, "Second \"quoted\" / system");
    assert_eq!(second.detail.as_deref(), Some("it's here"));
    assert_eq!(
        second.color,
        Color {
            r: 0xa0,
            g: 0xb1,
            b: 0xc2
        }
    );
    assert_eq!(second.icon.as_deref(), Some("\\EFI\\second\\icon.bmp"));
    assert_eq!(second.path, "\\EFI\\second\\boot.efi");
    assert_eq!(second.args.as_deref(), Some("one two"));
}

#[test]
fn defaults_without_keys() {
    let config = parse("# nothing here\n\n").unwrap();
    assert_eq!(config.default, None);
    assert_eq!(config.timeout, None);
    assert!(config.entries.is_empty());
    assert!(config.detect);
    assert_eq!(config.default_index(), 0);
}

#[test]
fn missing_file_configuration() {
    let config = Config::for_missing_file();
    assert_eq!(config.timeout, Some(0));
    assert_eq!(config.entries.len(), 1);
    assert_eq!(config.entries[0].id, "mikros");
    assert_eq!(config.entries[0].path, "\\EFI\\Mikros\\mikrosboot.efi");
}

#[test]
fn accepts_bom_crlf_and_zero_indented_lists() {
    let text = "\u{feff}entries:\r\n- id: a\r\n  name: A\r\n  path: a.efi\r\n";
    let config = parse(text).unwrap();
    assert_eq!(config.entries[0].path, "\\a.efi");
}

#[test]
fn rejects_invalid_utf8() {
    let err = Config::parse(b"timeout: \xff\n").unwrap_err();
    assert_eq!(
        err,
        Error {
            line: None,
            kind: ErrorKind::NotUtf8
        }
    );
}

#[test]
fn ignores_unknown_keys_and_their_nested_lines() {
    let text = "\
theme:
  colors: [not, parsed]
  - {also: ignored}
future: { x: 1 }
entries:
  - id: a
    name: A
    path: a.efi
    extra:
      - anything: [goes]
    other: 'x'
  - id: b
    name: B
    path: b.efi
";
    let config = parse(text).unwrap();
    assert_eq!(config.entries.len(), 2);
    assert_eq!(config.entries[1].id, "b");
}

#[test]
fn reports_duplicate_keys() {
    assert_eq!(
        kind_at("timeout: 1\ntimeout: 2\n"),
        (Some(2), ErrorKind::DuplicateKey("timeout".into()))
    );
    let text = "entries:\n  - id: a\n    name: A\n    name: B\n    path: a\n";
    assert_eq!(
        kind_at(text),
        (Some(4), ErrorKind::DuplicateKey("name".into()))
    );
}

#[test]
fn reports_entry_problems() {
    let entry = |body: &str| format!("entries:\n  - id: a\n{body}");
    assert_eq!(
        kind_at(&entry("    name: A\n")),
        (Some(2), ErrorKind::MissingKey("path"))
    );
    assert_eq!(
        kind_at(&entry("    name:\n    path: a\n")),
        (Some(3), ErrorKind::EmptyValue("name".into()))
    );
    assert_eq!(
        kind_at(&entry("    name: \"\"\n    path: a\n")),
        (Some(3), ErrorKind::EmptyValue("name".into()))
    );
    assert_eq!(
        kind_at(&entry("    name: A\n    path: a\n    color: \"#12345\"\n")),
        (Some(5), ErrorKind::InvalidColor)
    );
    assert_eq!(
        kind_at("entries:\n  - id: a b\n    name: A\n    path: a\n"),
        (Some(2), ErrorKind::InvalidId)
    );
    let twice =
        "entries:\n  - id: a\n    name: A\n    path: a\n  - id: a\n    name: B\n    path: b\n";
    assert_eq!(
        kind_at(twice),
        (Some(5), ErrorKind::DuplicateId("a".into()))
    );
}

#[test]
fn reports_top_level_problems() {
    assert_eq!(
        kind_at("timeout: -1\n"),
        (Some(1), ErrorKind::InvalidTimeout)
    );
    assert_eq!(
        kind_at("timeout: 99999999999\n"),
        (Some(1), ErrorKind::InvalidTimeout)
    );
    assert_eq!(
        kind_at("detect: yes\n"),
        (Some(1), ErrorKind::InvalidDetect)
    );
    assert_eq!(kind_at("entries: x\n"), (Some(1), ErrorKind::ExpectedList));
    assert_eq!(
        kind_at("default: nope\n"),
        (Some(1), ErrorKind::UnknownDefault("nope".into()))
    );
}

#[test]
fn reports_structure_errors_with_lines() {
    assert_eq!(
        kind_at("timeout: 1\n\ttimeout: 2\n"),
        (Some(2), ErrorKind::Tab)
    );
    assert_eq!(
        kind_at("timeout: 1\n  detect: true\n"),
        (Some(2), ErrorKind::Indentation)
    );
    assert!(matches!(
        kind_at("timeout: 1\ndetect: [1]\n"),
        (Some(2), ErrorKind::Syntax(_))
    ));
    let nested = "entries:\n  - id: a\n    name:\n      deeper: x\n    path: a\n";
    assert_eq!(
        kind_at(nested),
        (Some(4), ErrorKind::ExpectedValue("name".into()))
    );
    let misaligned = "entries:\n  - id: a\n     name: A\n";
    assert_eq!(
        kind_at(misaligned),
        (Some(3), ErrorKind::ExpectedValue("id".into()))
    );
}

#[test]
fn normalizes_paths() {
    assert_eq!(normalize_path("/EFI/a/b.efi"), "\\EFI\\a\\b.efi");
    assert_eq!(normalize_path("EFI\\a/b.efi"), "\\EFI\\a\\b.efi");
    assert_eq!(normalize_path("\\EFI\\a.efi"), "\\EFI\\a.efi");
}

#[test]
fn formats_errors_with_line_numbers() {
    let err = parse("detect: maybe\n").unwrap_err();
    assert_eq!(
        err.to_string(),
        "line 1: `detect` must be `true` or `false`"
    );
}
