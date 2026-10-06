use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use livi_core_proto::config::{AppearanceMode, Config};
use livi_core_proto::state::State;
use serde_json::{Map, Value, json};
use socketioxide::SocketIo;
use socketioxide::extract::{Data, SocketRef};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tower_http::cors::CorsLayer;

use crate::server::Core;

pub type Snapshot = Map<String, Value>;

pub const PORT: u16 = 4000;
const UPDATE: &str = "telemetry:update";
const PUSH: &str = "telemetry:push";
/// A producer may send these in parts, they merge field by field.
const NESTED: [&str; 2] = ["gps", "can"];
/// A moving car would rewrite the config with every fix.
const GPS_WRITE_EVERY: Duration = Duration::from_secs(30);
/// Rounding noise does not rewrite the config.
const VOLUME_STEP: f64 = 0.005;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub fn merge(snap: &mut Snapshot, patch: &Value) -> Option<Snapshot> {
    let patch = patch.as_object().filter(|p| !p.is_empty())?;
    let mut changed = Snapshot::new();
    for (key, incoming) in patch {
        let merged = match (incoming, snap.get(key)) {
            (Value::Object(inc), Some(Value::Object(prev))) if NESTED.contains(&key.as_str()) => {
                let mut block = prev.clone();
                block.extend(inc.clone());
                Value::Object(block)
            }
            _ => incoming.clone(),
        };
        snap.insert(key.clone(), merged.clone());
        changed.insert(key.clone(), merged);
    }
    if !patch.contains_key("ts") {
        let ts = Value::from(now_ms());
        snap.insert("ts".into(), ts.clone());
        changed.insert("ts".into(), ts);
    }
    Some(changed)
}

