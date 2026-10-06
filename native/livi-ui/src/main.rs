//! LIVI-Lite native UI.
//!
//! Replaces the Electron renderer with a small Slint surface that talks to the
//! Node core over a newline-delimited JSON Unix socket. Projection video is a
//! compositor plane above this surface; while it is visible the UI still gets
//! all touch input and forwards it to the phone as normalized stream coords.

mod client;
mod launcher;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use client::{Client, livi_log, request_async};
use serde_json::{Map, Value, json};
use slint::{Image, Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

// TouchAction from ProjectionEnums.ts.
const TOUCH_DOWN: i32 = 14;
const TOUCH_MOVE: i32 = 15;
const TOUCH_UP: i32 = 16;

// CommandMapping ids.
const CMD_REQUEST_HOST_UI: i64 = 3;
const CMD_REQUEST_VIDEO_FOCUS: i64 = 500;
const CMD_HOME: i64 = 200;
const CMD_PLAY_PAUSE: i64 = 203;
const CMD_NEXT: i64 = 204;
const CMD_PREV: i64 = 205;

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
    client: Arc<Client>,
    config: Map<String, Value>,
    lists_requested: bool,
    user_navigated: bool,
    touch: TouchState,
    stream_w: f32,
    stream_h: f32,
    proj_w: f32,
    proj_h: f32,
    audio_out_ids: Vec<String>,
    audio_in_ids: Vec<String>,
    save_tx: Sender<(String, Value)>,
}

impl Ctx {
    fn new(client: Arc<Client>) -> Arc<Mutex<Self>> {
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
                    livi_log!("save-settings {}", patch);
                    match save_client.request_blocking("save-settings", json!([patch])) {
                        Ok(_) => livi_log!("settings saved"),
                        Err(e) => livi_log!("settings save failed: {e}"),
                    }
                }
            })
            .expect("failed to spawn settings thread");

        Arc::new(Mutex::new(Self {
            client,
            config: Map::new(),
            lists_requested: false,
            user_navigated: false,
            touch: TouchState::default(),
            stream_w: 1280.0,
            stream_h: 720.0,
            proj_w: 1280.0,
            proj_h: 720.0,
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

    let args: Vec<String> = std::env::args().collect();
    let nested = args.iter().any(|a| a == "--nested");
    let no_compositor = std::env::var("LIVI_NO_COMPOSITOR").ok().as_deref() == Some("1");

    if !nested && !no_compositor {
        if launcher::bootstrap_compositor() {
            // The compositor re-launches us with --nested.
            return;
        }
        livi_log!("no compositor found, running the UI directly");
    }

    let core_guard = if std::env::var("LIVI_CORE_EXTERNAL").ok().as_deref() == Some("1") {
        livi_log!("LIVI_CORE_EXTERNAL=1, not spawning the core");
        None
    } else {
        launcher::spawn_core()
    };

    let ui = MainWindow::new().expect("failed to create the Slint window");
    run_ui(ui);

    drop(core_guard);
}

fn run_ui(ui: MainWindow) {
    let client = Client::new();
    let ctx = Ctx::new(client.clone());

    let on_event: client::EventFn = {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        Arc::new(move |event: &str, args: Value| {
            let event = event.to_string();
            let weak = weak.clone();
            let ctx = ctx.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    handle_event(&ui, &ctx, &event, args);
                }
            });
        })
    };

    let on_status: client::StatusFn = {
        let weak = ui.as_weak();
        Arc::new(move |connected: bool| {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    let app = ui.global::<App>();
                    app.set_core_connected(connected);
                    app.set_status_message(if connected {
                        "core connected".into()
                    } else {
                        "waiting for core".into()
                    });
                    if !connected {
                        app.set_streaming(false);
                    }
                }
            });
        })
    };

    client::spawn_listener(client, on_event, on_status);

    wire_callbacks(&ui, &ctx);

    let _clock = start_clock(&ui);
    let _breathe = start_breathe(&ui);

    // With nothing projecting yet, open on the shell instead of the (empty)
    // projection surface. The first "projection shown" event switches to page 0
    // unless the user navigated first.
    ui.global::<App>().set_page(1);
    request_devices(&ui, &ctx);
    request_transport_state(&ui, &ctx);

    livi_log!("starting Slint event loop");
    if let Err(e) = ui.run() {
        livi_log!("event loop error: {e}");
    }
    livi_log!("UI exited");
}

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

