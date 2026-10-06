//! The panel belongs to the host compositor (Cage in the kiosk), not to ours.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use zbus::zvariant::OwnedValue;

use crate::hub::Hub;

/// Our own compositor's display is a nested one, every query names the host's.
const HOST_DISPLAY: &str = "wayland-0";
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);
const PIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Ships with the headless installer, without it the mode is not pinned.
const VIDEO_MODE_HELPER: &str = "/usr/local/lib/livi/livi-video-mode.sh";
const MUTTER: &str = "org.gnome.Mutter.DisplayConfig";
const MUTTER_PATH: &str = "/org/gnome/Mutter/DisplayConfig";

#[derive(Debug, PartialEq)]
struct Current {
    mode: String,
    hz: u32,
}

fn size_of(mode: &str) -> Option<(u32, u32)> {
    let (w, h) = mode.split_once('x')?;
    let num = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    Some((num(w)?, num(h)?))
}

/// "1920x1080 px, 60.000000 Hz (preferred, current)"
fn mode_line(line: &str) -> Option<(&str, &str)> {
    let (mode, rest) = line.trim().split_once(" px")?;
    size_of(mode)?;
    Some((mode, rest))
}

fn name_in(listing: &str) -> Option<&str> {
    listing.lines().next()?.split_whitespace().next()
}

fn largest_first(sizes: impl Iterator<Item = (u32, u32)>) -> Vec<String> {
    let mut modes: Vec<(u32, u32)> = Vec::new();
    for size in sizes {
        if !modes.contains(&size) {
            modes.push(size);
        }
    }
    modes.sort_by_key(|&(w, h)| std::cmp::Reverse(u64::from(w) * u64::from(h)));
    modes.into_iter().map(|(w, h)| format!("{w}x{h}")).collect()
}

fn modes_in(listing: &str) -> Vec<String> {
    largest_first(listing.lines().filter_map(mode_line).filter_map(|(m, _)| size_of(m)))
}

type Props = HashMap<String, OwnedValue>;
type MutterMode = (String, i32, i32, f64, f64, Vec<f64>, Props);
type MutterMonitor = ((String, String, String, String), Vec<MutterMode>, Props);
type MutterLogical = (i32, i32, f64, u32, bool, Vec<(String, String, String, String)>, Props);
type MutterState = (u32, Vec<MutterMonitor>, Vec<MutterLogical>, Props);

fn mutter_modes_in(monitors: &[MutterMonitor]) -> Vec<String> {
    let sizes = monitors
        .iter()
        .flat_map(|m| &m.1)
        .filter_map(|mode| Some((u32::try_from(mode.1).ok()?, u32::try_from(mode.2).ok()?)));
    largest_first(sizes)
}

/// GNOME does not speak the wlroots output protocol but tells its modes on the
/// session bus.
async fn mutter_modes() -> Vec<String> {
    let state = async {
        let bus = zbus::Connection::session().await.ok()?;
        let reply = bus
            .call_method(Some(MUTTER), MUTTER_PATH, Some(MUTTER), "GetCurrentState", &())
            .await
            .ok()?;
        reply.body().deserialize::<MutterState>().ok()
    };
    match tokio::time::timeout(TOOL_TIMEOUT, state).await {
        Ok(Some((_, monitors, _, _))) => mutter_modes_in(&monitors),
        _ => Vec::new(),
    }
}

fn current_in(listing: &str) -> Option<Current> {
    listing.lines().filter(|l| l.contains("current")).find_map(|line| {
        let (mode, rest) = mode_line(line)?;
        let hz: f64 = rest.strip_prefix(", ")?.split_once(" Hz")?.0.parse().ok()?;
        let hz = match hz.round() as u32 {
            0 => 60,
            hz => hz,
        };
        Some(Current { mode: mode.to_string(), hz })
    })
}

fn fits_hd(mode: &str) -> bool {
    size_of(mode).is_some_and(|(w, h)| w <= 1280 && h <= 720)
}

/// Every phone handles 720p projection, so an unconfigured panel snaps down to it.
fn kiosk_mode(configured: &str, current: Option<&Current>, modes: &[String]) -> Option<String> {
    if size_of(configured).is_some() {
        return Some(configured.to_string());
    }
    let current = current?;
    if fits_hd(&current.mode) {
        return None;
    }
    modes.iter().find(|m| fits_hd(m)).or(modes.last()).cloned()
}

async fn run(program: &str, args: &[&str], within: Duration) -> Option<String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .env("WAYLAND_DISPLAY", HOST_DISPLAY)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(within, out).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        _ => None,
    }
}

async fn listing() -> Option<String> {
    run("wlr-randr", &[], TOOL_TIMEOUT).await
}

