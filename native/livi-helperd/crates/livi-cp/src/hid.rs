use crate::bplist::{Value, dict};

pub const TOUCH_HID_UID: u32 = 0x2a2a_2a2a;
pub const KNOB_HID_UID: u32 = 0x2a2a_2a2b;
pub const MEDIA_HID_UID: u32 = 0x2a2a_2a2c;
pub const TELEPHONY_HID_UID: u32 = 0x2a2a_2a2d;

/// Two contacts cover pinch and rotate.
pub const TOUCH_CONTACTS: usize = 2;
/// Index, touch bit plus padding, X and Y.
const BYTES_PER_FINGER: usize = 6;

fn finger_collection(x_max: u16, y_max: u16) -> Vec<u8> {
    let [x_lo, x_hi] = x_max.to_le_bytes();
    let [y_lo, y_hi] = y_max.to_le_bytes();
    vec![
        0x05, 0x0d, // Usage Page (Digitizers)
        0x09, 0x22, // Usage (Finger)
        0xa1, 0x02, // Collection (Logical)
        0x09, 0x38, // Usage (Transducer Index)
        0x75, 0x08, // Report Size (8)
        0x95, 0x01, // Report Count (1)
        0x81, 0x02, // Input (Data,Var,Abs)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x09, 0x33, // Usage (Touch)
        0x75, 0x01, // Report Size (1)
        0x95, 0x01, // Report Count (1)
        0x81, 0x02, // Input (Data,Var,Abs)
        0x95, 0x07, // Report Count (7)
        0x81, 0x03, // Input (Cnst,Var,Abs), padding
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x26, x_lo, x_hi, // Logical Maximum (x_max)
        0x09, 0x30, // Usage (X)
        0x75, 0x10, // Report Size (16)
        0x95, 0x01, // Report Count (1)
        0x81, 0x02, // Input (Data,Var,Abs)
        0x26, y_lo, y_hi, // Logical Maximum (y_max)
        0x09, 0x31, // Usage (Y)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ]
}

fn multitouch_descriptor(x_max: u16, y_max: u16) -> Vec<u8> {
    let mut out = vec![
        0x05, 0x0d, // Usage Page (Digitizers)
        0x09, 0x04, // Usage (Touch Screen)
        0xa1, 0x01, // Collection (Application)
    ];
    for _ in 0..TOUCH_CONTACTS {
        out.extend(finger_collection(x_max, y_max));
    }
    out.push(0xc0);
    out
}

const KNOB_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x08, // Usage (MultiAxisController)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x09, // Usage Page (Button)
    0x09, 0x01, // Usage (Button 1)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x01, // Logical Maximum (1)
    0x75, 0x01, // Report Size (1)
    0x95, 0x01, // Report Count (1)
    0x81, 0x02, // Input (Data,Var,Abs), bit 0 Select
    0x05, 0x0c, // Usage Page (Consumer)
    0x0a, 0x23, 0x02, // Usage (AC Home)
    0x0a, 0x24, 0x02, // Usage (AC Back)
    0x95, 0x02, // Report Count (2)
    0x81, 0x02, // Input (Data,Var,Abs), bit 1 Home, bit 2 Back
    0x95, 0x05, // Report Count (5)
    0x81, 0x01, // Input (Constant), bits 3 to 7
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x01, // Usage (Pointer)
    0xa1, 0x00, // Collection (Physical)
    0x09, 0x30, // Usage (X)
    0x09, 0x31, // Usage (Y)
    0x15, 0x81, // Logical Minimum (-127)
    0x25, 0x7f, // Logical Maximum (127)
    0x75, 0x08, // Report Size (8)
    0x95, 0x02, // Report Count (2)
    0x81, 0x02, // Input (Data,Var,Abs), X and Y
    0xc0, // End Collection
    0x09, 0x38, // Usage (Wheel)
    0x15, 0x81, // Logical Minimum (-127)
    0x25, 0x7f, // Logical Maximum (127)
    0x75, 0x08, // Report Size (8)
    0x95, 0x01, // Report Count (1)
    0x81, 0x06, // Input (Data,Var,Rel), Wheel
    0xc0, // End Collection
];

