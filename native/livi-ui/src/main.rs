//! LIVI-Lite native UI.
//!
//! Replaces the Electron renderer with a small Slint surface that talks to
//! livi-core over its framed-JSON Unix socket. Projection video is a compositor
//! plane above this surface; while it is visible the UI still gets all touch
//! input and hands it to core as normalized points.

mod client;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use client::{Client, CoreEvent, livi_log};
use livi_core_proto::input::{Input, Phase, Point};
use livi_core_proto::message::{Action, MediaControl};
use livi_core_proto::state::{Front, Screen};
use serde_json::{Map, Value, json};
use slint::{Image, Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

// CommandMapping ids the media page buttons use.
const CMD_PLAY_PAUSE: i32 = 203;
const CMD_NEXT: i32 = 204;
const CMD_PREV: i32 = 205;

// Edge-reveal gesture: swipe up from the bottom or right from the left.
const EDGE_ZONE: f32 = 60.0;
const EDGE_DEAD_ZONE: f32 = 20.0;
const EDGE_MAX_MS: u128 = 350;

const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Default)]
struct TouchState {
    active: bool,
    start_x: f32,
    start_y: f32,
    last_x: f32,
    last_y: f32,
    start_time: Option<Instant>,
    pending_edge: bool,
    forwarded: bool,
    canceled: bool,
}

struct Ctx {
    client: Client,
    config: Map<String, Value>,
    touch: TouchState,
    back_page: i32,
    audio_out_ids: Vec<String>,
    audio_in_ids: Vec<String>,
    save_tx: Sender<(String, Value)>,
}

impl Ctx {
    fn new(client: Client) -> Arc<Mutex<Self>> {
        let (save_tx, save_rx) = mpsc::channel::<(String, Value)>();
        let save_client = client.clone();
        std::thread::Builder::new()
            .name("settings-save".to_string())
            .spawn(move || {
                while let Ok((key, value)) = save_rx.recv() {
                    let mut patch = Map::new();
                    patch.insert(key, value);
                    while let Ok((key, value)) = save_rx.recv_timeout(SAVE_DEBOUNCE) {
                        patch.insert(key, value);
                    }
                    let patch = Value::Object(patch);
                    livi_log!("setConfig {patch}");
                    save_client.act(Action::SetConfig { patch });
                }
            })
            .expect("failed to spawn settings thread");

        Arc::new(Mutex::new(Self {
            client,
            config: Map::new(),
            touch: TouchState::default(),
            back_page: 0,
            audio_out_ids: Vec::new(),
            audio_in_ids: Vec::new(),
            save_tx,
        }))
    }
}

fn main() {
    if std::env::var_os("SLINT_BACKEND").is_none() {
        // Software renderer: no GL context, fast startup on the nested Wayland socket.
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }

    let ui = MainWindow::new().expect("failed to create the Slint window");
    run_ui(ui);
}

fn run_ui(ui: MainWindow) {
    let client = Client::new();
    let ctx = Ctx::new(client.clone());

    let (event_tx, event_rx) = mpsc::channel::<CoreEvent>();
    client::spawn(client.clone(), event_tx);
    {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("core-events".to_string())
            .spawn(move || {
                while let Ok(event) = event_rx.recv() {
                    let weak = weak.clone();
                    let ctx = ctx.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            handle_core_event(&ui, &ctx, event);
                        }
                    });
                }
            })
            .expect("failed to spawn core events thread");
    }

    wire_callbacks(&ui, &ctx);

    let _clock = start_clock(&ui);
    let _breathe = start_breathe(&ui);

    // Core populates the link speed when asked; re-asserted after every reconnect.
    client.link_speed(true);

    // With nothing projecting yet, open on the shell instead of the (empty)
    // projection surface. Core's front changes switch pages from there.
    ui.global::<App>().set_page(1);

    livi_log!("starting Slint event loop");
    if let Err(e) = ui.run() {
        livi_log!("event loop error: {e}");
    }
    livi_log!("UI exited");
}

