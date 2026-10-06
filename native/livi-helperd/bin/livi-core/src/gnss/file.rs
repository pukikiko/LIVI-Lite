//! gpsData.json is read by programs outside LIVI.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

use super::info::{GnssInfo, GpsFix};

pub const FILE: &str = "gpsData.json";
const VERSION: u32 = 2;
const DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Serialize)]
struct Contents<'a> {
    version: u32,
    /// Unix milliseconds.
    ts: u64,
    fix: Option<&'a GpsFix>,
    receiver: Value,
}

pub struct GpsFile {
    path: PathBuf,
    info: GnssInfo,
    fix: Option<GpsFix>,
    due: Option<Instant>,
}

impl GpsFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path, info: GnssInfo::default(), fix: None, due: None }
    }

    pub fn set_info(&mut self, info: GnssInfo, now: Instant) {
        self.info = info;
        self.schedule(now);
    }

    pub fn set_fix(&mut self, fix: &GpsFix, now: Instant) {
        self.fix.get_or_insert_default().merge(fix);
        self.schedule(now);
    }

    fn schedule(&mut self, now: Instant) {
        self.due.get_or_insert(now + DEBOUNCE);
    }

    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    fn render(&self, unix_ms: u64) -> String {
        let mut receiver = serde_json::to_value(&self.info).unwrap_or(Value::Null);
        if let Some(fields) = receiver.as_object_mut() {
            fields.remove("satellites");
        }
        let contents = Contents { version: VERSION, ts: unix_ms, fix: self.fix.as_ref(), receiver };
        let mut text = serde_json::to_string_pretty(&contents).unwrap_or_default();
        text.push('\n');
        text
    }

    pub fn flush(&mut self, unix_ms: u64) {
        self.due = None;
        let tmp = self.path.with_extension("json.tmp");
        // A reader gets the old file or the new one, never half of it.
        let written = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&tmp, self.render(unix_ms)))
            .and_then(|()| std::fs::rename(&tmp, &self.path));
        if let Err(e) = written {
            eprintln!("[gnss] {} not written: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config_file::tests::TempDir;
    use crate::gnss::info::{Constellation, Satellite};

    fn read(dir: &TempDir) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.0.join(FILE)).unwrap()).unwrap()
    }

    fn fix(lat: f64, lng: f64) -> GpsFix {
        GpsFix { lat: Some(lat), lng: Some(lng), ..Default::default() }
    }

    #[test]
    fn a_burst_of_changes_is_one_write_a_second_later() {
        let dir = TempDir::new();
        let mut file = GpsFile::new(dir.0.join(FILE));
        let now = Instant::now();
        assert_eq!(file.due(), None);
        file.set_fix(&fix(1.0, 2.0), now);
        file.set_info(GnssInfo { connected: true, ..Default::default() }, now + DEBOUNCE / 2);
        file.set_fix(&fix(3.0, 4.0), now + DEBOUNCE / 2);
        assert_eq!(file.due(), Some(now + DEBOUNCE));
        assert!(!dir.0.join(FILE).exists());
        file.flush(1_800_000_000_000);
        assert_eq!(file.due(), None);
        let v = read(&dir);
        assert_eq!(v["fix"], json!({ "lat": 3, "lng": 4 }));
        assert_eq!(v["receiver"]["connected"], true);
    }

    #[test]
    fn the_file_carries_the_fix_and_the_receiver_without_its_satellites() {
        let dir = TempDir::new();
        let path = dir.0.join("nested/dir").join(FILE);
        let mut file = GpsFile::new(path.clone());
        let now = Instant::now();
        file.set_info(
            GnssInfo {
                connected: true,
                satellites_used: 7.0,
                satellites_visible: 12,
                hdop: Some(0.8),
                constellations: vec![Constellation::Gps, Constellation::Galileo],
                satellites: vec![Satellite {
                    id: 4.0,
                    constellation: Constellation::Galileo,
                    used: true,
                    elevation: None,
                    azimuth: None,
                    snr: Some(44.0),
                }],
                ..Default::default()
            },
            now,
        );
        file.set_fix(&GpsFix { accuracy_m: Some(2.5), ..fix(48.1, 11.5) }, now);
        file.set_fix(&GpsFix { alt: Some(500.0), ..Default::default() }, now);
        file.flush(1_800_000_000_000);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n  \"version\": 2,\n  \"ts\": 1800000000000,\n  \"fix\": {"));
        assert!(text.ends_with("}\n"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["fix"], json!({ "lat": 48.1, "lng": 11.5, "alt": 500, "accuracyM": 2.5 }));
        assert_eq!(
            v["receiver"],
            json!({
                "connected": true,
                "fixQuality": "none",
                "fixMode": "none",
                "satellitesUsed": 7,
                "satellitesVisible": 12,
                "hdop": 0.8,
                "constellations": ["gps", "galileo"]
            })
        );
    }

    #[test]
    fn an_empty_file_says_no_fix_and_a_failed_write_is_survived() {
        let dir = TempDir::new();
        let mut file = GpsFile::new(dir.0.join(FILE));
        file.flush(0);
        assert_eq!(read(&dir)["fix"], Value::Null);

        std::fs::write(dir.0.join("blocker"), "").unwrap();
        let mut blocked = GpsFile::new(dir.0.join("blocker").join(FILE));
        blocked.flush(0);
    }
}
