use super::*;

const SNAPSHOT: &str = "U\t12345000000\n\
    O\tProcess\t2\n\
    B\t65536\t300\n\
    C\t0\t7\t1500000000\n\
    K\tabcd\n\
    H\t16\t10\n\
    H\tpages\t3\n\
    F\t1000\t400\t5\n\
    D\t8192\t1048576\n\
    S\t0\t1\n\
    I\t0x0010\t2\t5\n\
    P\t5\t1\t4\t2097152\tinit\n\
    T\t7\t5\tblocked\t9\t0\t250000000\tinit\n\
    X\tunknown\n";

fn identity(koid: u64) -> Option<Identity> {
    (koid == 5).then(|| Identity {
        user: 0,
        app_id: "system.init".into(),
    })
}

#[test]
fn renders_a_table_per_section_with_identities() {
    let text = render(SNAPSHOT, &identity, usize::MAX);
    let tables: Vec<&str> = text.split("\n\n").collect();
    assert_eq!(tables.len(), 9, "{text}");
    assert_eq!(tables[0], "System\nuptime s  build ID\n12.345    abcd");
    assert!(
        tables[1].contains("frames in total        1000 (3.9 MiB)"),
        "{}",
        tables[1]
    );
    assert!(
        tables[1].contains("kernel and other       300 (1.1 MiB)"),
        "{}",
        tables[1]
    );
    assert!(
        tables[1].contains("kernel dynamic region  8 KiB of 1.0 MiB"),
        "{}",
        tables[1]
    );
    assert!(
        tables[1].ends_with("kernel memory charged  64 KiB"),
        "{}",
        tables[1]
    );
    assert_eq!(
        tables[2],
        "Processors\ncpu  thread  idle s  shootdown pending\n0    7       1.500   yes"
    );
    assert_eq!(
        tables[3],
        "Processes\nkoid  user  app ID       threads  handles  mapped   name\n\
         5     0     system.init  1        4        2.0 MiB  init"
    );
    assert!(
        tables[4].ends_with("7     5        blocked  9         0    0.250  init"),
        "{}",
        tables[4]
    );
    assert_eq!(tables[5], "Kernel objects\ntype     live\nProcess  2");
    assert!(
        tables[6].contains("pages       3       12 KiB"),
        "{}",
        tables[6]
    );
    assert_eq!(
        tables[7],
        "IOMMU domains\ndevice  domain  pinned pages\n0x0010  2       5"
    );
}

#[test]
fn leaves_unknown_processes_without_identity_and_cuts_the_text() {
    let text = render("P\t9\t0\t0\t0\tx\n", &identity, usize::MAX);
    assert!(
        text.contains("\n9                   0        0        0 KiB   x\n"),
        "{text:?}"
    );
    assert_eq!(render(SNAPSHOT, &identity, 10), "System\n");
    assert_eq!(render(SNAPSHOT, &identity, 6), "");
}