fn handle_core_event(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, event: CoreEvent) {
    let app = ui.global::<App>();
    match event {
        CoreEvent::Connected(connected) => {
            app.set_core_connected(connected);
            if connected {
                app.set_status_message("core connected".into());
            } else {
                app.set_status_message("waiting for core".into());
                app.set_streaming(false);
                app.set_projection_active(false);
            }
        }
        CoreEvent::Welcome { version, state, .. } => {
            app.set_version_text(version.into());
            apply_state(ui, ctx, &state);
            report_front(ui, ctx);
        }
        CoreEvent::Patch { state, .. } => apply_state(ui, ctx, &state),
        CoreEvent::Refused(reason) => {
            app.set_status_message(format!("core refused: {reason}").into());
        }
    }
}

// ---------------------------------------------------------------- state

fn apply_state(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, state: &Value) {
    let app = ui.global::<App>();
    if let Some(config) = state.get("config") {
        apply_settings(ui, ctx, config);
    }
    if let Some(devices) = state.get("devices") {
        set_devices(ui, devices);
    }
    if let Some(now_playing) = state.get("nowPlaying") {
        apply_now_playing(ui, now_playing);
    }
    if let Some(sessions) = state.get("sessions") {
        apply_sessions(ui, sessions);
    }
    if let Some(system) = state.get("system") {
        apply_system(ui, ctx, system);
    }
    if let Some(telemetry) = state.get("telemetry") {
        apply_telemetry(ui, telemetry);
    }
    if let Some(navigation) = state.get("navigation") {
        apply_navigation(ui, navigation);
    }
    if let Some(update) = state.get("update") {
        apply_update(ui, update);
    }
    if let Some(front) = state.pointer("/front/main").and_then(Value::as_str) {
        apply_front(ui, ctx, front);
    }
    if let Some(dash) = state.pointer("/front/dash").and_then(Value::as_str) {
        app.set_cluster_on(dash == "cluster");
    }
    sync_indexes(ui, ctx);
}

/// What core put in front on the main screen decides the page. Its own moves
/// (a call, Siri, the phone handing the screen back) are followed, and the
/// user's page is remembered for when the projection leaves again.
fn apply_front(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, front: &str) {
    let app = ui.global::<App>();
    let projection = front == "projection";
    app.set_projection_shown(projection);
    app.set_streaming(projection);
    if projection {
        if app.get_page() != 0 {
            let mut ctx = ctx.lock().unwrap();
            if ctx.back_page == 0 {
                ctx.back_page = app.get_page();
            }
            drop(ctx);
            app.set_page(0);
        }
    } else if app.get_page() == 0 {
        let back = ctx.lock().unwrap().back_page;
        app.set_page(if back > 0 { back } else { 1 });
    }
}

fn apply_sessions(ui: &MainWindow, sessions: &Value) {
    let app = ui.global::<App>();
    let active = sessions.get("active").and_then(Value::as_str);
    let protocol = match active {
        Some("carplay") => "carplay",
        Some("androidauto") => "androidauto",
        _ => "none",
    };
    app.set_protocol(protocol.into());
    app.set_projection_active(active.is_some());
    if app.get_core_connected() {
        app.set_status_message(if active.is_some() { "phone connected" } else { "ready" }.into());
    }
}

fn apply_telemetry(ui: &MainWindow, telemetry: &Value) {
    let app = ui.global::<App>();
    let speed = telemetry.get("speedKph").and_then(Value::as_f64);
    app.set_speed_text(match speed {
        Some(speed) => format!("{speed:.0} km/h").into(),
        None => "".into(),
    });
}

fn apply_navigation(ui: &MainWindow, navigation: &Value) {
    let app = ui.global::<App>();
    let destination = navigation.get("destinationName").and_then(Value::as_str);
    app.set_nav_text(match destination {
        Some(destination) => format!("→ {destination}").into(),
        None => "".into(),
    });
}

fn apply_update(ui: &MainWindow, update: &Value) {
    let app = ui.global::<App>();
    let phase = update.get("phase").and_then(Value::as_str).unwrap_or("idle");
    let error = update.get("error").and_then(Value::as_str);
    let checking = update.get("checking").and_then(Value::as_bool).unwrap_or(false);
    let latest = update.pointer("/latest/version").and_then(Value::as_str);
    let received = update.get("received").and_then(Value::as_f64).unwrap_or(0.0);
    let total = update.get("total").and_then(Value::as_f64).unwrap_or(0.0);

    let text = if let Some(error) = error {
        error.to_string()
    } else if checking {
        "checking for updates…".to_string()
    } else {
        match phase {
            "download" if total > 0.0 => format!("downloading {:.0}%", received / total * 100.0),
            "download" => "downloading update".to_string(),
            "ready" => "update ready".to_string(),
            "installing" => "installing update…".to_string(),
            "relaunching" => "restarting…".to_string(),
            "error" => "update failed".to_string(),
            _ => match latest {
                Some(version) => format!("v{version} available"),
                None => String::new(),
            },
        }
    };
    app.set_update_text(text.into());
}

