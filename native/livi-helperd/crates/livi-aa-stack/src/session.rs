//! The helper has already done the version exchange and TLS.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::channels::audio::{AudioChannel, AudioChannelEvent, AudioChannelType};
use crate::channels::input::{self, TouchPointer};
use crate::channels::media_info::{
    MediaInfoChannel, MediaInfoEvent, MediaPlaybackMetadata, MediaPlaybackStatus,
};
use crate::channels::mic::{MicChannel, MicEvent};
use crate::channels::navigation::{NavigationChannel, NavigationEvent};
use crate::channels::video::{VideoChannel, VideoChannelEvent};
use crate::channels::{Emit, Frame};
use crate::codec::{check_nested, decode, encode};
use crate::config::{AaConfig, Geometry};
use crate::consts::{STATUS_OK, av_msg, av_setup_status, ch, ctrl_msg, frame_flags};
use crate::control::{ControlChannel, ControlEvent, ControlOut};
use crate::discovery::{self, VideoCodec};
use crate::log::{debug, detail, trace};
use crate::proto::aap_protobuf::service::bluetooth::message::{
    BluetoothPairingRequest, BluetoothPairingResponse,
};
use crate::proto::aap_protobuf::service::control::message::{
    AuthResponse, ChannelOpenResponse, PingRequest,
};
use crate::proto::aap_protobuf::service::phonestatus::message::PhoneStatus;
use crate::proto::oaa::proto::messages::{AvChannelSetupRequest, AvChannelSetupResponse};
use crate::sensors::{self, Sensor};
use crate::wire::{decode_start, encode_uvarint};

pub const PING_INTERVAL: Duration = Duration::from_millis(1500);
const PING_TIMEOUT: Duration = Duration::from_millis(5000);
const WATCHDOG: Duration = Duration::from_secs(30);
const KEYFRAME_FOLLOW_UP: Duration = Duration::from_millis(60);
const SHUTDOWN_ACK: Duration = Duration::from_secs(1);

pub const BYEBYE_USER_SELECTION: u8 = 1;

const SENSOR_START_REQUEST: u16 = 0x8001;
const SENSOR_START_RESPONSE: u16 = 0x8002;
const SENSOR_BATCH: u16 = 0x8003;
const PHONE_STATUS: u16 = 0x8001;
const BT_PAIRING_REQUEST: u16 = 0x8001;
const BT_PAIRING_RESPONSE: u16 = 0x8002;
const WIFI_CREDENTIALS_REQUEST: u16 = 0x8001;
const WIFI_CREDENTIALS_RESPONSE: u16 = 0x8002;
const KEY_BINDING_REQUEST: u16 = 0x8002;
const KEY_BINDING_RESPONSE: u16 = 0x8003;

/// Focus indications: projected asks for a keyframe, native stops the encoder.
const FOCUS_PROJECTED: [u8; 2] = [0x08, 0x01];
const FOCUS_NATIVE: [u8; 2] = [0x08, 0x02];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Init,
    Auth,
    ServiceDiscovery,
    ChannelSetup,
    Running,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    Watchdog,
    /// The second focus indication of a keyframe request.
    MainKeyframe,
    ClusterKeyframe,
    ShutdownTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneCall {
    pub state: CallState,
    pub duration_s: u32,
    pub number: Option<String>,
    pub caller_id: Option<String>,
    pub number_type: Option<String>,
    pub thumbnail: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Unknown,
    InCall,
    OnHold,
    Inactive,
    Incoming,
    Conferenced,
    Muted,
}