fn wire_callbacks(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let app = ui.global::<App>();

    app.on_nav({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |page| {
            if let Some(ui) = weak.upgrade() {
                if let Ok(mut ctx) = ctx.lock() {
                    ctx.user_navigated = true;
                }
                ui.global::<App>().set_page(page);
                let client = ctx.lock().unwrap().client.clone();
                client.fire("ui:path", json!([page_path(page)]));
                match page {
                    1 => {
                        request_devices(&ui, &ctx);
                        request_transport_state(&ui, &ctx);
                    }
                    2 => request_media(&ui, &ctx),
                    3 => fetch_options(&ui, &ctx, true),
                    _ => {}
                }
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
                    request_devices(&ui, &ctx);
                    return;
                }
                let client = ctx.lock().unwrap().client.clone();
                client.fire("projection-command", json!([CMD_HOME]));
                set_projection_visible(&ctx, true);
                request_async(client.clone(), "projection-sendframe", json!([]), |res| {
                    if let Err(e) = res {
                        livi_log!("sendframe failed: {e}");
                    }
                });
                client.fire("ui:path", json!([page_path(0)]));
                app.set_page(0);
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
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                let app = ui.global::<App>();
                app.set_devices_status(format!("selecting {id}").into());
                let client = ctx.lock().unwrap().client.clone();
                let id = id.to_string();
                request_async(client, "devices:select", json!([id]), move |res| {
                    if let Err(e) = res {
                        livi_log!("devices:select failed: {e}");
                    }
                });
            }
        }
    });

    app.on_forget_device({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |id| {
            if weak.upgrade().is_some() {
                let client = ctx.lock().unwrap().client.clone();
                let id = id.to_string();
                let looks_like_mac = id.matches(':').count() == 5;
                request_async(client.clone(), "devices:forget", json!([id]), {
                    let weak = weak.clone();
                    let ctx = ctx.clone();
                    move |res| {
                        if let Err(e) = res {
                            livi_log!("devices:forget failed: {e}");
                        }
                        if let Some(ui) = weak.upgrade() {
                            request_devices(&ui, &ctx);
                        }
                    }
                });
                if looks_like_mac {
                    request_async(client, "projection-bt-forget-device", json!([id]), |res| {
                        if let Err(e) = res {
                            livi_log!("projection-bt-forget-device failed: {e}");
                        }
                    });
                }
            }
        }
    });

    app.on_connect_device({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |mac| {
            if weak.upgrade().is_some() {
                let client = ctx.lock().unwrap().client.clone();
                let mac = mac.to_string();
                request_async(client, "projection-bt-connect-device", json!([mac]), {
                    let weak = weak.clone();
                    let ctx = ctx.clone();
                    move |res| {
                        match &res {
                            Ok(v) => livi_log!("bt connect: {v}"),
                            Err(e) => livi_log!("bt connect failed: {e}"),
                        }
                        if let Some(ui) = weak.upgrade() {
                            request_devices(&ui, &ctx);
                        }
                    }
                });
            }
        }
    });

    app.on_refresh_devices({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                request_devices(&ui, &ctx);
            }
        }
    });

    app.on_cycle_session({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move || {
            if weak.upgrade().is_some() {
                let client = ctx.lock().unwrap().client.clone();
                request_async(client, "devices:cycle", json!([]), {
                    let weak = weak.clone();
                    let ctx = ctx.clone();
                    move |res| {
                        if let Err(e) = res {
                            if let Some(ui) = weak.upgrade() {
                                ui.global::<App>().set_devices_status(e.into());
                            }
                        }
                        if let Some(ui) = weak.upgrade() {
                            request_devices(&ui, &ctx);
                        }
                    }
                });
            }
        }
    });

    app.on_cluster_request({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |enabled| {
            if weak.upgrade().is_some() {
                let client = ctx.lock().unwrap().client.clone();
                request_async(client, "cluster:request", json!([enabled]), {
                    let weak = weak.clone();
                    move |res| {
                        if let Ok(v) = res {
                            let on = v.get("enabled").and_then(Value::as_bool).unwrap_or(enabled);
                            if let Some(ui) = weak.upgrade() {
                                ui.global::<App>().set_cluster_on(on);
                            }
                        }
                    }
                });
            }
        }
    });

    app.on_switch_transport({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                let app = ui.global::<App>();
                app.set_devices_status(format!("switching transport for {id}").into());
                let client = ctx.lock().unwrap().client.clone();
                let id = id.to_string();
                request_async(client, "transport:switch", json!([id]), move |res| match res {
                    Ok(v) => livi_log!("transport:switch -> {v}"),
                    Err(e) => livi_log!("transport:switch failed: {e}"),
                });
            }
        }
    });

    app.on_media_command({
        let ctx = ctx.clone();
        move |cmd| {
            let client = ctx.lock().unwrap().client.clone();
            let id = match cmd {
                c if c == CMD_PREV as i32 => CMD_PREV,
                c if c == CMD_PLAY_PAUSE as i32 => CMD_PLAY_PAUSE,
                c if c == CMD_NEXT as i32 => CMD_NEXT,
                c if c == CMD_HOME as i32 => CMD_HOME,
                other => other as i64,
            };
            client.fire("projection-command", json!([id]));
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
            let key = key.to_string();
            let stream = match key.as_str() {
                "audioVolume" => Some("music"),
                "navVolume" => Some("nav"),
                "voiceAssistantVolume" => Some("voiceAssistant"),
                "callVolume" => Some("call"),
                _ => None,
            };
            if let Some(stream) = stream {
                let client = ctx.lock().unwrap().client.clone();
                // The core's projection-set-volume handler expects `{stream,volume}`.
                client
                    .fire("projection-set-volume", json!([{ "stream": stream, "volume": value }]));
            }
            queue_setting(&ctx, key, json!(value));
        }
    });

    app.on_set_bool({
        let ctx = ctx.clone();
        move |key, value| {
            queue_setting(&ctx, key.to_string(), json!(value));
        }
    });

    app.on_refresh_settings({
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move || {
            if weak.upgrade().is_some() {
                let client = ctx.lock().unwrap().client.clone();
                request_async(client, "getSettings", json!([]), {
                    let weak = weak.clone();
                    let ctx = ctx.clone();
                    move |res| match res {
                        Ok(settings) => {
                            if let Some(ui) = weak.upgrade() {
                                apply_settings(&ui, &ctx, &settings);
                            }
                        }
                        Err(e) => livi_log!("getSettings failed: {e}"),
                    }
                });
            }
        }
    });

    app.on_restart_app({
        let ctx = ctx.clone();
        move || {
            let client = ctx.lock().unwrap().client.clone();
            livi_log!("restart requested");
            request_async(client, "app:restartApp", json!([]), |res| {
                if let Err(e) = res {
                    livi_log!("app:restartApp failed: {e}");
                }
            });
        }
    });

    app.on_quit_app({
        let ctx = ctx.clone();
        move || {
            let client = ctx.lock().unwrap().client.clone();
            livi_log!("quit requested");
            request_async(client, "app:quitApp", json!([]), |res| {
                if let Err(e) = res {
                    livi_log!("app:quitApp failed: {e}");
                }
            });
        }
    });

    app.on_projection_control({
        let ctx = ctx.clone();
        move |action| {
            let (method, label) = match action {
                0 => ("projection-start", "start"),
                1 => ("projection-stop", "stop"),
                _ => ("projection-restart", "restart"),
            };
            let client = ctx.lock().unwrap().client.clone();
            livi_log!("projection {label} requested");
            request_async(client, method, json!([]), move |res| {
                if let Err(e) = res {
                    livi_log!("{method} failed: {e}");
                }
            });
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

// ---------------------------------------------------------------- events

fn handle_event(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, event: &str, args: Value) {
    match event {
        "hello" => {
            let pid = args.pointer("/0/pid").and_then(Value::as_u64).unwrap_or(0);
            let protocol = args.pointer("/0/protocol").and_then(Value::as_u64).unwrap_or(0);
            livi_log!("core hello (pid {pid}, protocol {protocol})");
        }
        "settings" => {
            if let Some(settings) = args.get(0) {
                apply_settings(ui, ctx, settings);
            }
        }
        "projection-event" => {
            if let Some(ev) = args.get(0) {
                handle_projection_event(ui, ctx, ev);
            }
        }
        "telemetry:update" => {
            if let Some(snapshot) = args.get(0) {
                let speed = snapshot
                    .get("speedKph")
                    .and_then(Value::as_f64)
                    .or_else(|| snapshot.get("speed").and_then(Value::as_f64));
                if let Some(speed) = speed {
                    ui.global::<App>().set_speed_text(format!("{speed:.0} km/h").into());
                }
            }
        }
        "link-speed" => {
            let app = ui.global::<App>();
            match args.get(0) {
                Some(Value::Object(link)) => {
                    let down = link.get("downMbps").and_then(Value::as_f64).unwrap_or(0.0);
                    let up = link.get("upMbps").and_then(Value::as_f64).unwrap_or(0.0);
                    app.set_link_text(format!("{down:.1} / {up:.1} Mbps").into());
                }
                Some(Value::Null) | None => app.set_link_text("".into()),
                Some(other) => app.set_link_text(format!("link {other}").into()),
            }
        }
        "cluster-video-resolution" => {
            if let Some(res) = args.get(0) {
                livi_log!("cluster video resolution {res}");
            }
        }
        "app:media-key" => {
            if let Some(cmd) = args.get(0).and_then(Value::as_str) {
                livi_log!("media key: {cmd}");
            }
        }
        "update:event" => {
            if let Some(payload) = args.get(0) {
                let phase = payload.get("phase").and_then(Value::as_str).unwrap_or("");
                let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
                let text = match (phase, message) {
                    ("", "") => String::new(),
                    ("", m) => m.to_string(),
                    (p, "") => p.to_string(),
                    (p, m) => format!("{p}: {m}"),
                };
                ui.global::<App>().set_update_text(text.into());
            }
        }
        "update:progress" => {
            if let Some(payload) = args.get(0) {
                let percent = payload.get("percent").and_then(Value::as_f64);
                let text = match percent {
                    Some(p) => format!("update {p:.0}%"),
                    None => match payload.get("received").and_then(Value::as_f64) {
                        Some(received) => format!("update {:.1} MB", received / 1e6),
                        None => "updating".to_string(),
                    },
                };
                ui.global::<App>().set_update_text(text.into());
            }
        }
        other => livi_log!("unhandled event '{other}'"),
    }
}

fn handle_projection_event(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, ev: &Value) {
    let app = ui.global::<App>();
    match ev.get("type").and_then(Value::as_str).unwrap_or("") {
        "plugged" => {
            app.set_projection_active(true);
            app.set_streaming(true);
            app.set_status_message("phone connected".into());
        }
        "unplugged" => {
            app.set_projection_active(false);
            app.set_streaming(false);
            app.set_protocol("none".into());
            clear_media(&app);
            // A teardown must never leave the user on the (now empty) projection
            // surface with the shell hidden.
            if app.get_page() == 0 {
                set_projection_visible(ctx, false);
                app.set_page(1);
                request_devices(ui, ctx);
            }
        }
        "projection" => {
            let shown = ev.get("shown").and_then(Value::as_bool).unwrap_or(false);
            app.set_projection_shown(shown);
            if shown {
                // A phone starts projecting: jump to the projection page unless the
                // user already navigated somewhere on their own. The core keeps the
                // plane hidden across session teardowns, so re-show it with the page.
                let navigated = ctx.lock().map(|c| c.user_navigated).unwrap_or(true);
                if !navigated || app.get_page() == 0 {
                    set_projection_visible(ctx, true);
                    app.set_page(0);
                }
            } else {
                set_projection_visible(ctx, false);
                if app.get_page() == 0 {
                    app.set_page(1);
                    request_devices(ui, ctx);
                }
            }
        }
        "resolution" => {
            let width = ev.pointer("/payload/width").and_then(Value::as_f64);
            let height = ev.pointer("/payload/height").and_then(Value::as_f64);
            if let (Some(width), Some(height)) = (width, height) {
                if width > 0.0 && height > 0.0 {
                    let mut ctx = ctx.lock().unwrap();
                    ctx.stream_w = width as f32;
                    ctx.stream_h = height as f32;
                    livi_log!("stream resolution {}x{}", width, height);
                }
            }
        }
        "session" => {
            let protocol = ev.get("protocol").and_then(Value::as_str).unwrap_or("none");
            app.set_protocol(protocol.into());
        }
        "media" => {
            if let Some(payload) = ev.pointer("/payload/payload") {
                apply_media_payload(ui, payload);
            }
        }
        "media-reset" => clear_media(&app),
        "devices" => {
            if let Some(payload) = ev.get("payload") {
                set_devices(ui, payload);
            }
        }
        "navigation" => {
            let destination = ev
                .pointer("/payload/display/destinationName")
                .and_then(Value::as_str)
                .or_else(|| ev.pointer("/payload/navi/destinationName").and_then(Value::as_str));
            if let Some(destination) = destination {
                app.set_nav_text(format!("→ {destination}").into());
            }
        }
        "navigation-reset" => app.set_nav_text("".into()),
        "command" => {
            // CommandMapping.requestHostUI: the phone hands video focus back to
            // the head unit (Android Auto's Exit / CarPlay's "My Car"). Hide the
            // video plane and reveal the shell; without this the plane keeps
            // covering the UI with the phone's last (black) frame.
            let value = ev.pointer("/message/value").and_then(Value::as_i64);
            if value == Some(CMD_REQUEST_HOST_UI) {
                livi_log!("phone requested the host UI");
                set_projection_visible(ctx, false);
                if app.get_page() == 0 {
                    let client = ctx.lock().unwrap().client.clone();
                    client.fire("ui:path", json!([page_path(1)]));
                    app.set_page(1);
                    request_devices(ui, ctx);
                }
            } else if value == Some(CMD_REQUEST_VIDEO_FOCUS) {
                // The phone is projecting again (Android Auto reopened or
                // CarPlay resumed): show it unless the user navigated away.
                let navigated = ctx.lock().map(|c| c.user_navigated).unwrap_or(true);
                if !navigated || app.get_page() == 0 {
                    livi_log!("phone took video focus");
                    set_projection_visible(ctx, true);
                    app.set_page(0);
                }
            }
        }
        "audio" | "audioInfo" | "audioDevicesChanged" => {}
        other => livi_log!("projection event '{other}'"),
    }
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

    {
        let mut ctx = ctx.lock().unwrap();
        if let (Some(w), Some(h)) =
            (get_f32(&map, "projectionWidth"), get_f32(&map, "projectionHeight"))
        {
            if w > 0.0 && h > 0.0 {
                ctx.proj_w = w;
                ctx.proj_h = h;
                ctx.stream_w = w;
                ctx.stream_h = h;
            }
        }
        ctx.config = map;
    }

    app.set_settings_loaded(true);
    sync_indexes(ui, ctx);
    fetch_options(ui, ctx, false);
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

fn fetch_options(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, force: bool) {
    let client = {
        let mut ctx = ctx.lock().unwrap();
        if ctx.lists_requested && !force {
            return;
        }
        ctx.lists_requested = true;
        ctx.client.clone()
    };

    // Version.
    request_async(client.clone(), "app:getVersion", json!([]), {
        let weak = ui.as_weak();
        move |res| {
            if let (Some(ui), Ok(Value::String(version))) = (weak.upgrade(), res) {
                ui.global::<App>().set_version_text(version.into());
            }
        }
    });

    // Wi-Fi interfaces.
    request_async(client.clone(), "app:listWifiInterfaces", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>()
                        .set_wifi_interface_options(string_model(string_list(&value)));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Bluetooth adapters.
    request_async(client.clone(), "app:listBtAdapters", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_bt_adapter_options(string_model(string_list(&value)));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Wi-Fi channels.
    request_async(client.clone(), "app:listWifiChannels", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_wifi_channel_options(string_model(number_list(&value)));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Country codes.
    request_async(client.clone(), "app:listWifiCountryCodes", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_country_options(string_model(string_list(&value)));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Display modes.
    request_async(client.clone(), "app:listDisplayModes", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_display_mode_options(string_model(string_list(&value)));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Audio sinks (label -> id kept parallel in Ctx).
    request_async(client.clone(), "audio:listSinks", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                let (labels, ids) = audio_options(&value);
                {
                    let mut ctx = ctx.lock().unwrap();
                    ctx.audio_out_ids = ids;
                }
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_audio_output_options(string_model(labels));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Audio sources.
    request_async(client.clone(), "audio:listSources", json!([]), {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        move |res| {
            if let Ok(value) = res {
                let (labels, ids) = audio_options(&value);
                {
                    let mut ctx = ctx.lock().unwrap();
                    ctx.audio_in_ids = ids;
                }
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_audio_input_options(string_model(labels));
                    sync_indexes(&ui, &ctx);
                }
            }
        }
    });

    // Default microphone label (shown in the log; audio input keeps device ids).
    request_async(client.clone(), "get-sysdefault-mic-label", json!([]), |res| {
        if let Ok(value) = res {
            livi_log!("default mic label: {value}");
        }
    });

    // Transport state (the top bar shows whatever the shell can report).
    request_transport_state(ui, ctx);

    // No visualizer on this UI: make sure the core never streams audio chunks here.
    client.fire("projection-set-visualizer-enabled", json!([false]));

    // Telemetry snapshot.
    request_async(client, "telemetry:snapshot", json!([]), {
        let weak = ui.as_weak();
        move |res| {
            if let (Some(ui), Ok(snapshot)) = (weak.upgrade(), res) {
                let speed = snapshot
                    .get("speedKph")
                    .and_then(Value::as_f64)
                    .or_else(|| snapshot.get("speed").and_then(Value::as_f64));
                if let Some(speed) = speed {
                    ui.global::<App>().set_speed_text(format!("{speed:.0} km/h").into());
                }
            }
        }
    });
}

fn request_transport_state(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let client = ctx.lock().unwrap().client.clone();
    request_async(client, "transport:state", json!([]), {
        let weak = ui.as_weak();
        move |res| {
            if let (Some(ui), Ok(value)) = (weak.upgrade(), res) {
                if let Some(text) = transport_text(&value) {
                    ui.global::<App>().set_transport(text.into());
                }
            }
        }
    });
}

fn transport_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(map) => {
            for key in ["active", "transport", "current", "mode", "name"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
            None
        }
        _ => None,
    }
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

fn request_devices(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let client = ctx.lock().unwrap().client.clone();
    ui.global::<App>().set_devices_status("loading…".into());
    request_async(client, "devices:list", json!([]), {
        let weak = ui.as_weak();
        move |res| match res {
            Ok(value) => {
                if let Some(ui) = weak.upgrade() {
                    set_devices(&ui, &value);
                }
            }
            Err(e) => {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<App>().set_devices_status(e.into());
                }
            }
        }
    });
}

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

fn request_media(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>) {
    let client = ctx.lock().unwrap().client.clone();

    request_async(client.clone(), "projection-media-read", json!([]), {
        let weak = ui.as_weak();
        move |res| {
            if let (Some(ui), Ok(value)) = (weak.upgrade(), res) {
                if !value.is_null() {
                    if let Some(payload) = value.get("payload").or(Some(&value)) {
                        apply_media_payload(&ui, payload);
                    }
                }
            }
        }
    });

    request_async(client, "projection-navigation-read", json!([]), {
        let weak = ui.as_weak();
        move |res| {
            if let (Some(ui), Ok(value)) = (weak.upgrade(), res) {
                let destination =
                    value.pointer("/payload/display/destinationName").and_then(Value::as_str);
                if let Some(destination) = destination {
                    ui.global::<App>().set_nav_text(format!("→ {destination}").into());
                }
            }
        }
    });
}

fn clear_media(app: &App) {
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

fn apply_media_payload(ui: &MainWindow, payload: &Value) {
    let app = ui.global::<App>();

    // Idle sessions report `error` with placeholder dashes; show the empty state.
    if payload.get("error").and_then(Value::as_bool).unwrap_or(false) {
        clear_media(&app);
        return;
    }

    if let Some(encoded) = payload.get("base64Image").and_then(Value::as_str) {
        if !encoded.is_empty() {
            set_artwork(ui, encoded);
        }
    }

    let Some(media) = payload.get("media") else {
        return;
    };
    if media.is_null() {
        return;
    }

    let mut state = app.get_media();
    state.available = true;
    if let Some(v) = media.get("MediaSongName").and_then(Value::as_str) {
        state.title = v.into();
    }
    if let Some(v) = media.get("MediaArtistName").and_then(Value::as_str) {
        state.artist = v.into();
    }
    if let Some(v) = media.get("MediaAlbumName").and_then(Value::as_str) {
        state.album = v.into();
    }
    if let Some(v) = media.get("MediaAPPName").and_then(Value::as_str) {
        state.app = v.into();
    }
    if let Some(v) = media.get("MediaSongDuration").and_then(Value::as_f64) {
        state.duration = format_time(v).into();
    }
    if let Some(v) = media.get("MediaSongPlayTime").and_then(Value::as_f64) {
        state.elapsed = format_time(v).into();
    }
    if let Some(v) = media.get("MediaPlayStatus").and_then(Value::as_i64) {
        state.state = match v {
            1 => "Playing",
            2 => "Paused",
            3 => "Stopped",
            _ => "",
        }
        .into();
    }
    let duration = media.get("MediaSongDuration").and_then(Value::as_f64).unwrap_or(0.0);
    let position = media.get("MediaSongPlayTime").and_then(Value::as_f64).unwrap_or(0.0);
    if duration > 0.0 {
        state.progress = (position / duration).clamp(0.0, 1.0) as f32;
        state.remaining = format!("-{}", format_time((duration - position).max(0.0))).into();
    }
    app.set_media(state);
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

// ---------------------------------------------------------------- touch

fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Port of useProjectionTouch.ts `norm()`: window -> letterboxed by the
/// content aspect -> crop-adjusted normalized stream coordinates.
fn map_touch(ctx: &Ctx, px: f32, py: f32, win_w: f32, win_h: f32) -> Option<(f32, f32)> {
    if win_w <= 0.0 || win_h <= 0.0 {
        return None;
    }
    let stream_w = ctx.stream_w;
    let stream_h = ctx.stream_h;
    let (proj_w, proj_h) = (ctx.proj_w, ctx.proj_h);

    if stream_w <= 0.0 || stream_h <= 0.0 || proj_w <= 0.0 || proj_h <= 0.0 {
        return Some((clamp01(px / win_w), clamp01(py / win_h)));
    }

    // The phone renders the user-chosen AR inside the transport tier.
    let user_ar = proj_w / proj_h;
    let frame_ar = stream_w / stream_h;
    let (visible_w, visible_h) = if user_ar <= frame_ar {
        (stream_h * user_ar, stream_h)
    } else {
        (stream_w, stream_w / user_ar)
    };
    if visible_w <= 0.0 || visible_h <= 0.0 {
        return None;
    }

    // Display-letterbox by the content AR.
    let content_ar = visible_w / visible_h;
    let (disp_w, disp_h, off_x, off_y) = if win_w / win_h > content_ar {
        let disp_w = win_h * content_ar;
        (disp_w, win_h, (win_w - disp_w) / 2.0, 0.0)
    } else {
        let disp_h = win_w / content_ar;
        (win_w, disp_h, 0.0, (win_h - disp_h) / 2.0)
    };

    let local_x = px - off_x;
    let local_y = py - off_y;
    if local_x < 0.0 || local_x > disp_w || local_y < 0.0 || local_y > disp_h {
        return None;
    }

    // Content-crop onto the transport tier.
    let crop_left = ((stream_w - visible_w) / 2.0).max(0.0);
    let crop_top = ((stream_h - visible_h) / 2.0).max(0.0);
    let stream_x = crop_left + (local_x / disp_w) * visible_w;
    let stream_y = crop_top + (local_y / disp_h) * visible_h;
    Some((clamp01(stream_x / stream_w), clamp01(stream_y / stream_h)))
}

fn send_touch(client: &Arc<Client>, x: f32, y: f32, action: i32) {
    // The core's projection-touch handler expects the preload's `{x,y,action}`.
    client.fire("projection-touch", json!([{ "x": x, "y": y, "action": action }]));
}

/// Toggles the compositor video plane. The core remembers the last value across
/// session teardowns, so the UI has to re-assert it whenever projection starts,
/// stops or the shell is revealed.
fn set_projection_visible(ctx: &Arc<Mutex<Ctx>>, visible: bool) {
    livi_log!("projection plane visible={visible}");
    let client = ctx.lock().unwrap().client.clone();
    request_async(client, "projection-set-visible", json!([visible]), move |res| {
        if let Err(e) = res {
            livi_log!("projection-set-visible({visible}) failed: {e}");
        }
    });
}

/// Hides the video plane and switches to the shell.
fn reveal_shell(ui: &MainWindow, client: Arc<Client>, app: &App, ctx: &Arc<Mutex<Ctx>>) {
    livi_log!("edge swipe -> showing shell");
    client.fire("ui:path", json!([page_path(1)]));
    request_async(client, "projection-set-visible", json!([false]), |res| {
        if let Err(e) = res {
            livi_log!("projection-set-visible(false) failed: {e}");
        }
    });
    app.set_page(1);
    request_devices(ui, ctx);
}

fn on_pointer(ui: &MainWindow, ctx: &Arc<Mutex<Ctx>>, kind: i32, x: f32, y: f32, w: f32, h: f32) {
    let app = ui.global::<App>();
    let ctx_arc = ctx.clone();
    let mut guard = ctx.lock().unwrap();
    let now = Instant::now();

    match kind {
        0 => {
            // Down.
            guard.client.fire("app:user-activity", json!([]));
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
                if let Some((nx, ny)) = map_touch(&guard, x, y, w, h) {
                    guard.touch.forwarded = true;
                    send_touch(&guard.client, nx, ny, TOUCH_DOWN);
                } else {
                    guard.touch.active = false;
                }
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
                    let client = guard.client.clone();
                    drop(guard);
                    reveal_shell(ui, client, &app, &ctx_arc);
                    return;
                }

                let distance = (dx * dx + dy * dy).sqrt();
                if distance > EDGE_DEAD_ZONE || elapsed > EDGE_MAX_MS {
                    // It is a normal touch that happened to start near the edge.
                    let start = map_touch(&guard, guard.touch.start_x, guard.touch.start_y, w, h);
                    if let Some((nx, ny)) = start {
                        guard.touch.forwarded = true;
                        send_touch(&guard.client, nx, ny, TOUCH_DOWN);
                        if let Some((mx, my)) = map_touch(&guard, x, y, w, h) {
                            send_touch(&guard.client, mx, my, TOUCH_MOVE);
                        }
                    } else {
                        guard.touch.active = false;
                        guard.touch.canceled = true;
                    }
                }
                return;
            }

            if guard.touch.forwarded {
                if let Some((nx, ny)) = map_touch(&guard, x, y, w, h) {
                    send_touch(&guard.client, nx, ny, TOUCH_MOVE);
                }
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
                    let client = guard.client.clone();
                    drop(guard);
                    reveal_shell(ui, client, &app, &ctx_arc);
                    return;
                }

                // A tap in the edge zone: deliver it after the fact.
                let at = map_touch(&guard, guard.touch.start_x, guard.touch.start_y, w, h)
                    .or_else(|| map_touch(&guard, x, y, w, h));
                if let Some((nx, ny)) = at {
                    send_touch(&guard.client, nx, ny, TOUCH_DOWN);
                    let (ux, uy) = map_touch(&guard, x, y, w, h).unwrap_or((nx, ny));
                    send_touch(&guard.client, ux, uy, TOUCH_UP);
                }
            } else if guard.touch.forwarded && !guard.touch.canceled {
                let at = map_touch(&guard, x, y, w, h)
                    .or_else(|| map_touch(&guard, guard.touch.last_x, guard.touch.last_y, w, h));
                let (ux, uy) = at.unwrap_or((0.0, 0.0));
                send_touch(&guard.client, ux, uy, TOUCH_UP);
            }
            guard.touch = TouchState::default();
        }
        _ => {
            // Cancel.
            if guard.touch.forwarded && !guard.touch.canceled {
                let at = map_touch(&guard, guard.touch.last_x, guard.touch.last_y, w, h);
                let (ux, uy) = at.unwrap_or((0.0, 0.0));
                send_touch(&guard.client, ux, uy, TOUCH_UP);
            }
            guard.touch = TouchState::default();
        }
    }
}