// ---------------------------------------------------------------- settings

fn get_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_string)
}

fn get_bool(map: &Map<String, Value>, key: &str) -> Option<bool> {
    map.get(key).and_then(Value::as_bool)
}

fn get_f32(map: &Map<String, Value>, key: &str) -> Option<f32> {
    map.get(key).and_then(Value::as_f64).map(|v| v as f32)
}

fn apply_settings(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, settings: &Value) {
    let Some(map) = settings.as_object().cloned() else {
        return;
    };
    let app = ui.global::<App>();

    if let Some(v) = get_str(&map, "carName") {
        app.set_car_name(v.into());
    }
    if let Some(v) = get_bool(&map, "autoConn") {
        app.set_auto_conn(v);
    }
    if let Some(v) = get_bool(&map, "wirelessCpEnabled") {
        app.set_wireless_cp_enabled(v);
    }
    if let Some(v) = get_bool(&map, "wirelessAaEnabled") {
        app.set_wireless_aa_enabled(v);
    }
    if let Some(v) = get_str(&map, "wifiPassword") {
        app.set_wifi_password(v.into());
    }
    if let Some(v) = map.get("wifiChannel").and_then(Value::as_i64) {
        app.set_wifi_channel(v as i32);
    }
    if let Some(v) = get_str(&map, "country") {
        app.set_country(v.into());
    }
    if let Some(v) = get_str(&map, "wifiInterface") {
        app.set_wifi_interface(v.into());
    }
    if let Some(v) = get_str(&map, "btAdapter") {
        app.set_bt_adapter(v.into());
    }
    if let Some(v) = get_bool(&map, "darkMode") {
        app.set_dark_mode(v);
    }
    if let Some(v) = get_f32(&map, "displayBrightness") {
        app.set_display_brightness(v.clamp(0.0, 1.0));
    }
    if let Some(v) = get_f32(&map, "huVolume") {
        app.set_hu_volume(v.clamp(0.0, 1.0));
    }
    if let Some(v) = get_f32(&map, "audioVolume") {
        app.set_audio_volume(v.clamp(0.0, 1.0));
    }
    if let Some(v) = get_f32(&map, "navVolume") {
        app.set_nav_volume(v.clamp(0.0, 1.0));
    }
    if let Some(v) = get_f32(&map, "voiceAssistantVolume") {
        app.set_voice_assistant_volume(v.clamp(0.0, 1.0));
    }
    if let Some(v) = get_f32(&map, "callVolume") {
        app.set_call_volume(v.clamp(0.0, 1.0));
    }
    if let Some(v) = get_str(&map, "audioOutputDevice") {
        app.set_audio_output_device(v.into());
    }
    if let Some(v) = get_str(&map, "audioInputDevice") {
        app.set_audio_input_device(v.into());
    }
    if let Some(v) = get_str(&map, "language") {
        app.set_language(v.into());
    }
    if let Some(v) = get_bool(&map, "debugLogging") {
        app.set_debug_logging(v);
    }
    if let Some(v) = get_str(&map, "displayMode") {
        app.set_display_mode(v.into());
    }

    ctx.lock().unwrap().config = map;
    app.set_settings_loaded(true);
}

