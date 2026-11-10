use super::*;
use alloc::vec;
use codes::*;

fn key(code: u16, value: i32) -> Event {
    Event::new(5, EV_KEY, code, value)
}

#[test]
fn bits_are_numbered_from_bit_0_of_byte_0() {
    let mut keys = [0u8; KEY_BYTES];
    set_bit(&mut keys, KEY_ESC);
    set_bit(&mut keys, KEY_A);
    set_bit(&mut keys, KEY_MAX);
    assert_eq!(keys[0], 0b10);
    assert_eq!(keys[3], 1 << 6);
    assert_eq!(keys[KEY_BYTES - 1], 0x80);
    assert!(bit(&keys, KEY_A));
    clear_bit(&mut keys, KEY_A);
    assert!(!bit(&keys, KEY_A));
    // Codes beyond the bitmap are ignored.
    set_bit(&mut keys, KEY_MAX + 1);
    assert!(!bit(&keys, KEY_MAX + 1));
}

#[test]
fn state_follows_keys_leds_and_axes() {
    let mut state = State::new(&[ABS_X, ABS_Y]);
    state.apply(&key(KEY_A, 1));
    state.apply(&key(KEY_B, 1));
    state.apply(&key(KEY_A, 0));
    state.apply(&Event::new(0, EV_LED, LED_CAPSL, 1));
    state.apply(&Event::new(0, EV_ABS, ABS_Y, 300));
    state.apply(&Event::new(0, EV_ABS, 7, 1));
    assert!(!bit(&state.keys, KEY_A));
    assert!(bit(&state.keys, KEY_B));
    assert_eq!(state.leds, 1 << LED_CAPSL);
    assert_eq!(state.absolute, vec![(ABS_X, 0), (ABS_Y, 300)]);
    state.apply(&Event::new(0, EV_LED, LED_CAPSL, 0));
    assert_eq!(state.leds, 0);
}

#[test]
fn groups_end_with_syn_report() {
    let g = group(9, [key(KEY_A, 1)]);
    assert_eq!(g, vec![key(KEY_A, 1), Event::new(9, EV_SYN, SYN_REPORT, 0)]);
}

#[test]
fn a_lost_group_is_followed_by_syn_dropped() {
    let mut stream = Stream::new();
    let mut sent: Vec<Vec<Event>> = Vec::new();
    let first = group(1, [key(KEY_A, 1)]);
    let second = group(2, [key(KEY_A, 0)]);
    let third = group(3, [key(KEY_B, 1)]);
    stream.send(&first, |g| {
        sent.push(g.to_vec());
        Ok::<_, ()>(())
    });
    // The reader's queue is full: the group is lost.
    stream.send(&second, |_| Err(()));
    assert!(stream.dropped());
    // Still full: neither SYN_DROPPED nor the group goes out.
    stream.send(&second, |_| Err(()));
    stream.send(&third, |g| {
        sent.push(g.to_vec());
        Ok::<_, ()>(())
    });
    assert!(!stream.dropped());
    let dropped = Event::new(3, EV_SYN, SYN_DROPPED, 0);
    assert_eq!(sent, vec![first, vec![dropped], third]);
}

#[test]
fn a_group_is_lost_when_syn_dropped_went_out_but_it_did_not() {
    let mut stream = Stream::new();
    stream.send(&group(1, [key(KEY_A, 1)]), |_| Err(()));
    let mut calls = 0;
    stream.send(&group(2, [key(KEY_A, 0)]), |_| {
        calls += 1;
        if calls == 1 { Ok(()) } else { Err(()) }
    });
    assert_eq!(calls, 2);
    assert!(stream.dropped());
}

#[test]
fn empty_groups_send_nothing() {
    let mut stream = Stream::new();
    stream.send(&[], |_| -> Result<(), ()> { panic!("sent") });
}
