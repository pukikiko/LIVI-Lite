//! The input channel (8), head unit to phone. Reports are stamped in microseconds.

use crate::channels::Frame;
use crate::consts::{ch, frame_flags};
use crate::wire::{field_len_delim, field_varint};

pub mod input_msg {
    pub const INPUT_REPORT: u16 = 0x8001;
    /// Phone to head unit.
    pub const KEY_BINDING_REQUEST: u16 = 0x8002;
    pub const KEY_BINDING_RESPONSE: u16 = 0x8003;
    pub const INPUT_FEEDBACK: u16 = 0x8004;
}

pub mod touch_action {
    pub const DOWN: u32 = 0;
    pub const UP: u32 = 1;
    pub const MOVED: u32 = 2;
    pub const POINTER_DOWN: u32 = 5;
    pub const POINTER_UP: u32 = 6;
}

/// Android key codes plus the protocol's own. A key only reaches the phone
/// when the service discovery advertised it.
pub mod button_key {
    pub const UNKNOWN: u32 = 0;
    pub const HOME: u32 = 3;
    pub const BACK: u32 = 4;
    pub const PHONE_ACCEPT: u32 = 5;
    pub const PHONE_DECLINE: u32 = 6;
    pub const KEY_0: u32 = 7;
    pub const KEY_1: u32 = 8;
    pub const KEY_2: u32 = 9;
    pub const KEY_3: u32 = 10;
    pub const KEY_4: u32 = 11;
    pub const KEY_5: u32 = 12;
    pub const KEY_6: u32 = 13;
    pub const KEY_7: u32 = 14;
    pub const KEY_8: u32 = 15;
    pub const KEY_9: u32 = 16;
    pub const KEY_STAR: u32 = 17;
    pub const KEY_POUND: u32 = 18;
    pub const DPAD_UP: u32 = 19;
    pub const DPAD_DOWN: u32 = 20;
    pub const DPAD_LEFT: u32 = 21;
    pub const DPAD_RIGHT: u32 = 22;
    pub const DPAD_CENTER: u32 = 23;
    /// Advertised for setups where the phone plays itself and the user binds
    /// hardware volume keys to it.
    pub const VOLUME_UP: u32 = 24;
    pub const VOLUME_DOWN: u32 = 25;
    pub const POWER: u32 = 26;
    pub const ENTER: u32 = 66;
    pub const HEADSETHOOK: u32 = 79;
    pub const MENU: u32 = 82;
    pub const SEARCH: u32 = 84;
    pub const MEDIA_PLAY_PAUSE: u32 = 85;
    pub const MEDIA_STOP: u32 = 86;
    pub const MEDIA_NEXT: u32 = 87;
    pub const MEDIA_PREV: u32 = 88;
    pub const MEDIA_REWIND: u32 = 89;
    pub const MEDIA_FAST_FWD: u32 = 90;
    pub const MUTE: u32 = 91;
    pub const ESCAPE: u32 = 111;
    pub const MEDIA_PLAY: u32 = 126;
    pub const MEDIA_PAUSE: u32 = 127;
    pub const VOLUME_MUTE: u32 = 164;
    pub const ASSIST: u32 = 219;
    pub const VOICE_ASSIST: u32 = 231;
    pub const NAVIGATE_PREVIOUS: u32 = 260;
    pub const NAVIGATE_NEXT: u32 = 261;
    pub const NAVIGATE_IN: u32 = 262;
    pub const NAVIGATE_OUT: u32 = 263;
    pub const ROTARY_CONTROLLER: u32 = 65536;
    pub const MEDIA: u32 = 65537;
    pub const TERTIARY_BUTTON: u32 = 65543;
    pub const TURN_CARD: u32 = 65544;
}

/// In the advertised touchscreen's pixels, the id stable from down to up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPointer {
    pub x: i64,
    pub y: i64,
    pub id: i64,
}

fn report(ts_micros: u64, field: u32, event: &[u8]) -> Frame {
    let msg = [field_varint(1, ts_micros as i64), field_len_delim(field, event)].concat();
    Frame::new(ch::INPUT, frame_flags::ENC_SIGNAL, input_msg::INPUT_REPORT, msg)
}

/// `action_index` names the pointer behind a pointer-down or pointer-up.
pub fn touch(
    ts_micros: u64,
    action: u32,
    pointers: &[TouchPointer],
    action_index: u32,
) -> Option<Frame> {
    if pointers.is_empty() {
        return None;
    }
    let mut event: Vec<u8> = pointers
        .iter()
        .flat_map(|p| {
            field_len_delim(
                1,
                &[field_varint(1, p.x), field_varint(2, p.y), field_varint(3, p.id)].concat(),
            )
        })
        .collect();
    event.extend(field_varint(2, i64::from(action_index)));
    event.extend(field_varint(3, i64::from(action)));
    Some(report(ts_micros, 3, &event))
}

/// `direction` is -1 back and 1 on.
pub fn rotary(ts_micros: u64, direction: i64) -> Frame {
    let rel =
        [field_varint(1, i64::from(button_key::ROTARY_CONTROLLER)), field_varint(2, direction)]
            .concat();
    report(ts_micros, 6, &field_len_delim(1, &rel))
}

/// Several codes ride one event, the phone picks whichever it understands where
/// the focus is. Every key field is written, some phones want longpress even when false.
pub fn button(ts_micros: u64, codes: &[u32], down: bool, longpress: bool) -> Option<Frame> {
    if codes.is_empty() {
        return None;
    }
    let keys: Vec<u8> = codes
        .iter()
        .flat_map(|code| {
            let key = [
                field_varint(1, i64::from(*code)),
                field_varint(2, i64::from(down)),
                field_varint(3, 0),
                field_varint(4, i64::from(longpress)),
            ]
            .concat();
            field_len_delim(1, &key)
        })
        .collect();
    Some(report(ts_micros, 4, &keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_carry_the_timestamp_and_the_event() {
        let f =
            touch(1000, touch_action::DOWN, &[TouchPointer { x: 10, y: 20, id: 0 }], 0).unwrap();
        assert_eq!(f.ch, ch::INPUT);
        assert_eq!(f.msg_id, input_msg::INPUT_REPORT);
        assert_eq!(
            f.payload,
            [
                0x08, 0xe8, 0x07, 0x1a, 0x0c, 0x0a, 0x06, 0x08, 0x0a, 0x10, 0x14, 0x18, 0x00, 0x10,
                0x00, 0x18, 0x00
            ]
        );
        assert!(touch(1, 0, &[], 0).is_none());
        assert!(button(1, &[], true, false).is_none());
        let b = button(0, &[button_key::HOME], true, false).unwrap();
        assert_eq!(
            b.payload,
            [0x08, 0x00, 0x22, 0x0a, 0x0a, 0x08, 0x08, 0x03, 0x10, 0x01, 0x18, 0x00, 0x20, 0x00]
        );
        let mut rel = vec![0x08, 0x80, 0x80, 0x04, 0x10];
        rel.extend([0xff; 9]);
        rel.push(0x01);
        let mut expected = vec![0x08, 0x00, 0x32, 0x11, 0x0a, 0x0f];
        expected.extend(rel);
        assert_eq!(rotary(0, -1).payload, expected);
    }
}
