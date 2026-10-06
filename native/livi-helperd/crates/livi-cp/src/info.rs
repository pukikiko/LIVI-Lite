//! The phone only asks for the screen when displays, audio formats and
//! latencies, the resource modes and the HID devices are all there.

use crate::bplist::{Value, dict};
use crate::hid;

const STREAM_TYPE_MAIN_SCREEN: u64 = 110;
const STREAM_TYPE_ALT_SCREEN: u64 = 111;
const DISPLAY_FEATURE_KNOBS: u64 = 0x02;
const DISPLAY_FEATURE_HIGH_FIDELITY_TOUCH: u64 = 0x08;
const PRIMARY_INPUT_KNOBS: u64 = 3;

const CARPLAY_FEATURES: u64 = 0x61_5653_aee2;
/// The audio bits, cleared to offer neither audio source nor sink.
const CARPLAY_AUDIO_FEATURES: u64 = 0x100_0454_0a00;

pub const MAIN_UUID: &str = "b7e6c5a0-1111-4000-8000-000000000001";
pub const ALT_UUID: &str = "b7e6c5a0-2222-4000-8000-000000000002";

const RESOURCE_SCREEN: u64 = 1;
const RESOURCE_AUDIO: u64 = 2;
const TRANSFER_TAKE: u64 = 1;
const PRIORITY_NICE_TO_HAVE: u64 = 100;
const CONSTRAINT_ANYTIME: u64 = 100;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Insets {
    pub top: i64,
    pub bottom: i64,
    pub left: i64,
    pub right: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayConfig {
    pub width_pixels: i64,
    pub height_pixels: i64,
    pub width_physical_mm: Option<i64>,
    pub height_physical_mm: Option<i64>,
    pub fps: Option<i64>,
    pub primary_input_device: Option<i64>,
    pub view_area: Option<Insets>,
    pub safe_area: Option<Insets>,
    pub safe_area_draw_outside: Option<bool>,
    /// The CarPlay URL an alternate screen opens with.
    pub initial_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub width_pixels: u64,
    pub height_pixels: u64,
    pub png: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InfoConfig {
    pub device_name: String,
    pub device_id: String,
    pub bt_mac: String,
    pub source_version: String,
    pub hevc: bool,
    pub main: DisplayConfig,
    pub cluster: Option<DisplayConfig>,
    /// 44100 or 48000, for the entertainment stream.
    pub entertainment_sample_rate: u32,
    /// No audio is offered, the phone plays it itself.
    pub disable_audio_output: bool,
    pub oem_label: String,
    pub icons: Vec<Icon>,
    pub right_hand_drive: bool,
}

fn num(v: i64) -> Value {
    u64::try_from(v).map(Value::Int).unwrap_or(Value::Real(v as f64))
}

fn resource(id: u64) -> Value {
    dict([
        ("resourceID", Value::Int(id)),
        ("transferType", Value::Int(TRANSFER_TAKE)),
        ("transferPriority", Value::Int(PRIORITY_NICE_TO_HAVE)),
        ("takeConstraint", Value::Int(CONSTRAINT_ANYTIME)),
        ("borrowConstraint", Value::Int(CONSTRAINT_ANYTIME)),
        ("unborrowConstraint", Value::Int(CONSTRAINT_ANYTIME)),
    ])
}

fn modes() -> Value {
    dict([
        ("resources", Value::Array(vec![resource(RESOURCE_SCREEN), resource(RESOURCE_AUDIO)])),
        (
            "appStates",
            Value::Array(vec![
                dict([("appStateID", Value::Int(2)), ("state", Value::Bool(false))]),
                dict([("appStateID", Value::Int(1)), ("speechMode", num(-1))]),
                dict([("appStateID", Value::Int(3)), ("state", Value::Bool(false))]),
            ]),
        ),
    ])
}

/// Informational only, the phone keeps its own buffering latency.
fn audio_latencies() -> Value {
    let base = |kind: u64, audio_type: Option<&str>| {
        let mut entry = vec![
            ("type".to_string(), Value::Int(kind)),
            ("inputLatencyMicros".to_string(), Value::Int(0)),
            ("outputLatencyMicros".to_string(), Value::Int(0)),
        ];
        if let Some(t) = audio_type {
            entry.push(("audioType".to_string(), Value::String(t.into())));
        }
        Value::Dict(entry)
    };
    Value::Array(vec![
        base(100, None),
        base(100, Some("default")),
        base(100, Some("media")),
        base(100, Some("telephony")),
        base(100, Some("speechRecognition")),
        base(100, Some("alert")),
        base(101, None),
        base(101, Some("default")),
        base(102, Some("default")),
    ])
}

/// PCM only exists over USB, so over Wi-Fi the phone takes OPUS for the
/// low-latency streams and AAC-LC for entertainment.
fn audio_formats(entertainment_rate: u32) -> Value {
    let f = |kind: u64, audio_type: &str, output: u64, input: Option<u64>| {
        let mut entry = vec![
            ("type".to_string(), Value::Int(kind)),
            ("audioType".to_string(), Value::String(audio_type.into())),
            ("audioOutputFormats".to_string(), Value::Int(output)),
        ];
        if let Some(input) = input {
            entry.push(("audioInputFormats".to_string(), Value::Int(input)));
        }
        Value::Dict(entry)
    };
    let is48 = entertainment_rate == 48000;
    let pcm_voice = 0x3fc;
    let pcm = pcm_voice | if is48 { 0xc000 } else { 0xc00 };
    let pcm_mono = 0x154 | if is48 { 0x4000 } else { 0x400 };
    let pcm_media = if is48 { 0x8000 } else { 0x800 };
    let opus = 0x7000_0000;
    let aac_lc = if is48 { 0x80_0000 } else { 0x40_0000 };
    Value::Array(vec![
        f(100, "compatibility", pcm, Some(pcm_mono)),
        f(101, "compatibility", pcm, None),
        f(100, "default", pcm | opus, Some(pcm_mono | opus)),
        f(100, "alert", pcm | opus, None),
        f(100, "media", pcm_media, None),
        f(100, "telephony", pcm_mono | opus, Some(pcm_mono | opus)),
        f(100, "speechRecognition", pcm_mono | opus, Some(pcm_mono | opus)),
        f(101, "default", pcm | opus, None),
        f(102, "media", aac_lc, None),
    ])
}

fn area(d: &DisplayConfig) -> Option<Value> {
    let va = d.view_area?;
    let (w, h) = (d.width_pixels, d.height_pixels);
    let mut view = vec![
        ("widthPixels".to_string(), num(w - va.left - va.right)),
        ("heightPixels".to_string(), num(h - va.top - va.bottom)),
        ("originXPixels".to_string(), num(va.left)),
        ("originYPixels".to_string(), num(va.top)),
    ];
    if let Some(sa) = d.safe_area {
        let mut safe = vec![
            ("widthPixels".to_string(), num(w - sa.left - sa.right)),
            ("heightPixels".to_string(), num(h - sa.top - sa.bottom)),
            ("originXPixels".to_string(), num(sa.left)),
            ("originYPixels".to_string(), num(sa.top)),
        ];
        if let Some(outside) = d.safe_area_draw_outside {
            safe.push(("drawUIOutsideSafeArea".to_string(), Value::Bool(outside)));
        }
        view.push(("safeArea".to_string(), Value::Dict(safe)));
    }
    Some(Value::Dict(view))
}

fn display(d: &DisplayConfig, kind: u64, uuid: &str) -> Value {
    let width_physical = d.width_physical_mm.unwrap_or(200);
    let height_physical = d.height_physical_mm.unwrap_or_else(|| {
        let h = width_physical as f64 * d.height_pixels as f64 / d.width_pixels.max(1) as f64;
        (hid::round_half_up(h) as i64).max(1)
    });
    let mut entry = vec![
        ("uuid".to_string(), Value::String(uuid.into())),
        ("type".to_string(), Value::Int(kind)),
        ("maxFPS".to_string(), num(d.fps.unwrap_or(60))),
        ("widthPixels".to_string(), num(d.width_pixels)),
        ("heightPixels".to_string(), num(d.height_pixels)),
        ("widthPhysical".to_string(), num(width_physical)),
        ("heightPhysical".to_string(), num(height_physical)),
        (
            "features".to_string(),
            Value::Int(DISPLAY_FEATURE_HIGH_FIDELITY_TOUCH | DISPLAY_FEATURE_KNOBS),
        ),
        (
            "primaryInputDevice".to_string(),
            num(d.primary_input_device.unwrap_or(PRIMARY_INPUT_KNOBS as i64)),
        ),
    ];
    if let Some(view) = area(d) {
        entry.push(("viewAreas".to_string(), Value::Array(vec![view])));
        entry.push(("initialViewArea".to_string(), Value::Int(0)));
    }
    if let Some(url) = d.initial_url.as_deref().filter(|u| !u.is_empty()) {
        entry.push(("initialURL".to_string(), Value::String(url.into())));
    }
    Value::Dict(entry)
}

fn hid_axis(v: i64) -> u16 {
    (v & 0xffff) as u16
}

pub fn build(cfg: &InfoConfig) -> Value {
    let mut displays = vec![display(&cfg.main, STREAM_TYPE_MAIN_SCREEN, MAIN_UUID)];
    if let Some(cluster) = &cfg.cluster {
        displays.push(display(cluster, STREAM_TYPE_ALT_SCREEN, ALT_UUID));
    }
    let features = if cfg.disable_audio_output {
        CARPLAY_FEATURES & !CARPLAY_AUDIO_FEATURES
    } else {
        CARPLAY_FEATURES
    };

    let mut info = vec![
        ("sourceVersion".to_string(), Value::String(cfg.source_version.clone())),
        ("features".to_string(), Value::Int(features)),
        ("statusFlags".to_string(), Value::Int(4)),
        ("model".to_string(), Value::String("LIVI".into())),
        ("manufacturer".to_string(), Value::String("LIVI".into())),
        ("deviceID".to_string(), Value::String(cfg.device_id.clone())),
        ("bluetoothIDs".to_string(), Value::Array(vec![Value::String(cfg.bt_mac.clone())])),
        ("name".to_string(), Value::String(cfg.device_name.clone())),
        ("rightHandDrive".to_string(), Value::Bool(cfg.right_hand_drive)),
        ("keepAliveLowPower".to_string(), Value::Bool(false)),
        ("keepAliveSendStatsAsBody".to_string(), Value::Bool(false)),
        ("modes".to_string(), modes()),
    ];
    if !cfg.disable_audio_output {
        info.push(("audioLatencies".to_string(), audio_latencies()));
        info.push(("audioFormats".to_string(), audio_formats(cfg.entertainment_sample_rate)));
    }
    info.push((
        "extendedFeatures".to_string(),
        Value::Array(vec![
            Value::String("vocoderInfo".into()),
            Value::String("enhancedRequestCarUI".into()),
        ]),
    ));
    info.push(("displays".to_string(), Value::Array(displays)));
    info.push((
        "hidDevices".to_string(),
        Value::Array(vec![
            hid::touch_device(
                hid_axis(cfg.main.width_pixels),
                hid_axis(cfg.main.height_pixels),
                MAIN_UUID,
            ),
            hid::knob_device(MAIN_UUID),
            hid::media_device(MAIN_UUID),
            hid::telephony_device(MAIN_UUID),
        ]),
    ));
    if !cfg.icons.is_empty() {
        info.push(("oemIconVisible".to_string(), Value::Bool(true)));
        info.push(("oemIconLabel".to_string(), Value::String(cfg.oem_label.clone())));
        let icons = cfg
            .icons
            .iter()
            .map(|icon| {
                dict([
                    ("imageData", Value::Data(icon.png.clone())),
                    ("widthPixels", Value::Int(icon.width_pixels)),
                    ("heightPixels", Value::Int(icon.height_pixels)),
                    ("prerendered", Value::Bool(true)),
                ])
            })
            .collect();
        info.push(("oemIcons".to_string(), Value::Array(icons)));
    }
    if cfg.hevc {
        info.push(("hevcInfo".to_string(), Value::Dict(Vec::new())));
    }
    Value::Dict(info)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sample() -> InfoConfig {
        InfoConfig {
            device_name: "LIVI".into(),
            device_id: "AA:BB:CC:DD:EE:FF".into(),
            bt_mac: "11:22:33:44:55:66".into(),
            source_version: "950.7.1".into(),
            hevc: true,
            main: DisplayConfig {
                width_pixels: 1280,
                height_pixels: 720,
                fps: Some(60),
                view_area: Some(Insets::default()),
                safe_area: Some(Insets { top: 0, bottom: 0, left: 10, right: 10 }),
                safe_area_draw_outside: Some(true),
                ..Default::default()
            },
            cluster: None,
            entertainment_sample_rate: 48000,
            disable_audio_output: false,
            oem_label: "App".into(),
            icons: Vec::new(),
            right_hand_drive: false,
        }
    }

    #[test]
    fn main_display_falls_back_to_200_mm_wide() {
        let info = build(&sample());
        let main = &info.get("displays").and_then(Value::as_array).unwrap()[0];
        assert_eq!(main.get("widthPhysical"), Some(&Value::Int(200)));
        assert_eq!(main.get("heightPhysical"), Some(&Value::Int(113)));
        assert_eq!(main.get("type"), Some(&Value::Int(110)));
    }

    #[test]
    fn no_audio_drops_formats_and_audio_features() {
        let mut cfg = sample();
        cfg.disable_audio_output = true;
        let info = build(&cfg);
        assert!(info.get("audioFormats").is_none());
        assert_eq!(
            info.get("features"),
            Some(&Value::Int(CARPLAY_FEATURES & !CARPLAY_AUDIO_FEATURES))
        );
    }

    #[test]
    fn speech_mode_minus_one_goes_out_as_a_real() {
        let info = build(&sample());
        let states =
            info.get("modes").and_then(|m| m.get("appStates")).and_then(Value::as_array).unwrap();
        assert_eq!(states[1].get("speechMode"), Some(&Value::Real(-1.0)));
    }
}
