use livi_cp::helper_sock::HelperSock;
use livi_cp::manager::{CpCmd, CpHandle};
use serde_json::{Map, Value};

use crate::nmea::{self, Position};

pub struct CpTelemetry {
    helper: HelperSock,
    night: Option<bool>,
}

fn changed(prev: &Map<String, Value>, next: &Map<String, Value>, key: &str) -> bool {
    next.contains_key(key) && prev.get(key) != next.get(key)
}

fn num(map: &Map<String, Value>, key: &str) -> Option<f64> {
    map.get(key).and_then(Value::as_f64)
}

impl CpTelemetry {
    pub fn new(helper: HelperSock) -> Self {
        Self { helper, night: None }
    }

    pub fn follow(&mut self, prev: &Map<String, Value>, next: &Map<String, Value>, cp: &CpHandle) {
        if changed(prev, next, "nightMode")
            && let Some(night) = next.get("nightMode").and_then(Value::as_bool)
            && self.night != Some(night)
        {
            self.night = Some(night);
            cp.send(CpCmd::NightMode(night));
        }
        if changed(prev, next, "rangeKm") || changed(prev, next, "ambientC") {
            let mut status = Map::new();
            if let Some(range) = num(next, "rangeKm") {
                status.insert("range".into(), (range.round().clamp(0.0, 65535.0) as u32).into());
            }
            if let Some(temp) = num(next, "ambientC") {
                status.insert("outsideTemperature".into(), (temp.round() as i64).into());
            }
            if !status.is_empty() {
                let helper = self.helper.clone();
                let status = Value::Object(status);
                tokio::spawn(async move { helper.send_vehicle_status(&status).await });
            }
        }
        if changed(prev, next, "gps")
            && let Some(gps) = next.get("gps").and_then(Value::as_object)
            && let (Some(lat), Some(lng)) = (num(gps, "lat"), num(gps, "lng"))
        {
            let nmea = nmea::encode(&Position {
                lat,
                lng,
                alt: num(gps, "alt"),
                heading: num(gps, "heading"),
                speed_ms: num(gps, "speedMs"),
                fix_ms: num(gps, "fixTs"),
                accuracy_m: num(gps, "accuracyM"),
            });
            let helper = self.helper.clone();
            tokio::spawn(async move { helper.send_location(&nmea).await });
        }
    }

    pub fn hydrate(&mut self, snap: &Map<String, Value>, cp: &CpHandle) {
        self.night = None;
        self.follow(&Map::new(), snap, cp);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::config_file::tests::TempDir;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    fn helper(dir: &TempDir) -> (HelperSock, mpsc::UnboundedReceiver<String>) {
        let path = dir.0.join("cp-bt.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (rd, mut wr) = stream.into_split();
                let mut line = String::new();
                let _ = BufReader::new(rd).read_line(&mut line).await;
                let _ = tx.send(line.trim_end().to_string());
                let _ = wr.write_all(b"{\"ok\":true}\n").await;
            }
        });
        (HelperSock::new(path), rx)
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn changes_reach_the_phone_and_a_new_session_hears_everything() {
        let dir = TempDir::new();
        let (sock, mut lines) = helper(&dir);
        let (tx, mut cmds) = mpsc::unbounded_channel();
        let cp = CpHandle::from_sender(tx);
        let mut t = CpTelemetry::new(sock);
        let first = obj(json!({ "nightMode": true, "speedKph": 50, "ts": 1 }));
        t.follow(&Map::new(), &first, &cp);
        assert_eq!(cmds.try_recv(), Ok(CpCmd::NightMode(true)));

        let second =
            obj(json!({ "nightMode": true, "rangeKm": 70000.4, "ambientC": -3.6, "ts": 2 }));
        t.follow(&first, &second, &cp);
        assert!(cmds.try_recv().is_err());
        assert_eq!(
            next(&mut lines).await,
            r#"vehicle-status {"outsideTemperature":-4,"range":65535}"#
        );

        let third = obj(
            json!({ "gps": { "lat": 1.0, "lng": 1.0, "fixTs": 1_759_665_845_000u64 }, "ts": 3 }),
        );
        t.follow(&second, &third, &cp);
        let location = next(&mut lines).await;
        assert!(location.starts_with("location "));

        t.hydrate(&second, &cp);
        assert_eq!(cmds.try_recv(), Ok(CpCmd::NightMode(true)));
        assert!(next(&mut lines).await.starts_with("vehicle-status "));

        t.follow(&second, &obj(json!({ "gps": { "lat": 1.0 } })), &cp);
        t.follow(&second, &obj(json!({ "nightMode": "dusk" })), &cp);
        assert!(cmds.try_recv().is_err());
    }
}