fn apply_system(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, system: &Value) {
    let app = ui.global::<App>();

    if let Some(list) = system.get("wifiInterfaces") {
        app.set_wifi_interface_options(string_model(string_list(list)));
    }
    if let Some(list) = system.get("btAdapters") {
        app.set_bt_adapter_options(string_model(string_list(list)));
    }
    if let Some(list) = system.get("wifiChannels") {
        app.set_wifi_channel_options(string_model(number_list(list)));
    }
    if let Some(list) = system.get("wifiCountries") {
        app.set_country_options(string_model(string_list(list)));
    }
    if let Some(list) = system.get("displayModes") {
        app.set_display_mode_options(string_model(string_list(list)));
    }
    if let Some(list) = system.get("audioSinks") {
        let (labels, ids) = audio_options(list);
        ctx.lock().unwrap().audio_out_ids = ids;
        app.set_audio_output_options(string_model(labels));
    }
    if let Some(list) = system.get("audioSources") {
        let (labels, ids) = audio_options(list);
        ctx.lock().unwrap().audio_in_ids = ids;
        app.set_audio_input_options(string_model(labels));
    }

    match system.get("linkSpeed") {
        Some(Value::Object(link)) => {
            let down = link.get("downMbps").and_then(Value::as_f64).unwrap_or(0.0);
            let up = link.get("upMbps").and_then(Value::as_f64).unwrap_or(0.0);
            app.set_link_text(format!("{down:.1} / {up:.1} Mbps").into());
        }
        _ => app.set_link_text("".into()),
    }
}

fn sync_indexes(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let app = ui.global::<App>();
    let (config, out_ids, in_ids) = {
        let ctx = ctx.lock().unwrap();
        (ctx.config.clone(), ctx.audio_out_ids.clone(), ctx.audio_in_ids.clone())
    };

    let value = |key: &str| get_str(&config, key).unwrap_or_default();
    app.set_wifi_interface_index(index_of(
        &app.get_wifi_interface_options(),
        &value("wifiInterface"),
    ));
    app.set_bt_adapter_index(index_of(&app.get_bt_adapter_options(), &value("btAdapter")));
    app.set_country_index(index_of(&app.get_country_options(), &value("country")));
    app.set_language_index(index_of(&app.get_language_options(), &value("language")));
    app.set_display_mode_index(index_of(&app.get_display_mode_options(), &value("displayMode")));

    let channel = config
        .get("wifiChannel")
        .and_then(Value::as_i64)
        .map(|v| v.to_string())
        .unwrap_or_default();
    app.set_wifi_channel_index(index_of(&app.get_wifi_channel_options(), &channel));

    let out_id = value("audioOutputDevice");
    app.set_audio_output_index(
        out_ids.iter().position(|id| *id == out_id).map(|i| i as i32).unwrap_or(-1),
    );
    let in_id = value("audioInputDevice");
    app.set_audio_input_index(
        in_ids.iter().position(|id| *id == in_id).map(|i| i as i32).unwrap_or(-1),
    );
}