const MEDIA_DESCRIPTOR: &[u8] = &[
    0x05, 0x0c, // Usage Page (Consumer)
    0x09, 0x01, // Usage (Consumer Control)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x06, // Logical Maximum (6)
    0x05, 0x0c, // Usage Page (Consumer)
    0x0a, 0x00, 0x00, // Usage (Unassigned), index 0
    0x0a, 0xb0, 0x00, // Usage (Play), 1
    0x0a, 0xb1, 0x00, // Usage (Pause), 2
    0x0a, 0xcd, 0x00, // Usage (Play/Pause), 3
    0x0a, 0xb5, 0x00, // Usage (Scan Next Track), 4
    0x0a, 0xb6, 0x00, // Usage (Scan Previous Track), 5
    0x0a, 0x9e, 0x02, // Usage (AC Navigation Guidance), 6
    0x75, 0x08, // Report Size (8)
    0x95, 0x01, // Report Count (1)
    0x81, 0x00, // Input (Data,Array,Abs)
    0xc0, // End Collection
];

const TELEPHONY_DESCRIPTOR: &[u8] = &[
    0x05, 0x0b, // Usage Page (Telephony)
    0x09, 0x07, // Usage (Telephony Keypad)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x11, // Logical Maximum (17)
    0x05, 0x0b, // Usage Page (Telephony)
    0x09, 0x00, // Usage (Unassigned), index 0
    0x09, 0x20, // Usage (Hook Switch), 1
    0x09, 0x21, // Usage (Flash), 2
    0x09, 0x26, // Usage (Drop), 3
    0x09, 0x2f, // Usage (Mute), 4
    0x09, 0xb0, // Usage (Phone Key 0), 5
    0x09, 0xb1, // Usage (Phone Key 1), 6
    0x09, 0xb2, // Usage (Phone Key 2), 7
    0x09, 0xb3, // Usage (Phone Key 3), 8
    0x09, 0xb4, // Usage (Phone Key 4), 9
    0x09, 0xb5, // Usage (Phone Key 5), 10
    0x09, 0xb6, // Usage (Phone Key 6), 11
    0x09, 0xb7, // Usage (Phone Key 7), 12
    0x09, 0xb8, // Usage (Phone Key 8), 13
    0x09, 0xb9, // Usage (Phone Key 9), 14
    0x09, 0xba, // Usage (Phone Key Star), 15
    0x09, 0xbb, // Usage (Phone Key Pound), 16
    0x05, 0x07, // Usage Page (Keyboard/Keypad)
    0x09, 0x2a, // Usage (Keyboard DELETE), 17
    0x75, 0x08, // Report Size (8)
    0x95, 0x01, // Report Count (1)
    0x81, 0x00, // Input (Data,Array,Abs)
    0xc0, // End Collection
];

fn device_entry(uid: u32, name: &str, descriptor: Vec<u8>, display_uuid: &str) -> Value {
    dict([
        ("hidProductID", Value::Int(1)),
        ("hidVendorID", Value::Int(2)),
        ("hidCountryCode", Value::Int(0)),
        ("uuid", Value::String(format!("{uid:x}"))),
        ("name", Value::String(name.into())),
        ("displayUUID", Value::String(display_uuid.into())),
        ("hidDescriptor", Value::Data(descriptor)),
    ])
}

pub fn touch_device(x_max: u16, y_max: u16, display_uuid: &str) -> Value {
    device_entry(
        TOUCH_HID_UID,
        "LIVI Touchscreen",
        multitouch_descriptor(x_max, y_max),
        display_uuid,
    )
}

pub fn knob_device(display_uuid: &str) -> Value {
    device_entry(KNOB_HID_UID, "LIVI Knob", KNOB_DESCRIPTOR.to_vec(), display_uuid)
}

pub fn media_device(display_uuid: &str) -> Value {
    device_entry(MEDIA_HID_UID, "LIVI Media", MEDIA_DESCRIPTOR.to_vec(), display_uuid)
}

pub fn telephony_device(display_uuid: &str) -> Value {
    device_entry(TELEPHONY_HID_UID, "LIVI Telephony", TELEPHONY_DESCRIPTOR.to_vec(), display_uuid)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// Position in display pixels.
    pub x: f64,
    pub y: f64,
    pub down: bool,
}

