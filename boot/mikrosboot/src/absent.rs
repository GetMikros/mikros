//! Checks that the UEFI code never uses some firmware services or
//! instructions. What it does not call cannot be observed at run time, so
//! these read its source.

/// The source of the UEFI code.
const SOURCES: [(&str, &str); 4] = [
    ("main.rs", include_str!("main.rs")),
    ("efi/mod.rs", include_str!("efi/mod.rs")),
    ("efi/gather.rs", include_str!("efi/gather.rs")),
    ("efi/enter.rs", include_str!("efi/enter.rs")),
];

/// Fails when a source contains any of `words`, ignoring case.
fn assert_unused(words: &[&str]) {
    for (file, source) in SOURCES {
        let source = source.to_lowercase();
        for word in words {
            assert!(!source.contains(&word.to_lowercase()), "{file} uses {word}");
        }
    }
}

#[test]
fn never_uses_multiprocessor_services() {
    assert_unused(&["MpServices", "mp::", "startup_all_aps", "startup_this_ap"]);
}

#[test]
fn never_enables_interrupts() {
    assert_unused(&["\"sti", "; sti", "enable_interrupts"]);
}

#[test]
fn disables_interrupts_and_clears_df_first_on_entry() {
    let enter = SOURCES[3].1;
    let start = enter.find("\"mikros_enter:\",").expect("mikros_enter") + 16;
    let mut instructions = enter[start..]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"));
    assert_eq!(instructions.next(), Some("\"cli\","));
    assert_eq!(instructions.next(), Some("\"cld\","));
}