/// A mode the output does not list leaves the host compositor unable to test
/// its swapchain, so only a listed one is sent.
async fn apply(name: &str, mode: &str, modes: &[String]) {
    if !modes.iter().any(|m| m == mode) {
        println!(
            "[display] {name} does not offer {mode}, leaving it as it is (offered: {})",
            if modes.is_empty() { "none".to_string() } else { modes.join(", ") }
        );
        return;
    }
    match run("wlr-randr", &["--output", name, "--mode", mode], TOOL_TIMEOUT).await {
        Some(_) => println!("[display] {name} → {mode}"),
        None => println!("[display] {name} refused {mode}, leaving it as it is"),
    }
}

/// Console and boot splash come up in the mode the panel runs in.
async fn pin() {
    if !Path::new(VIDEO_MODE_HELPER).exists() {
        return;
    }
    let Some(listed) = listing().await else { return };
    let (Some(name), Some(current)) = (name_in(&listed), current_in(&listed)) else { return };
    let pin = format!("{name}:{}@{}", current.mode, current.hz);
    match run("sudo", &["-n", VIDEO_MODE_HELPER, &pin], PIN_TIMEOUT).await {
        Some(_) => println!("[display] cmdline video pin → {pin}"),
        None => println!("[display] could not pin {pin} in the cmdline"),
    }
}

/// Runs before our compositor sizes its window to the panel's mode.
pub async fn prepare(hub: &Hub) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(listed) = listing().await else {
        let modes = mutter_modes().await;
        hub.update(|s| s.system.display_modes = modes);
        return;
    };
    let modes = modes_in(&listed);
    let cfg = hub.config();
    let kiosk = cfg.kiosk.main || std::env::var("LIVI_KIOSK").is_ok_and(|v| v == "1");
    if kiosk {
        let current = current_in(&listed);
        let wanted = kiosk_mode(&cfg.display_mode, current.as_ref(), &modes);
        match (wanted, name_in(&listed)) {
            (Some(mode), Some(name)) if current.as_ref().map(|c| &c.mode) != Some(&mode) => {
                apply(name, &mode, &modes).await;
            }
            (Some(_), None) => {
                println!("[display] no host output found, leaving the panel as it is")
            }
            _ => {}
        }
        pin().await;
    }
    hub.update(|s| {
        s.system.display_modes = modes;
        s.system.display_mode_settable = true;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "HDMI-A-1 \"Dell Inc. DELL U2415 (HDMI-A-1)\"\n  Enabled: yes\n  \
        Physical size: 520x320 mm\n  Modes:\n    1280x720 px, 60.000000 Hz\n    \
        1920x1080 px, 60.000000 Hz\n    1920x1200 px, 59.950001 Hz (preferred, current)\n    \
        1920x1080 px, 50.000000 Hz\n    720x576 px, 50.000000 Hz\n  Position: 0,0\n";

    fn current(mode: &str) -> Current {
        Current { mode: mode.into(), hz: 60 }
    }

    #[test]
    fn the_listing_gives_the_output_its_modes_and_the_current_one() {
        assert_eq!(name_in(LISTING), Some("HDMI-A-1"));
        assert_eq!(modes_in(LISTING), ["1920x1200", "1920x1080", "1280x720", "720x576"]);
        assert_eq!(current_in(LISTING), Some(Current { mode: "1920x1200".into(), hz: 60 }));
        assert_eq!(current_in("x\n  800x480 px, 0.000000 Hz (current)\n").unwrap().hz, 60);
        assert!(modes_in("").is_empty());
        assert_eq!(current_in(""), None);
    }

    #[test]
    fn gnome_gives_its_modes_as_sizes_of_every_monitor() {
        let mode = |w: i32, h: i32, hz: f64| {
            (format!("{w}x{h}@{hz:.3}"), w, h, hz, 1.0, vec![1.0], Props::new())
        };
        let id = |c: &str| (c.to_string(), String::new(), String::new(), String::new());
        let monitors = vec![
            (id("DP-4"), vec![mode(3840, 2160, 60.0), mode(3840, 2160, 59.982)], Props::new()),
            (id("HDMI-1"), vec![mode(1280, 720, 60.0), mode(-1, 480, 60.0)], Props::new()),
        ];
        assert_eq!(mutter_modes_in(&monitors), ["3840x2160", "1280x720"]);
        assert!(mutter_modes_in(&[]).is_empty());
    }

    #[test]
    fn the_kiosk_takes_the_configured_mode_or_snaps_down_to_720p() {
        let modes = modes_in(LISTING);
        let big = current("1920x1200");
        assert_eq!(kiosk_mode("1920x1080", Some(&big), &modes).as_deref(), Some("1920x1080"));
        assert_eq!(kiosk_mode("", Some(&big), &modes).as_deref(), Some("1280x720"));
        assert_eq!(kiosk_mode("", Some(&current("1280x720")), &modes), None);
        assert_eq!(kiosk_mode("", None, &modes), None);
        let large = ["3840x2160".to_string(), "2560x1440".to_string()];
        assert_eq!(kiosk_mode("", Some(&big), &large).as_deref(), Some("2560x1440"));
        assert_eq!(kiosk_mode("auto", Some(&big), &[]), None);
    }
}