/// A slot always carries its own number as transducer index, the phone
/// follows a finger by it.
pub fn touch_report(contacts: &[Contact]) -> [u8; BYTES_PER_FINGER * TOUCH_CONTACTS] {
    let mut r = [0u8; BYTES_PER_FINGER * TOUCH_CONTACTS];
    for i in 0..TOUCH_CONTACTS {
        let off = i * BYTES_PER_FINGER;
        r[off] = i as u8;
        let Some(c) = contacts.get(i) else { continue };
        r[off + 1] = u8::from(c.down);
        r[off + 2..off + 4].copy_from_slice(&axis(c.x).to_le_bytes());
        r[off + 4..off + 6].copy_from_slice(&axis(c.y).to_le_bytes());
    }
    r
}

/// Halves round up, also below zero.
pub(crate) fn round_half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

fn axis(v: f64) -> u16 {
    round_half_up(v).clamp(0.0, f64::from(u16::MAX)) as u16
}

pub mod media_button {
    pub const NONE: u8 = 0;
    pub const PLAY: u8 = 1;
    pub const PAUSE: u8 = 2;
    pub const PLAY_PAUSE: u8 = 3;
    pub const NEXT: u8 = 4;
    pub const PREV: u8 = 5;
    pub const NAV_GUIDANCE: u8 = 6;
}

pub mod telephony_button {
    pub const NONE: u8 = 0;
    pub const HOOK_SWITCH: u8 = 1;
    pub const FLASH: u8 = 2;
    pub const DROP: u8 = 3;
    pub const MUTE: u8 = 4;
    /// Keys 0 to 9 follow on.
    pub const KEY0: u8 = 5;
    pub const STAR: u8 = 15;
    pub const POUND: u8 = 16;
    pub const DELETE: u8 = 17;
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KnobState {
    pub select: bool,
    pub home: bool,
    pub back: bool,
    pub x: f64,
    pub y: f64,
    pub wheel: f64,
}

fn knob_axis(v: f64) -> u8 {
    (round_half_up(v).clamp(-127.0, 127.0) as i8) as u8
}

pub fn knob_report(s: KnobState) -> [u8; 4] {
    let buttons = u8::from(s.select) | (u8::from(s.home) << 1) | (u8::from(s.back) << 2);
    [buttons, knob_axis(s.x), knob_axis(s.y), knob_axis(s.wheel)]
}

pub fn media_report(index: u8) -> [u8; 1] {
    [index]
}

pub fn telephony_report(index: u8) -> [u8; 1] {
    [index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_descriptor_carries_the_display_size() {
        let Some(Value::Data(desc)) = touch_device(1280, 720, "main").get("hidDescriptor").cloned()
        else {
            panic!("no descriptor");
        };
        assert_eq!(desc.len(), 6 + 2 * 51 + 1);
        assert_eq!(&desc[6 + 32..6 + 35], &[0x26, 0x00, 0x05]);
        assert_eq!(&desc[6 + 43..6 + 46], &[0x26, 0xd0, 0x02]);
    }

    #[test]
    fn touch_report_fills_fixed_slots() {
        let r = touch_report(&[Contact { x: 100.4, y: 50.6, down: true }]);
        assert_eq!(r, [0, 1, 100, 0, 51, 0, 1, 0, 0, 0, 0, 0]);
        let r = touch_report(&[
            Contact { x: -3.0, y: 70000.0, down: false },
            Contact { x: 300.0, y: 2.0, down: true },
        ]);
        assert_eq!(r, [0, 0, 0, 0, 0xff, 0xff, 1, 1, 0x2c, 1, 2, 0]);
    }

    #[test]
    fn knob_report_packs_buttons_and_clamps_axes() {
        let r = knob_report(KnobState {
            home: true,
            back: true,
            x: -300.0,
            y: 5.4,
            wheel: 1.0,
            ..Default::default()
        });
        assert_eq!(r, [0b110, 0x81, 5, 1]);
    }

    #[test]
    fn devices_name_their_uid_in_hex() {
        assert_eq!(knob_device("m").get("uuid").and_then(Value::as_str), Some("2a2a2a2b"));
        assert_eq!(media_report(media_button::NEXT), [4]);
        assert_eq!(telephony_report(telephony_button::KEY0 + 9), [14]);
    }
}
