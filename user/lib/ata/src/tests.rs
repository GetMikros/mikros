use super::*;

/// IDENTIFY DEVICE words of a 48-bit disk of 1000 sectors of 512 bytes,
/// with the model "QEMU HARDDISK" and the serial "mikros".
fn words() -> [u16; 256] {
    let mut w = [0u16; 256];
    let put = |w: &mut [u16; 256], at: usize, text: &[u8], len: usize| {
        for i in 0..len {
            let byte = |j: usize| u16::from(*text.get(j).unwrap_or(&b' '));
            w[at + i] = byte(2 * i) << 8 | byte(2 * i + 1);
        }
    };
    put(&mut w, 10, b"mikros", 10);
    put(&mut w, 27, b"QEMU HARDDISK", 20);
    w[83] = 1 << 14 | 1 << 10;
    w[84] = 1 << 14;
    w[87] = 1 << 14;
    w[100] = 1000;
    w
}

#[test]
fn reads_the_disk_s_identity() {
    let identity = Identity::parse(&words()).unwrap();
    assert_eq!(identity.model, "QEMU HARDDISK");
    assert_eq!(identity.serial, "mikros");
    assert!(identity.lba48);
    assert_eq!(identity.sectors, 1000);
    assert_eq!(identity.sector_size, 512);
    assert!(!identity.ncq && !identity.trim && !identity.write_cache && !identity.fua);
    assert_eq!(identity.queue_depth, 1);
}

#[test]
fn reads_queuing_trim_and_the_write_cache() {
    let mut w = words();
    w[75] = 31;
    w[76] = 1 << 8;
    w[169] = 1;
    w[85] = 1 << 5;
    let identity = Identity::parse(&w).unwrap();
    assert!(identity.ncq && identity.trim && identity.write_cache);
    assert_eq!(identity.queue_depth, 32);
}

#[test]
fn reports_forced_unit_access_from_word_84_or_87() {
    for at in [84, 87] {
        let mut w = words();
        w[at] |= 1 << 6;
        assert!(Identity::parse(&w).unwrap().fua, "word {at}");
    }
    let mut w = words();
    w[84] |= 1 << 5 | 1 << 7;
    assert!(!Identity::parse(&w).unwrap().fua);
}

#[test]
fn takes_long_logical_sectors_only_as_powers_of_two() {
    let long = |w117: u16, w118: u16| {
        let mut w = words();
        w[106] = 0x4000 | 1 << 12;
        w[117] = w117;
        w[118] = w118;
        Identity::parse(&w).map(|i| i.sector_size)
    };
    assert_eq!(long(2048, 0), Some(4096));
    assert_eq!(long(260, 0), None);
    assert_eq!(long(128, 0), None);
    assert_eq!(long(0, 0x8000), None);
    // Word 106 without its valid pattern leaves sectors at 512 bytes.
    let mut w = words();
    w[106] = 1 << 12;
    w[117] = 2048;
    assert_eq!(Identity::parse(&w).unwrap().sector_size, 512);
}

#[test]
fn reads_and_writes_without_queuing() {
    let plain = |write| Ata {
        command: if write { WRITE_DMA_EXT } else { READ_DMA_EXT },
        lba: 7,
        count: 8,
        device: DEVICE_LBA,
        ..Ata::default()
    };
    assert_eq!(
        read_write(false, false, false, true, 7, 8),
        (plain(false), false)
    );
    assert_eq!(
        read_write(false, true, false, true, 7, 8),
        (plain(true), false)
    );
    // fua has no effect on a read.
    assert_eq!(
        read_write(false, false, true, true, 7, 8),
        (plain(false), false)
    );
    let forced = Ata {
        command: WRITE_DMA_FUA_EXT,
        ..plain(true)
    };
    assert_eq!(read_write(false, true, true, true, 7, 8), (forced, false));
    // Without forced unit access on the disk, a flush follows.
    assert_eq!(
        read_write(false, true, true, false, 7, 8),
        (plain(true), true)
    );
}

#[test]
fn reads_and_writes_queued() {
    let queued = |write| Ata {
        command: if write {
            WRITE_FPDMA_QUEUED
        } else {
            READ_FPDMA_QUEUED
        },
        features: 8,
        lba: 7,
        device: DEVICE_LBA,
        ..Ata::default()
    };
    assert_eq!(
        read_write(true, false, true, true, 7, 8),
        (queued(false), false)
    );
    assert_eq!(
        read_write(true, true, false, true, 7, 8),
        (queued(true), false)
    );
    let forced = Ata {
        device: DEVICE_LBA | FPDMA_FUA,
        ..queued(true)
    };
    assert_eq!(read_write(true, true, true, true, 7, 8), (forced, false));
    assert_eq!(
        read_write(true, true, true, false, 7, 8),
        (queued(true), true)
    );
}