fn index_of(model: &ModelRc<SharedString>, value: &str) -> i32 {
    if value.is_empty() {
        return -1;
    }
    model.iter().position(|item| item.as_str() == value).map(|i| i as i32).unwrap_or(-1)
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

fn number_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|item| match item {
                    Value::Number(n) => Some(n.to_string()),
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn audio_options(value: &Value) -> (Vec<String>, Vec<String>) {
    let mut labels = Vec::new();
    let mut ids = Vec::new();
    if let Some(list) = value.as_array() {
        for device in list {
            let id = device.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            let name = device.get("name").and_then(Value::as_str).unwrap_or(&id).to_string();
            let offline = device.get("offline").and_then(Value::as_bool).unwrap_or(false);
            let default = device.get("isDefault").and_then(Value::as_bool).unwrap_or(false);
            let mut label = if default { format!("{name} (default)") } else { name };
            if offline {
                label.push_str(" (offline)");
            }
            labels.push(label);
            ids.push(id);
        }
    }
    (labels, ids)
}

fn string_model(items: Vec<String>) -> ModelRc<SharedString> {
    let values: Vec<SharedString> = items.into_iter().map(SharedString::from).collect();
    ModelRc::new(VecModel::from(values))
}

// ---------------------------------------------------------------- devices

fn set_devices(ui: &MainWindow, value: &Value) {
    let rows = device_rows(value);
    let app = ui.global::<App>();
    app.set_devices_status(format!("{} device(s)", rows.len()).into());
    app.set_devices(ModelRc::new(VecModel::from(rows)));
}

fn device_rows(value: &Value) -> Vec<DeviceRow> {
    let mut rows = Vec::new();
    let Some(list) = value.as_array() else {
        return rows;
    };
    for device in list {
        let id = device.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let name = device
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| device.get("model").and_then(Value::as_str))
            .unwrap_or(id);
        let protocol = device.get("protocol").and_then(Value::as_str).unwrap_or("");
        let protocol_label = match protocol {
            "carplay" => "CarPlay",
            "androidauto" => "Android Auto",
            _ => "Unknown",
        };
        let transport = device.get("lastTransport").and_then(Value::as_str).unwrap_or("");
        let status = device.get("status").and_then(Value::as_str).unwrap_or("offline");
        let detail = format!("{protocol_label} · {transport} · {status}");

        let charging = device.get("batteryCharging").and_then(Value::as_bool).unwrap_or(false);
        let battery_level =
            device.get("batteryLevel").and_then(Value::as_f64).map(|v| v as f32).unwrap_or(-1.0);
        let signal =
            device.get("signalStrength").and_then(Value::as_f64).map(|v| v as f32).unwrap_or(-1.0);

        let looks_like_mac = id.matches(':').count() == 5;
        rows.push(DeviceRow {
            id: id.into(),
            name: name.into(),
            detail: detail.into(),
            status: status.into(),
            transport: transport.into(),
            protocol: protocol.into(),
            battery_level,
            battery_charging: charging,
            signal,
            can_connect: status != "active" && looks_like_mac,
            can_forget: true,
        });
    }
    rows
}

// ---------------------------------------------------------------- media

fn apply_now_playing(ui: &MainWindow, now_playing: &Value) {
    if now_playing.is_null() {
        clear_media(ui);
        return;
    }
    let app = ui.global::<App>();
    let title = now_playing.get("title").and_then(Value::as_str).unwrap_or("");
    let artist = now_playing.get("artist").and_then(Value::as_str).unwrap_or("");
    let playing = now_playing.get("playing").and_then(Value::as_bool);
    if title.is_empty() && artist.is_empty() && playing.is_none() {
        clear_media(ui);
        return;
    }

    let mut state = app.get_media();
    state.available = true;
    state.title = title.into();
    state.artist = artist.into();
    state.album = now_playing.get("album").and_then(Value::as_str).unwrap_or("").into();
    state.app = now_playing.get("app").and_then(Value::as_str).unwrap_or("").into();
    state.state = match playing {
        Some(true) => "Playing",
        Some(false) => "Paused",
        None => "",
    }
    .into();

    let duration = now_playing.get("durationMs").and_then(Value::as_f64).unwrap_or(0.0);
    let position = now_playing.get("elapsedMs").and_then(Value::as_f64).unwrap_or(0.0);
    state.duration = format_time(duration).into();
    state.elapsed = format_time(position).into();
    if duration > 0.0 {
        state.progress = (position / duration).clamp(0.0, 1.0) as f32;
        state.remaining = format!("-{}", format_time((duration - position).max(0.0))).into();
    }
    app.set_media(state);

    if let Some(artwork) = now_playing.get("artwork").and_then(Value::as_str) {
        if !artwork.is_empty() {
            set_artwork(ui, artwork);
        }
    }
}

fn clear_media(ui: &MainWindow) {
    let app = ui.global::<App>();
    let mut media = app.get_media();
    media.available = false;
    media.title = "".into();
    media.artist = "".into();
    media.album = "".into();
    media.app = "".into();
    media.state = "".into();
    media.elapsed = "".into();
    media.duration = "".into();
    media.remaining = "".into();
    media.progress = 0.0;
    app.set_media(media);
    app.set_has_artwork(false);
    app.set_artwork(Image::default());
}

fn format_time(ms: f64) -> String {
    if !ms.is_finite() || ms <= 0.0 {
        return "0:00".to_string();
    }
    let total = (ms / 1000.0) as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

fn set_artwork(ui: &MainWindow, encoded: &str) {
    use base64::Engine;
    let raw = encoded.split_once("base64,").map(|(_, rest)| rest).unwrap_or(encoded);
    let bytes = match base64::engine::general_purpose::STANDARD.decode(raw.trim()) {
        Ok(bytes) => bytes,
        Err(e) => {
            livi_log!("artwork base64 decode failed: {e}");
            return;
        }
    };
    match image::load_from_memory(&bytes) {
        Ok(decoded) => {
            let rgba = decoded.to_rgba8();
            let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
            );
            let app = ui.global::<App>();
            app.set_artwork(Image::from_rgba8(buffer));
            app.set_has_artwork(true);
        }
        Err(e) => livi_log!("artwork decode failed: {e}"),
    }
}

// ---------------------------------------------------------------- actions

fn wire_callbacks(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let app = ui.global::<App>();

    app.on_nav({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |page| {
            if let Some(ui) = weak.upgrade() {
                let app = ui.global::<App>();
                app.set_page(page);
                let ctx = ctx.lock().unwrap();
                ctx.client.path(page_path(page));
                ctx.client.act(Action::Show { screen: Screen::Main, front: Front::Livi });
            }
        }
    });

    app.on_home({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                let app = ui.global::<App>();
                // With no phone the core never showed the video plane, so jumping
                // to the projection surface would trap the user on a black page.
                if !app.get_projection_active() {
                    app.set_page(1);
                    let ctx = ctx.lock().unwrap();
                    ctx.client.path(page_path(1));
                    return;
                }
                app.set_page(0);
                let ctx = ctx.lock().unwrap();
                ctx.client.act(Action::Show { screen: Screen::Main, front: Front::Projection });
                ctx.client.path("/");
                send_key(&ctx, "home");
            }
        }
    });

    app.on_pointer({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |kind, x, y, w, h| {
            if let Some(ui) = weak.upgrade() {
                on_pointer(&ui, &ctx, kind, x, y, w, h);
            }
        }
    });

    app.on_select_device({
        let ctx = ctx.clone();
        move |id| {
            let ctx = ctx.lock().unwrap();
            livi_log!("select device {id}");
            ctx.client.act(Action::SelectDevice { id: id.to_string() });
        }
    });

    app.on_forget_device({
        let ctx = ctx.clone();
        move |id| {
            let ctx = ctx.lock().unwrap();
            livi_log!("forget device {id}");
            ctx.client.act(Action::ForgetDevice { id: id.to_string() });
        }
    });

    app.on_connect_device({
        let ctx = ctx.clone();
        move |id| {
            let ctx = ctx.lock().unwrap();
            livi_log!("connect device {id}");
            ctx.client.act(Action::ConnectDevice { id: id.to_string() });
        }
    });

    // The old transport switch is what selecting the paired device does now.
    app.on_switch_transport({
        let ctx = ctx.clone();
        move |id| {
            let ctx = ctx.lock().unwrap();
            ctx.client.act(Action::ConnectDevice { id: id.to_string() });
        }
    });

    app.on_refresh_devices({
        let ctx = ctx.clone();
        move || {
            let ctx = ctx.lock().unwrap();
            ctx.client.resync();
        }
    });

    app.on_cycle_session({
        let ctx = ctx.clone();
        move || {
            let ctx = ctx.lock().unwrap();
            ctx.client.act(Action::NextDevice);
        }
    });

    app.on_cluster_request({
        let ctx = ctx.clone();
        move |enabled| {
            let ctx = ctx.lock().unwrap();
            ctx.client.act(Action::Show {
                screen: Screen::Dash,
                front: if enabled { Front::Cluster } else { Front::Livi },
            });
        }
    });

    app.on_media_command({
        let ctx = ctx.clone();
        move |cmd| {
            let control = match cmd {
                CMD_PLAY_PAUSE => Some(MediaControl::PlayPause),
                CMD_NEXT => Some(MediaControl::Next),
                CMD_PREV => Some(MediaControl::Prev),
                _ => None,
            };
            if let Some(control) = control {
                let ctx = ctx.lock().unwrap();
                ctx.client.act(Action::Media { control });
            }
        }
    });

    app.on_set_string({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |key, value| {
            let key = key.to_string();
            let value = value.to_string();
            match key.as_str() {
                "wifiChannel" => {
                    if let Ok(number) = value.parse::<i64>() {
                        queue_setting(&ctx, key, json!(number));
                    }
                }
                "audioOutputDevice" | "audioInputDevice" => {
                    if let Some(ui) = weak.upgrade() {
                        let app = ui.global::<App>();
                        let (options, ids, label_key) = if key == "audioOutputDevice" {
                            (
                                app.get_audio_output_options(),
                                ctx.lock().unwrap().audio_out_ids.clone(),
                                "audioOutputDeviceLabel",
                            )
                        } else {
                            (
                                app.get_audio_input_options(),
                                ctx.lock().unwrap().audio_in_ids.clone(),
                                "audioInputDeviceLabel",
                            )
                        };
                        if let Some(index) =
                            options.iter().position(|s| s.as_str() == value.as_str())
                        {
                            if let Some(id) = ids.get(index) {
                                queue_setting(&ctx, key, Value::String(id.clone()));
                                queue_setting(
                                    &ctx,
                                    label_key.to_string(),
                                    Value::String(value.clone()),
                                );
                                return;
                            }
                        }
                        queue_setting(&ctx, key, Value::String(value));
                    }
                }
                _ => queue_setting(&ctx, key, Value::String(value)),
            }
        }
    });

    app.on_set_number({
        let ctx = ctx.clone();
        move |key, value| {
            queue_setting(&ctx, key.to_string(), json!(value));
        }
    });

    app.on_set_bool({
        let ctx = ctx.clone();
        move |key, value| {
            queue_setting(&ctx, key.to_string(), json!(value));
        }
    });

    app.on_refresh_settings({
        let ctx = ctx.clone();
        move || {
            let ctx = ctx.lock().unwrap();
            ctx.client.resync();
        }
    });

    app.on_restart_app({
        let ctx = ctx.clone();
        move || {
            livi_log!("restart requested");
            ctx.lock().unwrap().client.act(Action::Restart);
        }
    });

    app.on_quit_app({
        let ctx = ctx.clone();
        move || {
            livi_log!("quit requested");
            ctx.lock().unwrap().client.act(Action::Quit);
        }
    });
}

