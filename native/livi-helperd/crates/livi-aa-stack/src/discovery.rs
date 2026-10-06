use crate::codec::encode;
use crate::config::{AaConfig, Geometry, Insets};
use crate::consts::{
    bt_pairing_method, ch, display_type, media_codec, video_fps, video_resolution,
};
use crate::log::{debug, hex};
use crate::proto::aap_protobuf::service::Service;
use crate::proto::aap_protobuf::service::bluetooth::BluetoothService;
use crate::proto::aap_protobuf::service::control::message::{
    ConnectionConfiguration, HeadUnitInfo, PingConfiguration, ServiceDiscoveryResponse,
};
use crate::proto::aap_protobuf::service::inputsource::InputSourceService;
use crate::proto::aap_protobuf::service::inputsource::input_source_service::TouchScreen;
use crate::proto::aap_protobuf::service::media::shared::message::{
    AudioConfiguration, Insets as UiInsets, UiConfig,
};
use crate::proto::aap_protobuf::service::media::sink::MediaSinkService;
use crate::proto::aap_protobuf::service::media::sink::message::VideoConfiguration;
use crate::proto::aap_protobuf::service::media::source::MediaSourceService;
use crate::proto::aap_protobuf::service::mediaplayback::MediaPlaybackStatusService;
use crate::proto::aap_protobuf::service::navigationstatus::NavigationStatusService;
use crate::proto::aap_protobuf::service::navigationstatus::navigation_status_service::ImageOptions;
use crate::proto::aap_protobuf::service::phonestatus::PhoneStatusService;
use crate::proto::aap_protobuf::service::sensorsource::SensorSourceService;
use crate::proto::aap_protobuf::service::sensorsource::message::Sensor;
use crate::proto::aap_protobuf::service::wifiprojection::WifiProjectionService;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    H265,
    Vp9,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
        }
    }

    pub fn of_media_codec(codec: i32) -> Self {
        match codec {
            media_codec::VIDEO_H265 => Self::H265,
            media_codec::VIDEO_VP9 => Self::Vp9,
            media_codec::VIDEO_AV1 => Self::Av1,
            _ => Self::H264,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Discovery {
    pub buf: Vec<u8>,
    /// Indexed like the offered video configurations.
    pub video_codecs: Vec<VideoCodec>,
    pub cluster_codecs: Vec<VideoCodec>,
}

mod sensor {
    pub const LOCATION: i32 = 1;
    pub const COMPASS: i32 = 2;
    pub const SPEED: i32 = 3;
    pub const RPM: i32 = 4;
    pub const ODOMETER: i32 = 5;
    pub const FUEL: i32 = 6;
    pub const PARKING_BRAKE: i32 = 7;
    pub const GEAR: i32 = 8;
    pub const NIGHT_MODE: i32 = 10;
    pub const ENV_DATA: i32 = 11;
    pub const HVAC: i32 = 12;
    pub const DRIVING_STATUS: i32 = 13;
    pub const DOOR_DATA: i32 = 16;
    pub const LIGHT_DATA: i32 = 17;
    pub const TIRE_PRESSURE_DATA: i32 = 18;
    pub const ACCELEROMETER: i32 = 19;
    pub const GYROSCOPE: i32 = 20;
    pub const GPS_SATELLITE: i32 = 21;
    pub const VEHICLE_ENERGY_MODEL: i32 = 23;
    pub const RAW_VEHICLE_ENERGY_MODEL: i32 = 25;
    pub const RAW_EV_TRIP_SETTINGS: i32 = 26;
}

mod audio_stream {
    pub const GUIDANCE: i32 = 1;
    pub const SYSTEM: i32 = 2;
    pub const MEDIA: i32 = 3;
    #[allow(dead_code)]
    pub const TELEPHONY: i32 = 4;
}

/// Raw GPS plus accelerometer, gyroscope, compass and car speed.
const LOCATION_CHARACTERIZATION: u32 = 256 | 4 | 2 | 8 | 64;

const KEYCODES: [i32; 46] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 66,
    79, 82, 84, 85, 86, 87, 88, 89, 90, 91, 111, 126, 127, 164, 219, 231, 260, 261, 262, 263,
    65536,
];

const SENSORS: [i32; 21] = [
    sensor::DRIVING_STATUS,
    sensor::LOCATION,
    sensor::NIGHT_MODE,
    sensor::SPEED,
    sensor::GEAR,
    sensor::PARKING_BRAKE,
    sensor::FUEL,
    sensor::ODOMETER,
    sensor::ENV_DATA,
    sensor::DOOR_DATA,
    sensor::LIGHT_DATA,
    sensor::TIRE_PRESSURE_DATA,
    sensor::HVAC,
    sensor::ACCELEROMETER,
    sensor::GYROSCOPE,
    sensor::COMPASS,
    sensor::GPS_SATELLITE,
    sensor::RPM,
    sensor::VEHICLE_ENERGY_MODEL,
    sensor::RAW_VEHICLE_ENERGY_MODEL,
    sensor::RAW_EV_TRIP_SETTINGS,
];

fn resolution(width: u32) -> i32 {
    if width >= 3840 {
        5
    } else if width >= 2560 {
        4
    } else if width >= 1920 {
        video_resolution::R1920X1080
    } else if width <= 800 {
        video_resolution::R800X480
    } else {
        video_resolution::R1280X720
    }
}

fn resolution_of(width: u32, height: u32) -> Option<i32> {
    match (width, height) {
        (800, 480) => Some(video_resolution::R800X480),
        (1280, 720) => Some(video_resolution::R1280X720),
        (1920, 1080) => Some(video_resolution::R1920X1080),
        _ => None,
    }
}

fn ui_insets(i: Insets) -> UiInsets {
    UiInsets { top: Some(i.top), bottom: Some(i.bottom), left: Some(i.left), right: Some(i.right) }
}

fn audio(sampling_rate: u32, channels: u32) -> AudioConfiguration {
    AudioConfiguration { sampling_rate, number_of_bits: 16, number_of_channels: channels }
}

fn audio_sink(id: u8, stream: i32, sampling_rate: u32, channels: u32) -> Service {
    Service {
        id: i32::from(id),
        media_sink_service: Some(MediaSinkService {
            available_type: Some(media_codec::AUDIO_PCM),
            audio_type: Some(stream),
            available_while_in_call: Some(true),
            audio_configs: vec![audio(sampling_rate, channels)],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn video_configs(base: &VideoConfiguration, codecs: &[VideoCodec]) -> Vec<VideoConfiguration> {
    codecs
        .iter()
        .map(|c| VideoConfiguration {
            video_codec_type: Some(match c {
                VideoCodec::H264 => media_codec::VIDEO_H264_BP,
                VideoCodec::H265 => media_codec::VIDEO_H265,
                VideoCodec::Vp9 => media_codec::VIDEO_VP9,
                VideoCodec::Av1 => media_codec::VIDEO_AV1,
            }),
            ..*base
        })
        .collect()
}

/// The identity goes in the deprecated fields as well, older phones read it there.
#[allow(deprecated)]
pub fn build(cfg: &AaConfig) -> Discovery {
    let v_w = cfg.video_width.unwrap_or(1280);
    let dpi = cfg.video_dpi.unwrap_or(140);
    let v_res = resolution(v_w);
    let v_fps = if cfg.video_fps.unwrap_or(30) == 60 { video_fps::FPS60 } else { video_fps::FPS30 };
    let main = Geometry::main(cfg);
    let main_content = ui_insets(cfg.main_safe_area);

    let mut channels = Vec::new();

    let base = VideoConfiguration {
        codec_resolution: Some(v_res),
        frame_rate: Some(v_fps),
        width_margin: Some(main.width_margin),
        height_margin: Some(main.height_margin),
        density: Some(dpi),
        pixel_aspect_ratio_e4: Some(cfg.pixel_aspect_ratio_e4.unwrap_or(10000)),
        ui_config: Some(UiConfig {
            margins: Some(ui_insets(main.inset)),
            content_insets: Some(main_content),
            stable_content_insets: Some(main_content),
            ui_theme: None,
        }),
        ..Default::default()
    };
    let mut video_codecs = vec![VideoCodec::H264];
    if cfg.hevc_supported {
        video_codecs.push(VideoCodec::H265);
    }
    if debug() {
        let names: Vec<_> = video_codecs.iter().map(|c| c.name()).collect();
        println!("[Session] advertising codecs: {}", names.join(", "));
    }
    channels.push(Service {
        id: i32::from(ch::VIDEO),
        media_sink_service: Some(MediaSinkService {
            available_type: Some(media_codec::VIDEO_H264_BP),
            available_while_in_call: Some(true),
            video_configs: video_configs(&base, &video_codecs),
            ..Default::default()
        }),
        ..Default::default()
    });

    let mut cluster_codecs = Vec::new();
    if cfg.cluster_enabled {
        let (c_w, c_h) = (cfg.cluster_width, cfg.cluster_height);
        let tier = (cfg.cluster_tier_width.unwrap_or(c_w), cfg.cluster_tier_height.unwrap_or(c_h));
        let cluster_res = resolution_of(tier.0, tier.1).unwrap_or(v_res);
        let cluster_fps = match cfg.cluster_fps {
            60 => video_fps::FPS60,
            30 => video_fps::FPS30,
            _ => v_fps,
        };
        let geometry = Geometry::new(tier, (c_w, c_h), cfg.cluster_view_area);
        let content = ui_insets(cfg.cluster_safe_area);
        let base = VideoConfiguration {
            codec_resolution: Some(cluster_res),
            frame_rate: Some(cluster_fps),
            width_margin: Some(geometry.width_margin),
            height_margin: Some(geometry.height_margin),
            density: Some(cfg.cluster_dpi.unwrap_or(dpi)),
            pixel_aspect_ratio_e4: Some(cfg.cluster_pixel_aspect_ratio_e4.unwrap_or(10000)),
            ui_config: Some(UiConfig {
                margins: Some(ui_insets(geometry.inset)),
                content_insets: Some(content),
                stable_content_insets: Some(content),
                ui_theme: None,
            }),
            ..Default::default()
        };
        cluster_codecs.push(VideoCodec::H264);
        for (on, codec) in [
            (cfg.hevc_supported, VideoCodec::H265),
            (cfg.vp9_supported, VideoCodec::Vp9),
            (cfg.av1_supported, VideoCodec::Av1),
        ] {
            if on {
                cluster_codecs.push(codec);
            }
        }
        channels.push(Service {
            id: i32::from(ch::CLUSTER_VIDEO),
            media_sink_service: Some(MediaSinkService {
                available_type: Some(media_codec::VIDEO_H264_BP),
                available_while_in_call: Some(true),
                video_configs: video_configs(&base, &cluster_codecs),
                display_type: Some(display_type::CLUSTER),
                display_id: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        });
        channels.push(Service {
            id: i32::from(ch::CLUSTER_INPUT),
            input_source_service: Some(InputSourceService {
                display_id: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    if !cfg.disable_audio_output {
        channels.push(audio_sink(ch::MEDIA_AUDIO, audio_stream::MEDIA, 48000, 2));
        channels.push(audio_sink(ch::SPEECH_AUDIO, audio_stream::GUIDANCE, 16000, 1));
    }
    channels.push(audio_sink(ch::SYSTEM_AUDIO, audio_stream::SYSTEM, 16000, 1));
    channels.push(Service {
        id: i32::from(ch::MIC_INPUT),
        media_source_service: Some(MediaSourceService {
            available_type: Some(media_codec::AUDIO_PCM),
            audio_config: Some(audio(16000, 1)),
            available_while_in_call: Some(true),
        }),
        ..Default::default()
    });

    let fuel_types = if cfg.fuel_types.is_empty() { vec![1] } else { cfg.fuel_types.clone() };
    channels.push(Service {
        id: i32::from(ch::SENSOR),
        sensor_source_service: Some(SensorSourceService {
            sensors: SENSORS.iter().map(|t| Sensor { sensor_type: *t }).collect(),
            location_characterization: Some(LOCATION_CHARACTERIZATION),
            supported_fuel_types: fuel_types,
            supported_ev_connector_types: cfg.ev_connector_types.clone(),
        }),
        ..Default::default()
    });

    let (touch_w, touch_h) = main.touch_size();
    channels.push(Service {
        id: i32::from(ch::INPUT),
        input_source_service: Some(InputSourceService {
            keycodes_supported: KEYCODES.to_vec(),
            touchscreen: vec![TouchScreen {
                width: touch_w as i32,
                height: touch_h as i32,
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    });

    channels.push(Service {
        id: i32::from(ch::BLUETOOTH),
        bluetooth_service: Some(BluetoothService {
            car_address: cfg
                .bt_mac_address
                .clone()
                .unwrap_or_else(|| "00:00:00:00:00:00".to_string()),
            supported_pairing_methods: vec![
                bt_pairing_method::PIN,
                bt_pairing_method::NUMERIC_COMPARISON,
            ],
        }),
        ..Default::default()
    });

    channels.push(Service {
        id: i32::from(ch::NAVIGATION),
        navigation_status_service: Some(NavigationStatusService {
            minimum_interval_ms: 500,
            r#type: 1,
            image_options: Some(ImageOptions { width: 256, height: 256, colour_depth_bits: 32 }),
        }),
        ..Default::default()
    });
    channels.push(Service {
        id: i32::from(ch::MEDIA_INFO),
        media_playback_service: Some(MediaPlaybackStatusService {}),
        ..Default::default()
    });
    channels.push(Service {
        id: i32::from(ch::PHONE_STATUS),
        phone_status_service: Some(PhoneStatusService {}),
        ..Default::default()
    });
    if let Some(bssid) = cfg.wifi_bssid.as_ref().filter(|b| !b.is_empty()) {
        channels.push(Service {
            id: i32::from(ch::WIFI),
            wifi_projection_service: Some(WifiProjectionService {
                car_wifi_bssid: Some(bssid.clone()),
            }),
            ..Default::default()
        });
    }

    let make = || Some("LIVI".to_string());
    let model = || Some("Universal".to_string());
    let year = || Some("2026".to_string());
    let vehicle = || Some("livi-001".to_string());
    let hu_model = || Some("LIVI Head Unit".to_string());
    let build_no = || Some("1".to_string());
    let version = || Some("1.0".to_string());
    let channel_count = channels.len();
    let sdr = ServiceDiscoveryResponse {
        channels,
        make: make(),
        model: model(),
        year: year(),
        vehicle_id: vehicle(),
        driver_position: Some(i32::from(cfg.driver_position)),
        head_unit_make: make(),
        head_unit_model: hu_model(),
        head_unit_software_build: build_no(),
        head_unit_software_version: version(),
        can_play_native_media_during_vr: Some(true),
        session_configuration: None,
        display_name: Some(cfg.hu_name.clone().unwrap_or_else(|| "LIVI".to_string())),
        probe_for_support: Some(false),
        connection_configuration: Some(ConnectionConfiguration {
            ping_configuration: Some(PingConfiguration {
                timeout_ms: Some(5000),
                interval_ms: Some(1500),
                high_latency_threshold_ms: Some(500),
                tracked_ping_count: Some(5),
            }),
            wireless_tcp_configuration: None,
        }),
        headunit_info: Some(HeadUnitInfo {
            make: make(),
            model: model(),
            year: year(),
            vehicle_id: vehicle(),
            head_unit_make: make(),
            head_unit_model: hu_model(),
            head_unit_software_build: build_no(),
            head_unit_software_version: version(),
        }),
    };
    let buf = encode(&sdr);
    if debug() {
        println!("[Session] SDR: {channel_count} channels, {}B", buf.len());
        let shown = &buf[..buf.len().min(64)];
        println!("[Session] SDR hex: {}{}", hex(shown), if buf.len() > 64 { "..." } else { "" });
    }
    Discovery { buf, video_codecs, cluster_codecs }
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    fn decoded(cfg: &AaConfig) -> ServiceDiscoveryResponse {
        ServiceDiscoveryResponse::decode(build(cfg).buf.as_slice()).unwrap()
    }

    fn ids(sdr: &ServiceDiscoveryResponse) -> Vec<i32> {
        sdr.channels.iter().map(|c| c.id).collect()
    }

    #[test]
    fn the_default_offer() {
        let d = build(&AaConfig::default());
        assert_eq!(d.video_codecs, [VideoCodec::H264]);
        assert!(d.cluster_codecs.is_empty());
        let sdr = decoded(&AaConfig::default());
        assert_eq!(ids(&sdr), [3, 4, 5, 6, 9, 1, 8, 10, 12, 13, 14]);
        assert_eq!(sdr.display_name.as_deref(), Some("LIVI"));
        let bt = sdr.channels[7].bluetooth_service.as_ref().unwrap();
        assert_eq!(bt.car_address, "00:00:00:00:00:00");
        let touch = &sdr.channels[6].input_source_service.as_ref().unwrap().touchscreen[0];
        assert_eq!((touch.width, touch.height), (1280, 720));
    }

    #[test]
    fn codecs_cluster_and_audio_follow_the_config() {
        let cfg = AaConfig {
            hevc_supported: true,
            vp9_supported: true,
            av1_supported: true,
            cluster_enabled: true,
            cluster_width: 800,
            cluster_height: 400,
            cluster_fps: 60,
            disable_audio_output: true,
            wifi_bssid: Some("11:22:33:44:55:66".into()),
            ..Default::default()
        };
        let d = build(&cfg);
        assert_eq!(d.video_codecs, [VideoCodec::H264, VideoCodec::H265]);
        assert_eq!(
            d.cluster_codecs,
            [VideoCodec::H264, VideoCodec::H265, VideoCodec::Vp9, VideoCodec::Av1]
        );
        let sdr = decoded(&cfg);
        assert_eq!(ids(&sdr), [3, 19, 20, 6, 9, 1, 8, 10, 12, 13, 14, 18]);
        let cluster = sdr.channels[1].media_sink_service.as_ref().unwrap();
        assert_eq!(cluster.video_configs[0].codec_resolution, Some(video_resolution::R1280X720));
        assert_eq!(cluster.video_configs[0].height_margin, Some(0));
        assert_eq!(cluster.video_configs[0].width_margin, Some(0));
        assert_eq!(cluster.video_configs[0].frame_rate, Some(video_fps::FPS60));
        let tiered =
            AaConfig { cluster_tier_width: Some(800), cluster_tier_height: Some(480), ..cfg };
        let sdr = decoded(&tiered);
        let cluster = sdr.channels[1].media_sink_service.as_ref().unwrap();
        assert_eq!(cluster.video_configs[0].codec_resolution, Some(video_resolution::R800X480));
        assert_eq!(cluster.video_configs[0].height_margin, Some(80));
        assert_eq!(VideoCodec::of_media_codec(media_codec::VIDEO_AV1), VideoCodec::Av1);
        assert_eq!(VideoCodec::of_media_codec(0), VideoCodec::H264);
        assert_eq!(resolution(3840), 5);
        assert_eq!(resolution(2560), 4);
    }
}