fn night_of(mode: AppearanceMode) -> Option<bool> {
    match mode {
        AppearanceMode::Night => Some(true),
        AppearanceMode::Day => Some(false),
        AppearanceMode::Auto => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Fix {
    lat: f64,
    lng: f64,
    alt: Option<f64>,
    heading: Option<f64>,
}

fn valid(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lng)
        && !(lat == 0.0 && lng == 0.0)
}

fn fix_of(snap: &Snapshot) -> Option<Fix> {
    let gps = snap.get("gps")?.as_object()?;
    let num = |k: &str| gps.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    let (lat, lng) = (num("lat")?, num("lng")?);
    valid(lat, lng).then(|| Fix { lat, lng, alt: num("alt"), heading: num("heading") })
}

struct Telemetry {
    core: Arc<Core>,
    io: SocketIo,
    snap: Snapshot,
    appearance: AppearanceMode,
    volume: Option<f64>,
    gps_written: Option<(f64, f64)>,
    gps_written_at: Option<Instant>,
    gps_due: Option<Instant>,
}

impl Telemetry {
    fn new(core: Arc<Core>, io: SocketIo) -> Self {
        let cfg = core.hub.config();
        let mut t = Self {
            core,
            io,
            snap: Snapshot::new(),
            appearance: cfg.appearance_mode,
            volume: None,
            gps_written: None,
            gps_written_at: None,
            gps_due: None,
        };
        t.seed(&cfg);
        t
    }

    fn seed(&mut self, cfg: &Config) {
        let mut patch = Snapshot::new();
        if let Some(night) = night_of(cfg.appearance_mode) {
            patch.insert("nightMode".into(), night.into());
        }
        if let Some(g) = cfg.last_known_gps.as_ref().filter(|g| valid(g.lat, g.lng)) {
            let mut gps = json!({ "lat": g.lat, "lng": g.lng, "fixTs": g.ts });
            if let Some(alt) = g.alt {
                gps["alt"] = alt.into();
            }
            if let Some(heading) = g.heading {
                gps["heading"] = heading.into();
            }
            patch.insert("gps".into(), gps);
        }
        if cfg.hu_volume.is_finite() {
            let level = cfg.hu_volume.clamp(0.0, 1.0);
            patch.insert("volume".into(), level.into());
            self.volume = Some(level);
        }
        merge(&mut self.snap, &Value::Object(patch));
    }

    async fn publish(&self) {
        let snap = self.snap.clone();
        self.core.hub.update(|s| s.telemetry = snap);
        let _ = self.io.emit(UPDATE, &self.snap).await;
    }

    async fn push(&mut self, patch: &Value) {
        let Some(changed) = merge(&mut self.snap, patch) else { return };
        self.publish().await;
        if let Some(level) = changed.get("volume").and_then(Value::as_f64) {
            self.volume_from(level).await;
        }
        if changed.contains_key("gps") {
            self.gps_changed().await;
        }
    }

    async fn appearance(&mut self, mode: AppearanceMode) {
        if mode == self.appearance {
            return;
        }
        self.appearance = mode;
        if let Some(night) = night_of(mode) {
            self.push(&json!({ "nightMode": night })).await;
        }
    }

    /// Steering wheel buttons, a serial bus radio or a dash app send this level.
    async fn volume_from(&mut self, raw: f64) {
        if !raw.is_finite() {
            return;
        }
        let level = raw.clamp(0.0, 1.0);
        if self.volume.is_some_and(|last| (level - last).abs() < VOLUME_STEP) {
            return;
        }
        self.volume = Some(level);
        println!("[volume] head unit set to {} % from telemetry", (level * 100.0).round());
        self.save(json!({ "huVolume": level })).await;
    }

    async fn gps_changed(&mut self) {
        if fix_of(&self.snap).is_none() {
            return;
        }
        match self.gps_written_at {
            Some(at) if at.elapsed() < GPS_WRITE_EVERY => {
                self.gps_due.get_or_insert(at + GPS_WRITE_EVERY);
            }
            _ => self.write_gps().await,
        }
    }

    async fn write_due_gps(&mut self) {
        self.gps_due = None;
        self.write_gps().await;
    }

    async fn write_gps(&mut self) {
        let Some(fix) = fix_of(&self.snap) else { return };
        if self.gps_written == Some((fix.lat, fix.lng)) {
            return;
        }
        self.gps_written = Some((fix.lat, fix.lng));
        self.gps_written_at = Some(Instant::now());
        let mut last = json!({ "lat": fix.lat, "lng": fix.lng, "ts": now_ms() });
        if let Some(alt) = fix.alt {
            last["alt"] = alt.into();
        }
        if let Some(heading) = fix.heading {
            last["heading"] = heading.into();
        }
        self.save(json!({ "lastKnownGps": last })).await;
    }

    async fn save(&self, patch: Value) {
        if let Err(e) = self.core.set_config(&patch).await {
            eprintln!("[telemetry] config not saved: {e}");
        }
    }
}

async fn until(due: Option<Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

pub fn socket(
    state: watch::Receiver<State>,
) -> (axum::Router, SocketIo, mpsc::UnboundedReceiver<Value>) {
    let (service, io) = SocketIo::new_svc();
    let (pushes, pushed) = mpsc::unbounded_channel();
    io.ns("/", move |socket: SocketRef| {
        let snap = state.borrow().telemetry.clone();
        if !snap.is_empty() {
            let _ = socket.emit(UPDATE, &snap);
        }
        let pushes = pushes.clone();
        socket.on(PUSH, move |Data::<Value>(payload)| {
            let _ = pushes.send(payload);
            async {}
        });
        async {}
    });
    // A fallback service, so every endpoint shares one engine and its sessions.
    let app = axum::Router::new().fallback_service(service).layer(CorsLayer::permissive());
    (app, io, pushed)
}

async fn listen() -> Option<TcpListener> {
    for addr in [format!("[::]:{PORT}"), format!("0.0.0.0:{PORT}")] {
        match TcpListener::bind(&addr).await {
            Ok(listener) => return Some(listener),
            Err(e) => eprintln!("[telemetry] cannot listen on {addr}: {e}"),
        }
    }
    None
}

/// `inside` carries what the car bridge and the GNSS receiver in core push.
pub async fn run(core: Arc<Core>, mut inside: mpsc::UnboundedReceiver<Value>) {
    let (app, io, mut pushed) = socket(core.hub.watch());
    tokio::spawn(async move {
        let Some(listener) = listen().await else { return };
        println!("[telemetry] listening on port {PORT}");
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[telemetry] server stopped: {e}");
        }
    });
    let mut t = Telemetry::new(core.clone(), io);
    t.publish().await;
    let mut state = core.hub.watch();
    loop {
        let due = t.gps_due;
        tokio::select! {
            Some(patch) = pushed.recv() => t.push(&patch).await,
            Some(patch) = inside.recv() => t.push(&patch).await,
            changed = state.changed() => {
                if changed.is_err() {
                    return;
                }
                let mode = state.borrow_and_update().config.appearance_mode;
                t.appearance(mode).await;
            }
            () = until(due) => t.write_due_gps().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use livi_core_proto::config::LastKnownGps;
    use livi_core_proto::state::{Front, PerScreen};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::Notify;

    use super::*;
    use crate::config_file::tests::TempDir;
    use crate::config_file::{ConfigFile, defaults};
    use crate::hub::Hub;

    fn state(config: Config, telemetry: Snapshot) -> State {
        State {
            front: PerScreen { main: Front::Livi, dash: Front::Livi, aux: Front::Livi },
            sessions: Default::default(),
            now_playing: Default::default(),
            telemetry,
            navigation: Default::default(),
            system: Default::default(),
            devices: Default::default(),
            update: Default::default(),
            config,
        }
    }

    fn core(dir: &TempDir, config: Config) -> Arc<Core> {
        let livi = PerScreen { main: Front::Livi, dash: Front::Livi, aux: Front::Livi };
        let hub = Hub::new(state(config, Snapshot::new()));
        let file = ConfigFile::new(dir.0.join("config.json"), dir.0.join("backup.json"));
        let (senders, ..) = crate::projection::ui_channels(livi);
        Arc::new(Core::new(hub, file, 0, Arc::new(Notify::new()), senders))
    }

    fn obj(v: Value) -> Snapshot {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn gps_and_can_merge_by_field_everything_else_is_replaced() {
        let mut snap = obj(json!({ "gps": { "lat": 1.0, "lng": 2.0 }, "gear": "D", "ts": 5 }));
        let changed = merge(&mut snap, &json!({ "gps": { "lat": 3.0 }, "gear": 2 })).unwrap();
        assert_eq!(snap["gps"], json!({ "lat": 3.0, "lng": 2.0 }));
        assert_eq!(
            (&snap["gear"], &changed["gps"]),
            (&json!(2), &json!({ "lat": 3.0, "lng": 2.0 }))
        );
        assert!(changed["ts"].as_u64().unwrap() > 5);
        assert_eq!(snap["ts"], changed["ts"]);

        let changed = merge(&mut snap, &json!({ "can": { "id": 1 }, "ts": 7 })).unwrap();
        assert_eq!((&snap["ts"], &changed["can"]), (&json!(7), &json!({ "id": 1 })));
        assert!(merge(&mut snap, &json!({})).is_none());
        assert!(merge(&mut snap, &json!([1])).is_none());
        merge(&mut snap, &json!({ "gps": "lost" }));
        assert_eq!(snap["gps"], "lost");
    }

    #[tokio::test(start_paused = true)]
    async fn the_config_seeds_the_snapshot_and_takes_volume_and_gps_back() {
        let dir = TempDir::new();
        let mut cfg = defaults();
        cfg.appearance_mode = AppearanceMode::Night;
        cfg.hu_volume = 0.5;
        cfg.last_known_gps =
            Some(LastKnownGps { lat: 48.1, lng: 11.5, alt: Some(520.0), heading: None, ts: 9 });
        let core = core(&dir, cfg);
        let (_, io, _) = socket(core.hub.watch());
        let mut t = Telemetry::new(core.clone(), io);
        assert_eq!(t.snap["nightMode"], true);
        assert_eq!(t.snap["volume"], 0.5);
        assert_eq!(t.snap["gps"], json!({ "lat": 48.1, "lng": 11.5, "alt": 520.0, "fixTs": 9 }));

        t.push(&json!({ "volume": 0.502 })).await;
        assert_eq!(core.hub.config().hu_volume, 0.5);
        t.push(&json!({ "volume": 2.0 })).await;
        assert_eq!(core.hub.config().hu_volume, 1.0);
        assert_eq!(core.hub.watch().borrow().telemetry["volume"], 2.0);

        t.push(&json!({ "gps": { "lat": 52.5, "lng": 13.4, "heading": 90.0 } })).await;
        let last = core.hub.config().last_known_gps.unwrap();
        assert_eq!(
            (last.lat, last.lng, last.alt, last.heading),
            (52.5, 13.4, Some(520.0), Some(90.0))
        );
        t.push(&json!({ "gps": { "lat": 52.6 } })).await;
        assert_eq!(core.hub.config().last_known_gps.unwrap().lat, 52.5);
        let due = t.gps_due.unwrap();
        assert_eq!(due - Instant::now(), GPS_WRITE_EVERY);
        tokio::time::advance(GPS_WRITE_EVERY).await;
        t.write_due_gps().await;
        assert_eq!(core.hub.config().last_known_gps.unwrap().lat, 52.6);
        t.write_gps().await;
        t.push(&json!({ "gps": { "lat": 0.0, "lng": 0.0 } })).await;
        assert_eq!(core.hub.config().last_known_gps.unwrap().lat, 52.6);
        assert!(t.gps_due.is_none());

        t.appearance(AppearanceMode::Night).await;
        t.appearance(AppearanceMode::Day).await;
        assert_eq!(core.hub.watch().borrow().telemetry["nightMode"], false);
        t.appearance(AppearanceMode::Auto).await;
        assert_eq!(t.snap["nightMode"], false);
    }

    #[tokio::test]
    async fn nothing_known_leaves_the_snapshot_empty() {
        let dir = TempDir::new();
        let mut cfg = defaults();
        cfg.last_known_gps =
            Some(LastKnownGps { lat: 0.0, lng: 0.0, alt: None, heading: None, ts: 1 });
        cfg.hu_volume = f64::NAN;
        let core = core(&dir, cfg);
        let (_, io, _) = socket(core.hub.watch());
        let mut t = Telemetry::new(core, io);
        assert!(t.snap.is_empty());
        t.volume_from(f64::INFINITY).await;
        assert_eq!(t.volume, None);
    }

    async fn poll(addr: SocketAddr, method: &str, sid: &str, body: &str) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let sid = if sid.is_empty() { String::new() } else { format!("&sid={sid}") };
        let req = format!(
            "{method} /socket.io/?EIO=4&transport=polling{sid} HTTP/1.1\r\nHost: livi\r\n\
             Connection: close\r\nContent-Type: text/plain;charset=UTF-8\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_string(&mut out))
            .await
            .unwrap()
            .unwrap();
        out
    }

    #[tokio::test]
    async fn a_socket_io_client_gets_the_snapshot_and_its_pushes_arrive() {
        let snap = obj(json!({ "speedKph": 42 }));
        let (_tx, rx) = watch::channel(state(defaults(), snap));
        let (app, io, mut pushed) = socket(rx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let open = poll(addr, "GET", "", "").await;
        let sid = open.split("\"sid\":\"").nth(1).and_then(|s| s.split('"').next()).unwrap();
        let joining = poll(addr, "POST", sid, "40").await;
        assert!(joining.contains("ok"), "{open}\n---\n{joining}");
        let joined = poll(addr, "GET", sid, "").await;
        assert!(joined.contains("40{\"sid\""));
        assert!(joined.contains(r#"42["telemetry:update",{"speedKph":42}]"#));

        poll(addr, "POST", sid, r#"42["telemetry:push",{"rpm":900}]"#).await;
        let push = tokio::time::timeout(Duration::from_secs(5), pushed.recv()).await.unwrap();
        assert_eq!(push, Some(json!({ "rpm": 900 })));

        io.emit(UPDATE, &json!({ "rpm": 900 })).await.unwrap();
        assert!(poll(addr, "GET", sid, "").await.contains(r#"42["telemetry:update",{"rpm":900}]"#));
        let elsewhere = poll(addr, "GET", "", "").await;
        assert!(
            elsewhere.contains("Access-Control-Allow-Origin")
                || elsewhere.contains("access-control-allow-origin")
        );
    }
}