fn page_path(page: i32) -> &'static str {
    match page {
        1 => "/devices",
        2 => "/media",
        3 => "/settings",
        _ => "/",
    }
}

fn queue_setting(ctx: &Arc<Mutex<Ctx>>, key: String, value: Value) {
    if let Ok(ctx) = ctx.lock() {
        let _ = ctx.save_tx.send((key, value));
    }
}

/// Sends the code the bindings map to `name` as a press and release, so core
/// runs it like a physical key.
fn send_key(ctx: &Ctx, name: &str) {
    let code = ctx
        .config
        .get("bindings")
        .and_then(|bindings| bindings.get(name))
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty());
    if let Some(code) = code {
        ctx.client.input(Input::Key { code: code.to_string(), down: true });
        ctx.client.input(Input::Key { code: code.to_string(), down: false });
    }
}

/// Re-asserts what core should have in front for the page the UI is on, after
/// a reconnect or a resync.
fn report_front(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let front = if ui.global::<App>().get_page() == 0 { Front::Projection } else { Front::Livi };
    let ctx = ctx.lock().unwrap();
    ctx.client.act(Action::Show { screen: Screen::Main, front });
    ctx.client.path(page_path(ui.global::<App>().get_page()));
}

// ---------------------------------------------------------------- touch

