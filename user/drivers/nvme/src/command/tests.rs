use super::*;

#[test]
fn sets_force_unit_access_only_on_fua_writes() {
    let fua = Command::read_write(true, 1, 0, 8, 0x1000, 0, true).entry(7);
    assert_eq!(fua[0] & 0xFF, u32::from(WRITE));
    assert_eq!(fua[12] & FUA, FUA);
    assert_eq!(fua[12] & !FUA, 7);
    let plain = Command::read_write(true, 1, 0, 8, 0x1000, 0, false).entry(7);
    assert_eq!(plain[12], 7);
    // A read has no Force Unit Access bit to set.
    let read = Command::read_write(false, 1, 0, 8, 0x1000, 0, true).entry(7);
    assert_eq!(read[0] & 0xFF, u32::from(READ));
    assert_eq!(read[12], 7);
}

#[test]
fn lays_out_the_submission_queue_entry() {
    let block = 0x1234_5678_9ABC;
    let command = Command::read_write(true, 3, block, 1, 0x1_2345_6000, 0x7_0000_8000, true);
    let entry = command.entry(0xBEEF);
    assert_eq!(entry[0], 0xBEEF_0001);
    assert_eq!(entry[1], 3);
    assert_eq!(entry[2..6], [0; 4]);
    assert_eq!(entry[6..10], [0x2345_6000, 1, 0x8000, 7]);
    assert_eq!(entry[10..12], [0x5678_9ABC, 0x1234]);
    // The number of blocks is zero-based.
    assert_eq!(entry[12], FUA);
    assert_eq!(entry[13..], [0; 3]);
}
