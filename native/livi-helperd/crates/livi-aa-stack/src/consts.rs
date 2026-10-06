/// Byte 1 of the frame header.
pub mod frame_flags {
    pub const PLAINTEXT: u8 = 0x03;
    pub const ENC_SIGNAL: u8 = 0x0b;
    pub const ENC_CONTROL: u8 = 0x0f;
    pub const ENC_FIRST_FRAG: u8 = 0x08;
    pub const ENC_CONT_FRAG: u8 = 0x0a;
    pub const ENCRYPTED: u8 = 0x08;
}

pub mod ctrl_msg {
    pub const VERSION_REQUEST: u16 = 0x0001;
    pub const VERSION_RESPONSE: u16 = 0x0002;
    pub const SSL_HANDSHAKE: u16 = 0x0003;
    pub const AUTH_COMPLETE: u16 = 0x0004;
    /// Phone to head unit, despite the name.
    pub const SERVICE_DISCOVERY_REQUEST: u16 = 0x0005;
    pub const SERVICE_DISCOVERY_RESPONSE: u16 = 0x0006;
    /// The phone opens every channel.
    pub const CHANNEL_OPEN_REQUEST: u16 = 0x0007;
    pub const CHANNEL_OPEN_RESPONSE: u16 = 0x0008;
    pub const CHANNEL_CLOSE_NOTIFICATION: u16 = 0x0009;
    pub const PING_REQUEST: u16 = 0x000b;
    pub const PING_RESPONSE: u16 = 0x000c;
    pub const NAVIGATION_FOCUS_REQUEST: u16 = 0x000d;
    pub const NAVIGATION_FOCUS_RESPONSE: u16 = 0x000e;
    pub const SHUTDOWN_REQUEST: u16 = 0x000f;
    pub const SHUTDOWN_RESPONSE: u16 = 0x0010;
    /// Phone to head unit, 1 start and 2 end.
    pub const VOICE_SESSION_NOTIFICATION: u16 = 0x0011;
    pub const AUDIO_FOCUS_REQUEST: u16 = 0x0012;
    pub const AUDIO_FOCUS_RESPONSE: u16 = 0x0013;
    pub const BATTERY_STATUS_NOTIFICATION: u16 = 0x0017;
    pub const BINDING_REQUEST: u16 = 0x0019;
    pub const BINDING_RESPONSE: u16 = 0x001a;
}

pub mod av_msg {
    pub const AV_MEDIA_WITH_TIMESTAMP: u16 = 0x0000;
    pub const AV_MEDIA_INDICATION: u16 = 0x0001;
    pub const SETUP_REQUEST: u16 = 0x8000;
    pub const START_INDICATION: u16 = 0x8001;
    pub const STOP_INDICATION: u16 = 0x8002;
    pub const SETUP_RESPONSE: u16 = 0x8003;
    pub const AV_MEDIA_ACK: u16 = 0x8004;
    pub const AV_INPUT_OPEN_REQUEST: u16 = 0x8005;
    pub const AV_INPUT_OPEN_RESPONSE: u16 = 0x8006;
    pub const VIDEO_FOCUS_REQUEST: u16 = 0x8007;
    pub const VIDEO_FOCUS_INDICATION: u16 = 0x8008;
}

pub mod ch {
    pub const CONTROL: u8 = 0;
    pub const SENSOR: u8 = 1;
    pub const VIDEO: u8 = 3;
    pub const MEDIA_AUDIO: u8 = 4;
    pub const SPEECH_AUDIO: u8 = 5;
    pub const SYSTEM_AUDIO: u8 = 6;
    pub const INPUT: u8 = 8;
    pub const MIC_INPUT: u8 = 9;
    pub const BLUETOOTH: u8 = 10;
    pub const NAVIGATION: u8 = 12;
    pub const MEDIA_INFO: u8 = 13;
    pub const PHONE_STATUS: u8 = 14;
    pub const WIFI: u8 = 18;
    pub const CLUSTER_VIDEO: u8 = 19;
    /// The secondary display's input, not interactive.
    pub const CLUSTER_INPUT: u8 = 20;
}

pub mod version {
    pub const MAJOR: u16 = 1;
    pub const MINOR: u16 = 7;
    pub const STATUS_MATCH: u16 = 0x0000;
    pub const STATUS_MISMATCH: u16 = 0xffff;
}

pub const STATUS_OK: i32 = 0;

pub mod video_resolution {
    pub const R800X480: i32 = 1;
    pub const R1280X720: i32 = 2;
    pub const R1920X1080: i32 = 3;
}

pub mod video_fps {
    pub const FPS60: i32 = 1;
    pub const FPS30: i32 = 2;
}

pub mod media_codec {
    pub const AUDIO_PCM: i32 = 1;
    pub const AUDIO_AAC_LC: i32 = 2;
    pub const VIDEO_H264_BP: i32 = 3;
    pub const VIDEO_VP9: i32 = 5;
    pub const VIDEO_AV1: i32 = 6;
    pub const VIDEO_H265: i32 = 7;
}

pub mod av_stream_type {
    pub const AUDIO: i32 = 1;
    pub const VIDEO: i32 = 3;
}

pub mod display_type {
    pub const MAIN: i32 = 0;
    pub const CLUSTER: i32 = 1;
    pub const AUXILIARY: i32 = 2;
}

pub mod sensor_type {
    pub const DRIVING_STATUS: u8 = 13;
    pub const NIGHT_DATA: u8 = 10;
    pub const PARKING_BRAKE: u8 = 7;
    pub const GPS_LOCATION: u8 = 1;
    pub const CAR_SPEED: u8 = 3;
    pub const RPM: u8 = 4;
}

pub mod audio_type {
    pub const SPEECH: i32 = 1;
    pub const SYSTEM: i32 = 2;
    pub const MEDIA: i32 = 3;
}

pub mod bt_pairing_method {
    pub const NUMERIC_COMPARISON: i32 = 2;
    pub const PIN: i32 = 4;
}

pub mod color_scheme {
    pub const BASIC: i32 = 0;
    pub const MATERIAL_YOU_V2: i32 = 2;
    pub const MATERIAL_YOU_V3: i32 = 3;
}

pub mod av_setup_status {
    pub const NONE: i32 = 0;
    pub const FAIL: i32 = 1;
    pub const OK: i32 = 2;
}