fn send_pointer(ctx: &Ctx, x: f32, y: f32, w: f32, h: f32, phase: Phase) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let point = Point {
        id: 0,
        x: (x / w).clamp(0.0, 1.0) as f64,
        y: (y / h).clamp(0.0, 1.0) as f64,
        phase,
    };
    ctx.client.input(Input::Pointer { screen: Screen::Main, points: vec![point] });
}

/// Hides the video plane and switches to the shell.
fn reveal_shell(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    livi_log!("edge swipe -> showing shell");
    ui.global::<App>().set_page(1);
    let ctx = ctx.lock().unwrap();
    ctx.client.act(Action::Show { screen: Screen::Main, front: Front::Livi });
    ctx.client.path(page_path(1));
}

fn on_pointer(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, kind: i32, x: f32, y: f32, w: f32, h: f32) {
    let ctx_arc = ctx.clone();
    let mut guard = ctx.lock().unwrap();
    let now = Instant::now();

    match kind {
        0 => {
            // Down.
            let edge = y > h - EDGE_ZONE || x < EDGE_ZONE;
            guard.touch = TouchState {
                active: true,
                start_x: x,
                start_y: y,
                last_x: x,
                last_y: y,
                start_time: Some(now),
                pending_edge: edge,
                forwarded: false,
                canceled: false,
            };
            if !edge {
                send_pointer(&guard, x, y, w, h, Phase::Down);
                guard.touch.forwarded = true;
            }
        }
        2 => {
            // Move.
            if !guard.touch.active || guard.touch.canceled {
                return;
            }
            guard.touch.last_x = x;
            guard.touch.last_y = y;

            if guard.touch.pending_edge && !guard.touch.forwarded {
                let dx = x - guard.touch.start_x;
                let dy = y - guard.touch.start_y;
                let elapsed =
                    guard.touch.start_time.map(|t| now.duration_since(t).as_millis()).unwrap_or(0);
                let swipe_up = dy < -EDGE_DEAD_ZONE && dy.abs() > dx.abs();
                let swipe_right = dx > EDGE_DEAD_ZONE && dx.abs() > dy.abs();

                if elapsed <= EDGE_MAX_MS && (swipe_up || swipe_right) {
                    // Edge reveal: nobody gets this touch.
                    guard.touch.canceled = true;
                    guard.touch.active = false;
                    drop(guard);
                    reveal_shell(ui, &ctx_arc);
                    return;
                }

                let distance = (dx * dx + dy * dy).sqrt();
                if distance > EDGE_DEAD_ZONE || elapsed > EDGE_MAX_MS {
                    // It is a normal touch that happened to start near the edge.
                    send_pointer(
                        &guard,
                        guard.touch.start_x,
                        guard.touch.start_y,
                        w,
                        h,
                        Phase::Down,
                    );
                    guard.touch.forwarded = true;
                    send_pointer(&guard, x, y, w, h, Phase::Move);
                }
                return;
            }

            if guard.touch.forwarded {
                send_pointer(&guard, x, y, w, h, Phase::Move);
            }
        }
        1 => {
            // Up.
            if guard.touch.pending_edge && !guard.touch.forwarded && !guard.touch.canceled {
                // The window manager may coalesce moves; catch the swipe here too.
                let dx = x - guard.touch.start_x;
                let dy = y - guard.touch.start_y;
                let elapsed =
                    guard.touch.start_time.map(|t| now.duration_since(t).as_millis()).unwrap_or(0);
                let swipe_up = dy < -EDGE_DEAD_ZONE && dy.abs() > dx.abs();
                let swipe_right = dx > EDGE_DEAD_ZONE && dx.abs() > dy.abs();
                if elapsed <= EDGE_MAX_MS && (swipe_up || swipe_right) {
                    guard.touch.canceled = true;
                    guard.touch.active = false;
                    drop(guard);
                    reveal_shell(ui, &ctx_arc);
                    return;
                }

                // A tap in the edge zone: deliver it after the fact.
                send_pointer(&guard, guard.touch.start_x, guard.touch.start_y, w, h, Phase::Down);
                send_pointer(&guard, x, y, w, h, Phase::Up);
            } else if guard.touch.forwarded && !guard.touch.canceled {
                send_pointer(&guard, x, y, w, h, Phase::Up);
            }
            guard.touch = TouchState::default();
        }
        _ => {
            // Cancel.
            if guard.touch.forwarded && !guard.touch.canceled {
                send_pointer(&guard, guard.touch.last_x, guard.touch.last_y, w, h, Phase::Cancel);
            }
            guard.touch = TouchState::default();
        }
    }
}

// ---------------------------------------------------------------- chrome

/// Nav-rail clock (`e.g. 14:05`), ticked every second.
fn start_clock(ui: &MainWindow) -> slint::Timer {
    let weak = ui.as_weak();
    let tick = move || {
        if let Some(ui) = weak.upgrade() {
            ui.global::<App>().set_time_text(clock_text().into());
        }
    };
    tick();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), tick);
    timer
}

fn clock_text() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// CarPlay-style LED breathe, ported from initUiBreatheClock(): 1600ms wave
/// between 0.18 and 1.0, updated at ~24fps.
fn start_breathe(ui: &MainWindow) -> slint::Timer {
    let weak = ui.as_weak();
    let start = Instant::now();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(42), move || {
        if let Some(ui) = weak.upgrade() {
            let t = (start.elapsed().as_secs_f32() % 1.6) / 1.6;
            let wave = if t < 0.35 {
                t / 0.35
            } else if t < 0.5 {
                1.0
            } else if t < 0.85 {
                1.0 - (t - 0.5) / 0.35
            } else {
                0.0
            };
            ui.global::<App>().set_breathe_opacity(0.18 + 0.82 * wave);
        }
    });
    timer
}
