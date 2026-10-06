//! statusData.json is read by programs outside LIVI.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use livi_core_proto::state::Protocol;
use livi_cp::stack::AudioKind;
use serde::Serialize;
use tokio::sync::watch;

use crate::server::Core;

const FILE: &str = "statusData.json";
const VERSION: u32 = 1;
const DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    pub media: bool,
    pub speech: bool,
    pub system: bool,
    pub phone: bool,
    pub voice_assistant: bool,
    pub nav_announcing: bool,
}

impl Activity {
    pub fn stream(&mut self, kind: AudioKind, playing: bool) {
        match kind {
            AudioKind::Media => self.media = playing,
            AudioKind::Call => self.phone = playing,
            AudioKind::Speech => self.voice_assistant = playing,
            AudioKind::Alert => {
                self.nav_announcing = playing;
                self.speech = playing;
            }
        }
    }
}

#[derive(Serialize)]
struct Playing {
    playing: bool,
}

#[derive(Serialize)]
struct Active {
    active: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Projection {
    active: Option<&'static str>,
    streaming: bool,
    phone_type: Option<&'static str>,
}

#[derive(Serialize)]
struct Audio {
    media: Playing,
    speech: Playing,
    system: Playing,
}

#[derive(Serialize)]
struct Nav {
    announcing: bool,
}

#[derive(Serialize)]
struct Ui {
    path: String,
}

/// The field order is part of the file format.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    ui: Ui,
    projection: Projection,
    audio: Audio,
    phone: Active,
    voice_assistant: Active,
    nav: Nav,
}

#[derive(Serialize)]
struct File<'a> {
    version: u32,
    timestamp: &'a str,
    payload: Payload,
}

fn payload(path: &str, active: Option<Protocol>, a: Activity) -> Payload {
    let (protocol, phone_type) = match active {
        Some(Protocol::Carplay) => (Some("cp"), Some("CarPlay")),
        Some(Protocol::Androidauto) => (Some("aa"), Some("AndroidAuto")),
        None => (None, None),
    };
    Payload {
        ui: Ui { path: path.to_string() },
        projection: Projection { active: protocol, streaming: active.is_some(), phone_type },
        audio: Audio {
            media: Playing { playing: a.media },
            speech: Playing { playing: a.speech },
            system: Playing { playing: a.system },
        },
        phone: Active { active: a.phone },
        voice_assistant: Active { active: a.voice_assistant },
        nav: Nav { announcing: a.nav_announcing },
    }
}

/// UTC with milliseconds, 2026-10-05T18:00:00.000Z.
fn iso(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days to a civil date, after Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

fn render(path: &str, active: Option<Protocol>, a: Activity, now: SystemTime) -> String {
    let file = File { version: VERSION, timestamp: &iso(now), payload: payload(path, active, a) };
    let mut text = serde_json::to_string_pretty(&file).unwrap_or_default();
    text.push('\n');
    text
}

/// A reader gets the old file or the new one, never half of it.
fn write(target: &Path, text: &str) {
    let tmp = target.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, target)) {
        eprintln!("[status] {} not written: {e}", target.display());
    }
}

pub async fn run(core: Arc<Core>, mut activity: watch::Receiver<Activity>, user_data: PathBuf) {
    let target = user_data.join(FILE);
    let mut state = core.hub.watch();
    let mut path = core.ui_path.subscribe();
    let mut seen: Option<(String, Option<Protocol>, Activity)> = None;
    loop {
        let now = (
            path.borrow_and_update().clone(),
            state.borrow_and_update().sessions.active,
            *activity.borrow_and_update(),
        );
        if seen.as_ref() != Some(&now) {
            let (p, active, a) = &now;
            write(&target, &render(p, *active, *a, SystemTime::now()));
            seen = Some(now);
            tokio::time::sleep(DEBOUNCE).await;
            continue;
        }
        tokio::select! {
            changed = state.changed() => if changed.is_err() { return },
            changed = activity.changed() => if changed.is_err() { return },
            changed = path.changed() => if changed.is_err() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn the_file_keeps_its_old_shape() {
        let mut a = Activity::default();
        a.stream(AudioKind::Alert, true);
        a.stream(AudioKind::Call, true);
        let at = UNIX_EPOCH + Duration::from_millis(1_759_687_200_123);
        let text = render("/media", Some(Protocol::Carplay), a, at);
        assert!(text.ends_with("}\n"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["timestamp"], "2025-10-05T18:00:00.123Z");
        assert_eq!(
            v["payload"],
            json!({
                "ui": { "path": "/media" },
                "projection": { "active": "cp", "streaming": true, "phoneType": "CarPlay" },
                "audio": {
                    "media": { "playing": false },
                    "speech": { "playing": true },
                    "system": { "playing": false }
                },
                "phone": { "active": true },
                "voiceAssistant": { "active": false },
                "nav": { "announcing": true }
            })
        );
        let keys: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("    \"") && l.ends_with('{'))
            .map(|l| l.trim().split('"').nth(1).unwrap())
            .collect();
        assert_eq!(keys, ["ui", "projection", "audio", "phone", "voiceAssistant", "nav"]);
    }

    #[test]
    fn dates_are_written_in_utc() {
        assert_eq!(iso(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00.000Z");
        let aa = payload("", Some(Protocol::Androidauto), Activity::default());
        assert_eq!(
            (aa.projection.active, aa.projection.phone_type),
            (Some("aa"), Some("AndroidAuto"))
        );
    }
}