impl CallState {
    fn of(raw: i32) -> Self {
        match raw {
            1 => Self::InCall,
            2 => Self::OnHold,
            3 => Self::Inactive,
            4 => Self::Incoming,
            5 => Self::Conferenced,
            6 => Self::Muted,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// Sent once the main video channel is set up.
    Connected,
    /// Always sent exactly once.
    Disconnected(String),
    Error(String),
    VideoCodec(VideoCodec),
    ClusterVideoCodec(VideoCodec),
    AudioSetup {
        channel: AudioChannelType,
        sample_rate: u32,
        channels: u32,
    },
    Audio {
        channel: AudioChannelType,
        sample_rate: u32,
        channels: u32,
        active: bool,
    },
    MicStart,
    MicStop,
    VoiceSession(bool),
    AudioFocus(u8),
    HostUiRequested,
    DeviceInfo {
        name: String,
        model: String,
        instance_id: String,
        ip: String,
    },
    Battery {
        ip: String,
        level: Option<u32>,
        critical: bool,
        time_remaining_s: Option<u32>,
    },
    Signal {
        ip: String,
        strength: u32,
    },
    Calls {
        ip: String,
        calls: Vec<PhoneCall>,
    },
    VideoFocusProjected,
    ClusterVideoFocusProjected,
    VideoStarted,
    ClusterVideoStarted,
    MediaMetadata(MediaPlaybackMetadata),
    MediaStatus(MediaPlaybackStatus),
    Navigation(NavigationEvent),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Send(Frame),
    Control(Value),
    /// The helper half-closes towards the phone, reading goes on.
    End,
    Destroy,
    Event(SessionEvent),
    Ping(bool),
    After(Duration, Timer),
    ShutdownDone,
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_WALL: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn wall_ms() -> u64 {
    #[cfg(test)]
    if let Some(ms) = TEST_WALL.with(std::cell::Cell::get) {
        return ms;
    }
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn state_name(s: State) -> &'static str {
    match s {
        State::Init => "INIT",
        State::Auth => "AUTH",
        State::ServiceDiscovery => "SERVICE_DISCOVERY",
        State::ChannelSetup => "CHANNEL_SETUP",
        State::Running => "RUNNING",
        State::Closed => "CLOSED",
    }
}

/// Kept out of the DEBUG log. Sensors count because the phone walks every
/// sensor type at the start.
fn is_frame_channel(c: u8) -> bool {
    matches!(
        c,
        ch::VIDEO
            | ch::CLUSTER_VIDEO
            | ch::MEDIA_AUDIO
            | ch::SPEECH_AUDIO
            | ch::SYSTEM_AUDIO
            | ch::INPUT
            | ch::MIC_INPUT
            | ch::SENSOR
    )
}

fn is_ping_pong(c: u8, msg_id: u16) -> bool {
    c == ch::CONTROL && (msg_id == ctrl_msg::PING_REQUEST || msg_id == ctrl_msg::PING_RESPONSE)
}

pub struct Session {
    config: watch::Receiver<AaConfig>,
    /// Touches map to this geometry for the whole session.
    advertised: Option<AaConfig>,
    peer: String,
    state: State,
    disconnected: bool,
    link_ready: bool,
    mic_socket: Option<String>,
    last_pong: Instant,
    ping_running: bool,
    control: ControlChannel,
    video: VideoChannel,
    cluster: VideoChannel,
    audio: Vec<AudioChannel>,
    mic: MicChannel,
    media: MediaInfoChannel,
    nav: NavigationChannel,
    video_codecs: Vec<VideoCodec>,
    video_codec: Option<VideoCodec>,
    phone_codec_logged: bool,
    cluster_codecs: Vec<VideoCodec>,
    cluster_codec: Option<VideoCodec>,
    main_frame_seen: bool,
    cluster_focus_pending: bool,
    cluster_stream_wanted: bool,
    shutdowns_waiting: u32,
}

impl Session {
    pub fn new(config: watch::Receiver<AaConfig>, peer: &str) -> (Self, Vec<Out>) {
        let session = Self {
            config,
            advertised: None,
            peer: peer.to_string(),
            state: State::Init,
            disconnected: false,
            link_ready: false,
            mic_socket: None,
            last_pong: Instant::now(),
            ping_running: false,
            control: ControlChannel,
            video: VideoChannel::new(ch::VIDEO),
            cluster: VideoChannel::new(ch::CLUSTER_VIDEO),
            audio: [ch::MEDIA_AUDIO, ch::SPEECH_AUDIO, ch::SYSTEM_AUDIO]
                .into_iter()
                .map(AudioChannel::new)
                .collect(),
            mic: MicChannel::new(ch::MIC_INPUT),
            media: MediaInfoChannel,
            nav: NavigationChannel,
            video_codecs: Vec::new(),
            video_codec: None,
            phone_codec_logged: false,
            cluster_codecs: Vec::new(),
            cluster_codec: None,
            main_frame_seen: false,
            cluster_focus_pending: false,
            cluster_stream_wanted: true,
            shutdowns_waiting: 0,
        };
        (session, vec![Out::After(WATCHDOG, Timer::Watchdog)])
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn mic_socket_path(&self) -> Option<&str> {
        self.mic_socket.as_deref()
    }

    /// 16 kHz mono until the phone's setup.
    pub fn mic_format(&self) -> (u32, u32) {
        self.mic.format()
    }

    pub fn config(&self) -> AaConfig {
        self.advertised.clone().unwrap_or_else(|| self.config.borrow().clone())
    }

    pub fn peer(&self) -> String {
        let raw = self.peer.as_str();
        let raw = match raw.get(..7) {
            Some(prefix) if prefix.eq_ignore_ascii_case("::ffff:") => &raw[7..],
            _ => raw,
        };
        raw.split('%').next().unwrap_or("").to_string()
    }

    fn send(&self, out: &mut Vec<Out>, frame: Frame) {
        let encrypted = frame.flags & frame_flags::ENCRYPTED != 0;
        if self.state == State::Closed || (encrypted && self.state < State::Auth) {
            return;
        }
        out.push(Out::Send(frame));
    }

    fn send_to(
        &self,
        out: &mut Vec<Out>,
        c: u8,
        flags: u8,
        msg_id: u16,
        payload: impl Into<Vec<u8>>,
    ) {
        self.send(out, Frame::new(c, flags, msg_id, payload));
    }

    fn event(&self, out: &mut Vec<Out>, event: SessionEvent) {
        out.push(Out::Event(event));
    }

    fn transition(&mut self, out: &mut Vec<Out>, next: State, reason: &str) {
        self.state = next;
        if next == State::Closed {
            self.stop_ping(out);
            if !self.disconnected {
                self.disconnected = true;
                out.push(Out::Event(SessionEvent::Disconnected(reason.to_string())));
            }
        }
    }

    fn stop_ping(&mut self, out: &mut Vec<Out>) {
        if self.ping_running {
            self.ping_running = false;
            out.push(Out::Ping(false));
        }
    }

    pub fn close(&mut self, reason: &str) -> Vec<Out> {
        let mut out = vec![Out::Destroy];
        if self.state != State::Closed {
            self.transition(&mut out, State::Closed, reason);
        }
        out
    }

    pub fn on_link_closed(&mut self, error: Option<String>) -> Vec<Out> {
        let mut out = Vec::new();
        match error {
            Some(e) => {
                self.event(&mut out, SessionEvent::Error(e.clone()));
                self.transition(&mut out, State::Closed, &e);
            }
            None => self.transition(&mut out, State::Closed, "socket closed"),
        }
        out
    }

    pub fn on_link_control(&mut self, c: &Value) -> Vec<Out> {
        let mut out = Vec::new();
        match c.get("type").and_then(Value::as_str) {
            Some("ready") => {
                self.link_ready = true;
                self.mic_socket = c.get("mic").and_then(Value::as_str).map(str::to_string);
                self.on_link_ready(&mut out);
            }
            Some("first-frame") => {
                let channel = c.get("ch").and_then(Value::as_f64);
                if channel == Some(f64::from(ch::VIDEO)) {
                    if !self.main_frame_seen {
                        self.main_frame_seen = true;
                        if self.cluster_focus_pending {
                            self.request_cluster_stream(&mut out);
                        }
                    }
                    self.event(&mut out, SessionEvent::VideoStarted);
                } else if channel == Some(f64::from(ch::CLUSTER_VIDEO)) {
                    self.event(&mut out, SessionEvent::ClusterVideoStarted);
                }
            }
            Some("eof") => {
                self.stop_ping(&mut out);
                if self.state != State::Running {
                    out.push(Out::End);
                }
            }
            Some("closed") => {
                let reason = c.get("reason").and_then(Value::as_str).unwrap_or("helper closed");
                self.transition(&mut out, State::Closed, reason);
            }
            _ => {}
        }
        out
    }

    fn on_link_ready(&mut self, out: &mut Vec<Out>) {
        if !self.link_ready || self.state >= State::Auth {
            return;
        }
        self.transition(out, State::Auth, "");
        let auth = encode(&AuthResponse { status: STATUS_OK });
        if debug() {
            println!("[Session] AUTH_COMPLETE proto bytes: {}", crate::log::hex(&auth));
        }
        self.send_to(out, ch::CONTROL, frame_flags::PLAINTEXT, ctrl_msg::AUTH_COMPLETE, auth);
        self.transition(out, State::ServiceDiscovery, "");
        if debug() {
            println!("[Session] AUTH_COMPLETE sent, waiting for SERVICE_DISCOVERY_REQUEST");
        }
    }

    pub fn on_message(&mut self, c: u8, msg_id: u16, payload: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        self.last_pong = Instant::now();
        if debug() && (trace() || (!is_frame_channel(c) && !is_ping_pong(c, msg_id))) {
            println!(
                "[Session] MSG ch={c} msgId=0x{msg_id:04x} len={} state={}",
                payload.len(),
                state_name(self.state)
            );
        }

        if c == ch::CONTROL {
            let emitted = self.control.handle_message(msg_id, payload);
            self.on_control(&mut out, emitted);
            return out;
        }
        if msg_id == ctrl_msg::CHANNEL_OPEN_REQUEST {
            if debug() {
                println!("[Session] CHANNEL_OPEN_REQUEST ch={c}, responding OK");
            }
            let resp = encode(&ChannelOpenResponse { status: STATUS_OK });
            self.send_to(
                &mut out,
                c,
                frame_flags::ENC_CONTROL,
                ctrl_msg::CHANNEL_OPEN_RESPONSE,
                resp,
            );
            return out;
        }
        if c == ch::VIDEO || c == ch::CLUSTER_VIDEO {
            if msg_id == av_msg::SETUP_REQUEST {
                self.on_av_setup_request(&mut out, c, payload);
            } else {
                self.on_video_message(&mut out, c, msg_id, payload);
            }
            return out;
        }
        if msg_id != av_msg::SETUP_REQUEST
            && let Some(i) = self.audio.iter().position(|a| a.channel_id() == c)
        {
            let channel = self.audio[i].channel_type();
            let active = match self.audio[i].handle_message(msg_id, payload) {
                Some(AudioChannelEvent::Start) => true,
                Some(AudioChannelEvent::Stop) => false,
                _ => return out,
            };
            let (sample_rate, channels) = self.audio[i].format();
            self.event(&mut out, SessionEvent::Audio { channel, sample_rate, channels, active });
            return out;
        }
        match c {
            ch::SENSOR => {
                if msg_id == SENSOR_START_REQUEST {
                    self.on_sensor_start_request(&mut out, payload);
                } else if debug() {
                    println!("[Session] sensor ch={c} msgId=0x{msg_id:04x} (unhandled)");
                }
                return out;
            }
            ch::MEDIA_INFO => {
                match self.media.handle_message(msg_id, payload) {
                    Some(MediaInfoEvent::Metadata(m)) => {
                        self.event(&mut out, SessionEvent::MediaMetadata(m));
                    }
                    Some(MediaInfoEvent::Status(s)) => {
                        self.event(&mut out, SessionEvent::MediaStatus(s));
                    }
                    None => {}
                }
                return out;
            }
            ch::PHONE_STATUS => {
                if msg_id == PHONE_STATUS {
                    self.on_phone_status(&mut out, payload);
                }
                return out;
            }
            ch::NAVIGATION => {
                if let Some(e) = self.nav.handle_message(msg_id, payload) {
                    self.event(&mut out, SessionEvent::Navigation(e));
                }
                return out;
            }
            ch::BLUETOOTH => {
                if msg_id == BT_PAIRING_REQUEST {
                    // The phone routes call audio to our hands-free profile only
                    // after this answer.
                    match decode::<BluetoothPairingRequest>(payload, &[1, 2]) {
                        Ok(req) => detail!(
                            "[Session] BT pairing request from {} -> already paired",
                            req.phone_address
                        ),
                        Err(e) => {
                            if debug() {
                                eprintln!("[Session] BT pairing request parse error: {e}");
                            }
                        }
                    }
                    let resp =
                        encode(&BluetoothPairingResponse { status: 1, already_paired: true });
                    self.send_to(
                        &mut out,
                        ch::BLUETOOTH,
                        frame_flags::ENC_SIGNAL,
                        BT_PAIRING_RESPONSE,
                        resp,
                    );
                }
                return out;
            }
            ch::MIC_INPUT => {
                if msg_id == av_msg::SETUP_REQUEST {
                    self.on_av_setup_request(&mut out, c, payload);
                } else {
                    let emitted = self.mic.handle_message(msg_id, payload);
                    self.on_mic(&mut out, emitted);
                }
                return out;
            }
            ch::WIFI => {
                if msg_id == WIFI_CREDENTIALS_REQUEST {
                    if debug() {
                        println!("[Session] WifiCredentialsRequest received, sending credentials");
                    }
                    self.on_wifi_credentials_request(&mut out);
                } else if debug() {
                    println!("[Session] wifi ch={c} msgId=0x{msg_id:04x} (unhandled)");
                }
                return out;
            }
            _ => {}
        }
        if msg_id == av_msg::SETUP_REQUEST {
            self.on_av_setup_request(&mut out, c, payload);
            return out;
        }
        if msg_id == av_msg::START_INDICATION {
            if debug() {
                let start = decode_start(payload);
                let label = if matches!(c, ch::MEDIA_AUDIO | ch::SPEECH_AUDIO | ch::SYSTEM_AUDIO) {
                    "audio".to_string()
                } else {
                    format!("ch{c}")
                };
                detail!(
                    "[Session] {label} START_INDICATION ch={c} sessionId={} configIdx={}, stream starting",
                    start.map_or(-1, |s| i64::from(s.session_id)),
                    start.and_then(|s| s.config_index).map_or(-1, i64::from)
                );
            }
            return out;
        }
        if c == ch::INPUT && msg_id == KEY_BINDING_REQUEST {
            if debug() {
                println!(
                    "[Session] INPUT KeyBindingRequest (len={}), replying status=OK",
                    payload.len()
                );
            }
            self.send_to(
                &mut out,
                ch::INPUT,
                frame_flags::ENC_SIGNAL,
                KEY_BINDING_RESPONSE,
                [0x08, 0x00],
            );
            return out;
        }
        if debug() {
            println!("[Session] unhandled ch={c} msgId=0x{msg_id:04x}");
        }
        out
    }

    fn on_video_message(&mut self, out: &mut Vec<Out>, c: u8, msg_id: u16, payload: &[u8]) {
        let cluster = c == ch::CLUSTER_VIDEO;
        let channel = if cluster { &mut self.cluster } else { &mut self.video };
        let (answer, event) = channel.handle_message(msg_id, payload);
        if let Some(frame) = answer {
            self.send(out, frame);
        }
        match (event, cluster) {
            (Some(VideoChannelEvent::HostUiRequested), false) => {
                self.event(out, SessionEvent::HostUiRequested);
            }
            (Some(VideoChannelEvent::VideoFocusProjected), false) => {
                self.event(out, SessionEvent::VideoFocusProjected);
            }
            (Some(VideoChannelEvent::VideoFocusProjected), true) => {
                self.event(out, SessionEvent::ClusterVideoFocusProjected);
            }
            _ => {}
        }
    }

    fn on_mic(&mut self, out: &mut Vec<Out>, emitted: Vec<Emit<MicEvent>>) {
        for e in emitted {
            match e {
                Emit::Send(frame) => self.send(out, frame),
                Emit::Event(MicEvent::Start) => self.event(out, SessionEvent::MicStart),
                Emit::Event(MicEvent::Stop) => self.event(out, SessionEvent::MicStop),
            }
        }
    }

    fn on_control(&mut self, out: &mut Vec<Out>, emitted: Vec<ControlOut>) {
        for e in emitted {
            match e {
                Emit::Send(frame) => self.send(out, frame),
                Emit::Event(event) => self.on_control_event(out, event),
            }
        }
    }

    fn on_control_event(&mut self, out: &mut Vec<Out>, event: ControlEvent) {
        match event {
            ControlEvent::ServiceDiscoveryRequest(req) => {
                if debug() {
                    println!(
                        "[Session] Phone: {} / {}",
                        req.device_name.as_deref().unwrap_or("?"),
                        req.device_brand.as_deref().unwrap_or("?")
                    );
                }
                let name = req.device_name.clone().unwrap_or_default();
                let model = req.device_brand.clone().unwrap_or_default();
                let instance_id =
                    req.phone_info.as_ref().and_then(|p| p.instance_id.clone()).unwrap_or_default();
                if !name.is_empty() || !model.is_empty() || !instance_id.is_empty() {
                    let ip = self.peer();
                    self.event(out, SessionEvent::DeviceInfo { name, model, instance_id, ip });
                }
                let cfg = self.config.borrow().clone();
                let sdr = discovery::build(&cfg);
                self.advertised = Some(cfg);
                self.video_codecs = sdr.video_codecs;
                self.cluster_codecs = sdr.cluster_codecs;
                self.phone_codec_logged = false;
                self.send_to(
                    out,
                    ch::CONTROL,
                    frame_flags::ENC_SIGNAL,
                    ctrl_msg::SERVICE_DISCOVERY_RESPONSE,
                    sdr.buf,
                );
                // Video focus waits for the video setup answer. Sent this early,
                // it makes the phone release audio focus and hang up.
                self.last_pong = Instant::now();
                self.send_ping(out);
                if !self.ping_running {
                    self.ping_running = true;
                    out.push(Out::Ping(true));
                }
                if debug() {
                    println!("[Session] SDR + Ping sent (1500ms interval)");
                }
                self.transition(out, State::ChannelSetup, "");
                if debug() {
                    println!(
                        "[Session] Channel setup, waiting for phone CHANNEL_OPEN_REQUEST on each service channel"
                    );
                }
            }
            ControlEvent::ChannelOpenRequest(_) => {
                let frame = self.control.channel_open_response(STATUS_OK);
                self.send(out, frame);
            }
            ControlEvent::AvSetupRequest { ch, payload } => {
                self.on_av_setup_request(out, ch, &payload);
            }
            ControlEvent::Ping(_) => {}
            ControlEvent::Pong => self.last_pong = Instant::now(),
            ControlEvent::AudioFocusRequest(t) => self.event(out, SessionEvent::AudioFocus(t)),
            ControlEvent::Battery { level, critical, time_remaining_s } => {
                let ip = self.peer();
                self.event(out, SessionEvent::Battery { ip, level, critical, time_remaining_s });
            }
            ControlEvent::Shutdown(reason) => {
                if debug() {
                    println!("[Session] Phone shutdown, reason={reason}");
                }
                self.transition(out, State::Closed, &format!("phone shutdown reason={reason}"));
            }
            ControlEvent::ShutdownComplete => {
                let waiting = std::mem::take(&mut self.shutdowns_waiting);
                for _ in 0..waiting {
                    self.finish_shutdown(out, "acked by phone (ByeByeResponse)");
                }
            }
            ControlEvent::VoiceSession(active) => {
                self.event(out, SessionEvent::VoiceSession(active));
            }
        }
    }

    fn send_ping(&mut self, out: &mut Vec<Out>) {
        let ping = encode(&PingRequest {
            timestamp: (wall_ms() * 1000) as i64,
            bug_report: None,
            data: None,
        });
        self.send_to(out, ch::CONTROL, frame_flags::PLAINTEXT, ctrl_msg::PING_REQUEST, ping);
    }

    pub fn on_ping_tick(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state >= State::Closed {
            return out;
        }
        if self.last_pong.elapsed() > PING_TIMEOUT {
            println!(
                "[Session] PING timeout ({}ms without PING_RESPONSE), closing session",
                PING_TIMEOUT.as_millis()
            );
            self.transition(&mut out, State::Closed, "ping timeout");
            out.push(Out::Destroy);
            return out;
        }
        self.send_ping(&mut out);
        out
    }

    pub fn on_timer(&mut self, timer: Timer) -> Vec<Out> {
        let mut out = Vec::new();
        match timer {
            Timer::Watchdog => {
                if self.state >= State::Running {
                    return out;
                }
                eprintln!(
                    "[Session] pre-RUNNING watchdog fired: stuck in state {} after 30s, aborting",
                    state_name(self.state)
                );
                self.event(
                    &mut out,
                    SessionEvent::Error(
                        "session stalled in pre-RUNNING state, phone-side AA service likely zombie"
                            .into(),
                    ),
                );
                out.extend(self.close("pre-RUNNING watchdog"));
            }
            Timer::MainKeyframe => {
                if self.state == State::Running {
                    self.send_to(
                        &mut out,
                        ch::VIDEO,
                        frame_flags::ENC_SIGNAL,
                        av_msg::VIDEO_FOCUS_INDICATION,
                        FOCUS_PROJECTED,
                    );
                }
            }
            Timer::ClusterKeyframe => {
                if self.state == State::Running && self.cluster_stream_wanted {
                    self.send_to(
                        &mut out,
                        ch::CLUSTER_VIDEO,
                        frame_flags::ENC_SIGNAL,
                        av_msg::VIDEO_FOCUS_INDICATION,
                        FOCUS_PROJECTED,
                    );
                }
            }
            Timer::ShutdownTimeout => {
                if self.shutdowns_waiting > 0 {
                    self.shutdowns_waiting -= 1;
                    self.finish_shutdown(&mut out, "fallback timeout (no ByeByeResponse)");
                }
            }
        }
        out
    }

    fn on_av_setup_request(&mut self, out: &mut Vec<Out>, c: u8, payload: &[u8]) {
        let codec = match decode::<AvChannelSetupRequest>(payload, &[1]) {
            Ok(req) => req.media_codec_type,
            Err(e) => {
                eprintln!("[Session] AVSetupRequest ch={c} unreadable ({e}), dropped");
                return;
            }
        };
        if debug() {
            println!("[Session] AVSetupRequest ch={c} codec={codec}");
        }
        if let Some(i) = self.audio.iter().position(|a| a.channel_id() == c) {
            let (rate, count) = if c == ch::MEDIA_AUDIO { (48000, 2) } else { (16000, 1) };
            let channel = self.audio[i].channel_type();
            if let AudioChannelEvent::Setup { sample_rate, channels, .. } =
                self.audio[i].handle_setup_request(codec, rate, count)
            {
                self.event(out, SessionEvent::AudioSetup { channel, sample_rate, channels });
            }
        } else if c == ch::MIC_INPUT {
            self.mic.handle_setup_request(codec, 16000, 1);
        }

        // The phone drops the session on status NONE, so it has to be OK.
        let mut config_index = 0;
        if c == ch::VIDEO {
            let want = VideoCodec::of_media_codec(codec);
            if let Some(i) = self.video_codecs.iter().position(|v| *v == want) {
                config_index = i as u32;
            }
            if self.video_codec != Some(want) {
                self.video_codec = Some(want);
                self.event(out, SessionEvent::VideoCodec(want));
                if debug() {
                    println!(
                        "[Session] video codec selected: {} (configIdx={config_index})",
                        want.name()
                    );
                }
            }
            if !self.phone_codec_logged {
                self.phone_codec_logged = true;
                let offered: Vec<_> = self.video_codecs.iter().map(|v| v.name()).collect();
                println!(
                    "[Session] phone picked video codec: {} (offered: {})",
                    want.name().to_uppercase(),
                    offered.join(", ")
                );
            }
        } else if c == ch::CLUSTER_VIDEO {
            let want = VideoCodec::of_media_codec(codec);
            if let Some(i) = self.cluster_codecs.iter().position(|v| *v == want) {
                config_index = i as u32;
            }
            if self.cluster_codec != Some(want) {
                self.cluster_codec = Some(want);
                self.event(out, SessionEvent::ClusterVideoCodec(want));
                if debug() {
                    println!(
                        "[Session] cluster codec selected: {} (configIdx={config_index})",
                        want.name()
                    );
                }
            }
        }
        let resp = encode(&AvChannelSetupResponse {
            media_status: av_setup_status::OK,
            max_unacked: Some(1),
            configs: vec![config_index],
        });
        self.send_to(out, c, frame_flags::ENC_SIGNAL, av_msg::SETUP_RESPONSE, resp);
        if debug() {
            println!(
                "[Session] AVChannelSetupResponse ch={c} status=OK({}) sent",
                av_setup_status::OK
            );
        }

        if c == ch::VIDEO {
            self.send_to(
                out,
                ch::VIDEO,
                frame_flags::ENC_SIGNAL,
                av_msg::VIDEO_FOCUS_INDICATION,
                FOCUS_PROJECTED,
            );
            // The phone starts the stream itself once it is ready.
            self.transition(out, State::Running, "");
            self.event(out, SessionEvent::Connected);
        } else if c == ch::CLUSTER_VIDEO {
            self.request_cluster_stream(out);
        }
    }

    fn on_sensor_start_request(&mut self, out: &mut Vec<Out>, payload: &[u8]) {
        let sensor_type = match payload {
            [0x08, t, ..] => *t,
            _ => 0,
        };
        if debug() {
            println!("[Session] SensorStartRequest type={sensor_type}");
        }
        self.send_to(out, ch::SENSOR, frame_flags::ENC_SIGNAL, SENSOR_START_RESPONSE, [0x08, 0x00]);
        if sensor_type == 13 {
            self.send_to(
                out,
                ch::SENSOR,
                frame_flags::ENC_SIGNAL,
                SENSOR_BATCH,
                [0x6a, 0x02, 0x08, 0x00],
            );
        } else if sensor_type == 10 {
            let night = self.config.borrow().initial_night_mode == Some(true);
            self.send_to(
                out,
                ch::SENSOR,
                frame_flags::ENC_SIGNAL,
                SENSOR_BATCH,
                [0x52, 0x02, 0x08, u8::from(night)],
            );
            if debug() {
                println!("[Session] SensorBatch: NightMode={night} sent");
            }
        }
    }

    fn on_phone_status(&mut self, out: &mut Vec<Out>, payload: &[u8]) {
        let status =
            check_nested(payload, 1, &[1, 2]).and_then(|()| decode::<PhoneStatus>(payload, &[]));
        let ps = match status {
            Ok(ps) => ps,
            Err(e) => {
                if debug() {
                    eprintln!("[Session] phone-status parse error: {e}");
                }
                return;
            }
        };
        let ip = self.peer();
        if let Some(strength) = ps.signal_strength {
            self.event(out, SessionEvent::Signal { ip: ip.clone(), strength });
        }
        let calls = ps
            .calls
            .into_iter()
            .map(|c| PhoneCall {
                state: CallState::of(c.phone_state),
                duration_s: c.call_duration_seconds,
                number: c.caller_number,
                caller_id: c.caller_id,
                number_type: c.caller_number_type,
                thumbnail: c.caller_thumbnail,
            })
            .collect();
        self.event(out, SessionEvent::Calls { ip, calls });
    }

    fn on_wifi_credentials_request(&mut self, out: &mut Vec<Out>) {
        let (ssid, pass) = {
            let cfg = self.config.borrow();
            (cfg.wifi_ssid.clone(), cfg.wifi_password.clone())
        };
        if ssid.is_empty() && debug() {
            eprintln!(
                "[Session] WifiCredentialsRequest: no wifiSsid configured, sending empty response"
            );
        }
        let mut resp = Vec::new();
        if !pass.is_empty() {
            resp.push(0x0a);
            resp.extend(encode_uvarint(pass.len() as u64));
            resp.extend(pass.as_bytes());
        }
        // WPA2 personal, in this message's own numbering.
        resp.extend([0x10, 0x05]);
        if !ssid.is_empty() {
            resp.push(0x1a);
            resp.extend(encode_uvarint(ssid.len() as u64));
            resp.extend(ssid.as_bytes());
        }
        // A static access point.
        resp.extend([0x28, 0x00]);
        if debug() {
            println!(
                "[Session] WifiCredentialsResponse: ssid=\"{ssid}\" security=WPA2_PERSONAL(5) type=STATIC"
            );
        }
        self.send_to(out, ch::WIFI, frame_flags::ENC_SIGNAL, WIFI_CREDENTIALS_RESPONSE, resp);
    }

    fn ts_micros(&self) -> u64 {
        wall_ms() * 1000
    }

    /// Coordinates in the advertised touchscreen's pixels.
    pub fn send_touch(
        &mut self,
        action: u32,
        pointers: &[TouchPointer],
        action_index: u32,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running {
            return out;
        }
        if let Some(frame) = input::touch(self.ts_micros(), action, pointers, action_index) {
            self.send(&mut out, frame);
        }
        out
    }

    pub fn send_button(&mut self, codes: &[u32], down: bool) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running {
            return out;
        }
        if let Some(frame) = input::button(self.ts_micros(), codes, down, false) {
            self.send(&mut out, frame);
        }
        out
    }

    /// `direction` is -1 to turn back, 1 to turn forward.
    pub fn send_rotary(&mut self, direction: i64) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running {
            return out;
        }
        let frame = input::rotary(self.ts_micros(), direction);
        self.send(&mut out, frame);
        out
    }

    pub fn send_sensor(&mut self, sensor: &Sensor) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running {
            return out;
        }
        if let Some(batch) = sensors::batch(sensor) {
            self.send_to(&mut out, ch::SENSOR, frame_flags::ENC_SIGNAL, SENSOR_BATCH, batch);
            if debug() {
                println!("[Session] SensorBatch {sensor:?}");
            }
        }
        out
    }

    pub fn request_video_focus(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running {
            return out;
        }
        self.send_to(
            &mut out,
            ch::VIDEO,
            frame_flags::ENC_SIGNAL,
            av_msg::VIDEO_FOCUS_REQUEST,
            [0x10, 0x01, 0x18, 0x00],
        );
        if debug() {
            println!("[Session] main video focus request (PROJECTED) sent");
        }
        out
    }

    /// Native then projected focus, which makes the phone send a keyframe.
    pub fn request_main_keyframe(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running {
            return out;
        }
        self.send_to(
            &mut out,
            ch::VIDEO,
            frame_flags::ENC_SIGNAL,
            av_msg::VIDEO_FOCUS_INDICATION,
            FOCUS_NATIVE,
        );
        out.push(Out::After(KEYFRAME_FOLLOW_UP, Timer::MainKeyframe));
        out
    }

    pub fn request_cluster_keyframe(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        self.request_cluster_stream(&mut out);
        out
    }

    pub fn force_cluster_keyframe(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state != State::Running || !self.cluster_stream_wanted {
            return out;
        }
        self.send_to(
            &mut out,
            ch::CLUSTER_VIDEO,
            frame_flags::ENC_SIGNAL,
            av_msg::VIDEO_FOCUS_INDICATION,
            FOCUS_NATIVE,
        );
        out.push(Out::After(KEYFRAME_FOLLOW_UP, Timer::ClusterKeyframe));
        out
    }

    pub fn set_cluster_stream_active(&mut self, active: bool) -> Vec<Out> {
        let mut out = Vec::new();
        if self.cluster_stream_wanted == active {
            return out;
        }
        self.cluster_stream_wanted = active;
        if active {
            self.request_cluster_stream(&mut out);
        } else {
            self.cluster_focus_pending = false;
            if self.state == State::Running {
                self.send_to(
                    &mut out,
                    ch::CLUSTER_VIDEO,
                    frame_flags::ENC_SIGNAL,
                    av_msg::VIDEO_FOCUS_INDICATION,
                    FOCUS_NATIVE,
                );
                if debug() {
                    println!("[Session] cluster video focus indication (NATIVE) sent");
                }
            }
        }
        out
    }

    fn request_cluster_stream(&mut self, out: &mut Vec<Out>) {
        if !self.cluster_stream_wanted {
            return;
        }
        if self.state != State::Running || !self.main_frame_seen {
            self.cluster_focus_pending = true;
            if debug() {
                println!("[Session] cluster stream request held until first main frame");
            }
            return;
        }
        self.cluster_focus_pending = false;
        self.send_to(
            out,
            ch::CLUSTER_VIDEO,
            frame_flags::ENC_SIGNAL,
            av_msg::VIDEO_FOCUS_INDICATION,
            FOCUS_PROJECTED,
        );
        if debug() {
            println!("[Session] cluster video focus indication (PROJECTED) sent");
        }
    }

    pub fn request_shutdown(&mut self, reason: u8) -> Vec<Out> {
        let mut out = Vec::new();
        if self.state >= State::Closed {
            out.push(Out::ShutdownDone);
            return out;
        }
        if debug() {
            println!("[Session] requesting shutdown reason={reason}");
        }
        self.send_to(
            &mut out,
            ch::CONTROL,
            frame_flags::ENC_SIGNAL,
            ctrl_msg::SHUTDOWN_REQUEST,
            [0x08, reason],
        );
        self.shutdowns_waiting += 1;
        out.push(Out::After(SHUTDOWN_ACK, Timer::ShutdownTimeout));
        out
    }

    fn finish_shutdown(&mut self, out: &mut Vec<Out>, how: &str) {
        println!("[Session] shutdown {how}");
        self.transition(out, State::Closed, "hu-initiated shutdown");
        out.push(Out::End);
        out.push(Out::ShutdownDone);
    }

    pub fn send_media_sink(&self, fields: Map<String, Value>) -> Vec<Out> {
        let mut sink = Map::new();
        sink.insert("type".into(), Value::String("sink".into()));
        sink.extend(fields);
        vec![Out::Control(Value::Object(sink))]
    }

    pub fn geometry(&self) -> Geometry {
        Geometry::main(&self.config())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::wire::field_varint;

    pub(crate) fn frames(out: &[Out]) -> Vec<Frame> {
        out.iter()
            .filter_map(|o| match o {
                Out::Send(f) => Some(f.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn events(out: &[Out]) -> Vec<SessionEvent> {
        out.iter()
            .filter_map(|o| match o {
                Out::Event(e) => Some(e.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn session(cfg: AaConfig) -> (Session, watch::Sender<AaConfig>) {
        let (tx, rx) = watch::channel(cfg);
        let (s, out) = Session::new(rx, "::ffff:10.0.0.5%wlan0");
        assert_eq!(out, [Out::After(WATCHDOG, Timer::Watchdog)]);
        TEST_WALL.with(|w| w.set(Some(1_700_000_000_000)));
        (s, tx)
    }

    pub(crate) fn running(cfg: AaConfig) -> (Session, watch::Sender<AaConfig>) {
        let (mut s, tx) = session(cfg);
        s.on_link_control(&serde_json::json!({ "type": "ready", "mic": "/tmp/m.sock" }));
        s.on_message(ch::CONTROL, ctrl_msg::SERVICE_DISCOVERY_REQUEST, &[]);
        s.on_message(ch::VIDEO, av_msg::SETUP_REQUEST, &[0x08, 0x03]);
        assert_eq!(s.state(), State::Running);
        (s, tx)
    }

    #[test]
    fn ready_sends_auth_and_discovery_runs_the_pings() {
        let (mut s, _tx) = session(AaConfig::default());
        assert!(s.send_touch(0, &[TouchPointer { x: 1, y: 1, id: 0 }], 0).is_empty());
        let out = s.on_link_control(&serde_json::json!({ "type": "ready", "mic": "/tmp/m.sock" }));
        assert_eq!(
            frames(&out),
            [Frame::new(
                ch::CONTROL,
                frame_flags::PLAINTEXT,
                ctrl_msg::AUTH_COMPLETE,
                [0x08, 0x00]
            )]
        );
        assert_eq!(s.state(), State::ServiceDiscovery);
        assert_eq!(s.mic_socket_path(), Some("/tmp/m.sock"));
        assert!(s.on_link_control(&serde_json::json!({ "type": "ready" })).is_empty());
        assert_eq!(s.mic_socket_path(), None);

        let sdr_req = [0x22, 0x01, b'P', 0x2a, 0x01, b'B', 0x32, 0x03, 0x0a, 0x01, b'I'];
        let out = s.on_message(ch::CONTROL, ctrl_msg::SERVICE_DISCOVERY_REQUEST, &sdr_req);
        assert_eq!(
            events(&out),
            [SessionEvent::DeviceInfo {
                name: "P".into(),
                model: "B".into(),
                instance_id: "I".into(),
                ip: "10.0.0.5".into()
            }]
        );
        let sent = frames(&out);
        assert_eq!(sent[0].msg_id, ctrl_msg::SERVICE_DISCOVERY_RESPONSE);
        assert_eq!(sent[1].msg_id, ctrl_msg::PING_REQUEST);
        assert_eq!(sent[1].payload, [field_varint(1, 1_700_000_000_000_000)].concat());
        assert!(out.contains(&Out::Ping(true)));
        assert_eq!(s.state(), State::ChannelSetup);
        assert!(frames(&s.on_ping_tick())[0].msg_id == ctrl_msg::PING_REQUEST);
    }

    #[test]
    fn the_video_setup_starts_the_session() {
        let (mut s, _tx) = session(AaConfig { hevc_supported: true, ..Default::default() });
        s.on_link_control(&serde_json::json!({ "type": "ready" }));
        s.on_message(ch::CONTROL, ctrl_msg::SERVICE_DISCOVERY_REQUEST, &[]);
        let out = s.on_message(ch::VIDEO, av_msg::SETUP_REQUEST, &[0x08, 0x07]);
        assert_eq!(
            events(&out),
            [SessionEvent::VideoCodec(VideoCodec::H265), SessionEvent::Connected]
        );
        assert_eq!(
            frames(&out),
            [
                Frame::new(
                    ch::VIDEO,
                    frame_flags::ENC_SIGNAL,
                    av_msg::SETUP_RESPONSE,
                    [0x08, 0x02, 0x10, 0x01, 0x18, 0x01]
                ),
                Frame::new(
                    ch::VIDEO,
                    frame_flags::ENC_SIGNAL,
                    av_msg::VIDEO_FOCUS_INDICATION,
                    FOCUS_PROJECTED
                ),
            ]
        );
        let out = s.on_message(ch::VIDEO, av_msg::SETUP_REQUEST, &[0x08, 0x07]);
        assert_eq!(events(&out), [SessionEvent::Connected]);
        assert!(s.on_message(ch::VIDEO, av_msg::SETUP_REQUEST, &[]).is_empty());
    }

    #[test]
    fn encrypted_frames_wait_for_auth_and_nothing_goes_out_closed() {
        let (mut s, _tx) = session(AaConfig::default());
        assert!(s.on_message(ch::SENSOR, SENSOR_START_REQUEST, &[0x08, 0x0d]).is_empty());
        let out = s.on_message(ch::CONTROL, ctrl_msg::PING_REQUEST, &[0x08, 0x01]);
        assert_eq!(frames(&out).len(), 1);
        let out = s.close("bye");
        assert_eq!(out, [Out::Destroy, Out::Event(SessionEvent::Disconnected("bye".into()))]);
        assert_eq!(s.close("again"), [Out::Destroy]);
        assert!(s.on_message(ch::CONTROL, ctrl_msg::PING_REQUEST, &[0x08, 0x01]).is_empty());
        assert!(s.on_link_closed(None).is_empty());
    }

    #[test]
    fn a_config_change_reaches_the_session() {
        let (mut s, tx) = session(AaConfig::default());
        s.on_link_control(&serde_json::json!({ "type": "ready" }));
        tx.send_modify(|c| {
            c.hevc_supported = true;
            c.initial_night_mode = Some(true);
            c.wifi_ssid = "Car".into();
        });
        s.on_message(ch::CONTROL, ctrl_msg::SERVICE_DISCOVERY_REQUEST, &[]);
        let out = s.on_message(ch::VIDEO, av_msg::SETUP_REQUEST, &[0x08, 0x07]);
        assert_eq!(frames(&out)[0].payload, [0x08, 0x02, 0x10, 0x01, 0x18, 0x01]);
        let out = s.on_message(ch::SENSOR, SENSOR_START_REQUEST, &[0x08, 0x0a]);
        assert_eq!(frames(&out)[1].payload, [0x52, 0x02, 0x08, 0x01]);
        tx.send_modify(|c| c.wifi_password = "pw".into());
        let out = s.on_message(ch::WIFI, WIFI_CREDENTIALS_REQUEST, &[]);
        assert_eq!(
            frames(&out)[0].payload,
            [0x0a, 0x02, b'p', b'w', 0x10, 0x05, 0x1a, 0x03, b'C', b'a', b'r', 0x28, 0x00]
        );
        tx.send_modify(|c| c.video_width = Some(1920));
        assert_eq!(s.geometry().tier_width, 1280);
    }

    #[test]
    fn a_second_discovery_keeps_one_ping_interval() {
        let (mut s, _tx) = running(AaConfig::default());
        let out = s.on_message(ch::CONTROL, ctrl_msg::SERVICE_DISCOVERY_REQUEST, &[]);
        assert!(!out.contains(&Out::Ping(true)));
        assert_eq!(s.state(), State::ChannelSetup);
        assert_eq!(s.close("x").iter().filter(|o| **o == Out::Ping(false)).count(), 1);
    }

    #[test]
    fn calls_and_unreadable_setups() {
        let (mut s, _tx) = running(AaConfig::default());
        let out = s.on_message(
            ch::PHONE_STATUS,
            PHONE_STATUS,
            &[0x0a, 0x06, 0x08, 0x01, 0x10, 0x3c, 0x1a, 0x00],
        );
        assert_eq!(
            events(&out),
            [SessionEvent::Calls {
                ip: "10.0.0.5".into(),
                calls: vec![PhoneCall {
                    state: CallState::InCall,
                    duration_s: 60,
                    number: Some(String::new()),
                    caller_id: None,
                    number_type: None,
                    thumbnail: None,
                }]
            }]
        );
        assert!(s.on_message(ch::PHONE_STATUS, PHONE_STATUS, &[0x0a, 0x02, 0x08, 0x01]).is_empty());
        assert!(s.on_message(ch::MEDIA_AUDIO, av_msg::SETUP_REQUEST, &[0x10, 0x01]).is_empty());
        let gps = crate::sensors::GpsFix {
            lat_deg: f64::INFINITY,
            lng_deg: 1.0,
            accuracy_m: None,
            altitude_m: None,
            speed_ms: None,
            bearing_deg: None,
        };
        assert!(s.send_sensor(&Sensor::Gps(gps)).is_empty());
        assert_eq!(s.mic_format(), (16000, 1));
        assert_eq!(
            s.request_shutdown(1).last(),
            Some(&Out::After(SHUTDOWN_ACK, Timer::ShutdownTimeout))
        );
        let out = s.on_message(ch::CONTROL, ctrl_msg::SHUTDOWN_RESPONSE, &[]);
        assert_eq!(out.last(), Some(&Out::ShutdownDone));
        assert_eq!(s.request_shutdown(1), [Out::ShutdownDone]);
        assert!(s.on_timer(Timer::ShutdownTimeout).is_empty());
    }
}
