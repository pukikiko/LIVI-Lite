//! LIVI-Link wifid wire: line-oriented TCP on the dongle's control port. Commands:
//!   channels | status | on | off | apply | save | down | deauth | watch
//!   set <ssid|country|channel|width|passphrase> <value>
//!   bt on | bt off
//!   iap <order>   for the Bluetooth accessory, answered the way iapd answers
//! `on`, `off` and `bt` are kept on the dongle, a boot brings back what was switched last.
//! `down` takes the access point off the air until the next apply or boot, nothing is kept.
//! `deauth` sends every station off, `deauth <count>` says how many.
//! `watch` never answers, it streams `joined <mac>` and `left <mac>` until the client goes.
//! Responses end in `ok\n` or `error <reason>\n`.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use livi_wifi::listing;

use crate::hostapd::{self, Station};
use crate::radio::{self, Radio};

const RADIO_TRIES: u32 = 40;
const RADIO_POLL: Duration = Duration::from_millis(500);
const START_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(250);
const WATCH_RETRY: Duration = Duration::from_secs(1);

const HOSTAPD: &str = "/usr/sbin/hostapd";
const IFACE: &str = "wlan0";
const BT: &str = "hci0";
const BT_DEV: u16 = 0;
/// Loads the driver and brings hci0 up with btd and iapd, for what a boot left out.
const LIVI_RADIO: &str = "/usr/bin/livi-radio";
const ACCESSORY: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, livi_net::port::ACCESSORY));
const ACCESSORY_WAIT: Duration = Duration::from_secs(3);

pub type OnSave = Box<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Standards {
    /// 802.11ac.
    pub vht: bool,
    /// 802.11ax.
    pub he: bool,
}

pub struct Ap {
    base: PathBuf,
    live: [PathBuf; 2],
    log: PathBuf,
    config: PathBuf,
    hostapd: Option<Child>,
    on_save: Option<OnSave>,
    standards: Standards,
}

impl Ap {
    pub fn new<P: Into<PathBuf>>(base: P, live: [P; 2], log: P) -> Self {
        let base = base.into();
        let live = live.map(Into::into);
        Self {
            config: base.clone(),
            base,
            live,
            log: log.into(),
            hostapd: None,
            on_save: None,
            standards: Standards::default(),
        }
    }

    pub fn with_standards(mut self, standards: Standards) -> Self {
        self.standards = standards;
        self
    }

    pub fn with_on_save(mut self, on_save: OnSave) -> Self {
        self.on_save = Some(on_save);
        self
    }
}

#[derive(Default)]
struct Wanted {
    ssid: Option<String>,
    country: Option<String>,
    channel: Option<u32>,
    width: Option<u32>,
    passphrase: Option<String>,
    /// Some(false) once the radio turned 802.11ac down, so a saved config does not ask again.
    ac: Option<bool>,
    /// The same for 802.11ax. None when never asked, so a config saved without it gets it tried
    /// once.
    ax: Option<bool>,
}

enum Cmd<'a> {
    Channels,
    Status,
    Rates,
    Set(&'a str, &'a str),
    Apply,
    Save,
    Down,
    Deauth,
    Watch,
    On,
    Off,
    Bt(bool),
    Iap(&'a str),
    Empty,
    Unknown(&'a str),
}

pub fn serve<S: std::io::Read + Write>(io: &mut S, ap: &Mutex<Ap>) {
    let mut reader = BufReader::new(io);
    let mut wanted = Wanted::default();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let answer = match command(line.trim_end_matches(['\r', '\n'])) {
            Cmd::Channels => match listing() {
                Ok(text) => format!("{text}ok\n"),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Status => status(&held(ap)),
            Cmd::Rates => rates(),
            Cmd::Set(key, value) => match remember(&mut wanted, key, value) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Apply => {
                let answer = match apply(&mut held(ap), &wanted) {
                    Ok(()) => "ok\n".into(),
                    Err(e) => format!("error {e}\n"),
                };
                wanted = Wanted::default();
                answer
            }
            Cmd::On => {
                let mut ap = held(ap);
                keep(&ap, Radio::Wifi, true);
                match on(&mut ap) {
                    Ok(()) => "ok\n".into(),
                    Err(e) => format!("error {e}\n"),
                }
            }
            Cmd::Off => {
                let mut ap = held(ap);
                keep(&ap, Radio::Wifi, false);
                deauth();
                off(&mut ap);
                "ok\n".into()
            }
            Cmd::Deauth => format!("deauth {}\nok\n", deauth()),
            Cmd::Watch => {
                watch(reader.get_mut());
                return;
            }
            Cmd::Save => match save(&held(ap)) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("error {e}\n"),
            },
            Cmd::Down => {
                stop(&mut held(ap));
                println!("[wifid] access point down until the next apply");
                "ok\n".into()
            }
            Cmd::Bt(up) => {
                let ap = held(ap);
                keep(&ap, Radio::Bt, up);
                match bluetooth(up) {
                    Ok(()) => "ok\n".into(),
                    Err(e) => format!("error {e}\n"),
                }
            }
            // Never waits for the access point, an apply can take half a minute
            Cmd::Iap(order) => accessory(ACCESSORY, order),
            Cmd::Empty => continue,
            Cmd::Unknown(what) => format!("error unknown command {what}\n"),
        };
        if reader.get_mut().write_all(answer.as_bytes()).is_err() {
            return;
        }
    }
}

fn command(line: &str) -> Cmd<'_> {
    let line = line.trim();
    let (head, rest) = line.split_once(' ').unwrap_or((line, ""));
    match head {
        "" => Cmd::Empty,
        "channels" => Cmd::Channels,
        "status" => Cmd::Status,
        "rates" => Cmd::Rates,
        "apply" => Cmd::Apply,
        "save" => Cmd::Save,
        "down" => Cmd::Down,
        "deauth" => Cmd::Deauth,
        "watch" => Cmd::Watch,
        "on" => Cmd::On,
        "off" => Cmd::Off,
        "bt" => match rest.trim() {
            "on" => Cmd::Bt(true),
            "off" => Cmd::Bt(false),
            _ => Cmd::Unknown(line),
        },
        "iap" if !rest.trim().is_empty() => Cmd::Iap(rest.trim()),
        "set" => match rest.split_once(' ') {
            Some((key, value)) => Cmd::Set(key, value),
            None => Cmd::Unknown(line),
        },
        _ => Cmd::Unknown(head),
    }
}

fn held(ap: &Mutex<Ap>) -> MutexGuard<'_, Ap> {
    ap.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accessory(at: SocketAddr, order: &str) -> String {
    ask(at, order).unwrap_or_else(|e| format!("error accessory: {e}\n"))
}

fn ask(at: SocketAddr, order: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&at, ACCESSORY_WAIT)?;
    stream.set_read_timeout(Some(ACCESSORY_WAIT))?;
    writeln!(stream, "{order}")?;
    let mut reader = BufReader::new(stream);
    let (mut answer, mut line) = (String::new(), String::new());
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        answer.push_str(&line);
        if line == "ok\n" || line.starts_with("error ") {
            return Ok(answer);
        }
    }
}

fn remember(wanted: &mut Wanted, key: &str, value: &str) -> Result<(), String> {
    if value.contains(['\n', '\r']) {
        return Err("a value holds a line break".into());
    }
    match key {
        "ssid" => {
            if value.is_empty() || value.len() > 32 {
                return Err("ssid must be 1 to 32 bytes".into());
            }
            wanted.ssid = Some(value.to_string());
        }
        "country" => {
            if value.len() != 2 || !value.bytes().all(|b| b.is_ascii_alphabetic()) {
                return Err("country must be two letters".into());
            }
            wanted.country = Some(value.to_ascii_uppercase());
        }
        "channel" => {
            let channel = value.parse::<u32>().map_err(|_| "channel must be a number")?;
            if !(1..=196).contains(&channel) {
                return Err("channel is out of range".into());
            }
            wanted.channel = Some(channel);
        }
        "width" => {
            let width = value.parse::<u32>().map_err(|_| "width must be a number")?;
            if ![20, 40, 80].contains(&width) {
                return Err("width must be 20, 40 or 80".into());
            }
            wanted.width = Some(width);
        }
        "passphrase" => {
            if !(8..=63).contains(&value.len()) {
                return Err("passphrase must be 8 to 63 bytes".into());
            }
            wanted.passphrase = Some(value.to_string());
        }
        other => eprintln!("[wifid] setting {other} is not known here, dropped"),
    }
    Ok(())
}

fn config(base: &str, wanted: &Wanted, standards: Standards) -> String {
    // The base pins vht_oper_centr_freq_seg0_idx to its own channel and hostapd refuses any
    // other, so the channel lines are regenerated for the wanted channel and width.
    let mut out = String::new();
    for line in base.lines() {
        let replaced = match setting(line) {
            Some("ssid") => wanted.ssid.is_some(),
            Some("country_code") => wanted.country.is_some(),
            Some(
                "channel"
                | "hw_mode"
                | "ht_capab"
                | "vendor_elements"
                | "assocresp_elements"
                | "ieee80211ac"
                | "vht_capab"
                | "vht_oper_chwidth"
                | "vht_oper_centr_freq_seg0_idx"
                | "ieee80211ax"
                | "he_oper_chwidth"
                | "he_oper_centr_freq_seg0_idx",
            ) => wanted.channel.is_some(),
            Some("wpa_passphrase") => wanted.passphrase.is_some(),
            Some("ctrl_interface") => true,
            _ => false,
        };
        if !replaced {
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(country) = &wanted.country {
        out.push_str(&format!("country_code={country}\n"));
    }
    if let Some(channel) = wanted.channel {
        let ie = apple_ie(channel);
        let width = wanted.width.unwrap_or(40);
        let ht_capab = if width >= 40 {
            format!("[SHORT-GI-20][SHORT-GI-40]{}", ht40(channel))
        } else {
            "[SHORT-GI-20]".to_string()
        };
        out.push_str(&format!(
            "hw_mode={}\nchannel={channel}\nht_capab={ht_capab}\n\
             vendor_elements={ie}\nassocresp_elements={ie}\n",
            band(channel)
        ));
        if standards.vht && wanted.ac != Some(false) && band(channel) == "a" {
            let centre = vht_centre(channel).filter(|_| width >= 80);
            match centre {
                Some(centre) => out.push_str(&format!(
                    "ieee80211ac=1\nvht_capab=[SHORT-GI-80]\nvht_oper_chwidth=1\n\
                     vht_oper_centr_freq_seg0_idx={centre}\n"
                )),
                None => out.push_str("ieee80211ac=1\nvht_oper_chwidth=0\n"),
            }
            match (standards.he, wanted.ax, centre) {
                (false, ..) => {}
                (true, Some(false), _) => out.push_str("ieee80211ax=0\n"),
                (true, _, Some(centre)) => out.push_str(&format!(
                    "ieee80211ax=1\nhe_oper_chwidth=1\nhe_oper_centr_freq_seg0_idx={centre}\n"
                )),
                (true, _, None) => out.push_str("ieee80211ax=1\nhe_oper_chwidth=0\n"),
            }
        }
    }
    if let Some(ssid) = &wanted.ssid {
        out.push_str(&format!("ssid={ssid}\n"));
    }
    if let Some(passphrase) = &wanted.passphrase {
        out.push_str(&format!("wpa_passphrase={passphrase}\n"));
    }
    out.push_str(&ctrl_line());
    out
}

fn ctrl_line() -> String {
    format!("ctrl_interface={}\n", hostapd::CTRL_DIR)
}

/// Without the control socket line no deauth reaches hostapd.
fn with_ctrl(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if text.lines().any(|line| setting(line) == Some("ctrl_interface")) {
        return Ok(());
    }
    let gap = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    std::fs::write(path, format!("{text}{gap}{}", ctrl_line()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn setting(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(key, _)| key.trim())
}

fn await_radio() -> Result<(), String> {
    let path = format!("/sys/class/net/{IFACE}");
    for _ in 0..RADIO_TRIES {
        if std::path::Path::new(&path).exists() {
            return Ok(());
        }
        std::thread::sleep(RADIO_POLL);
    }
    Err(format!("{IFACE} never appeared"))
}

fn band(channel: u32) -> &'static str {
    if channel <= 14 { "g" } else { "a" }
}

fn apple_ie(channel: u32) -> String {
    let band_bit: u8 = if channel >= 36 { 0x01 } else { 0x02 };
    format!("dd0800a04000000200{:02x}", 0x20 | band_bit)
}

fn vht_centre(channel: u32) -> Option<u32> {
    match channel {
        36..=48 => Some(42),
        149..=161 => Some(155),
        _ => None,
    }
}

fn ht40(channel: u32) -> &'static str {
    let up = if channel <= 14 { channel <= 7 } else { (channel / 4) % 2 == 1 };
    if up { "[HT40+]" } else { "[HT40-]" }
}

fn apply(ap: &mut Ap, wanted: &Wanted) -> Result<(), String> {
    if !radio::enabled(Radio::Wifi) {
        return Err("wifi is switched off".into());
    }
    await_radio()?;
    let base =
        std::fs::read_to_string(&ap.base).map_err(|e| format!("{}: {e}", ap.base.display()))?;
    if let Some(parent) = ap.live[0].parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = config(&base, wanted, ap.standards);
    if running() && std::fs::read_to_string(&ap.config).is_ok_and(|current| current == text) {
        return Ok(());
    }
    let next = if ap.config == ap.live[0] { ap.live[1].clone() } else { ap.live[0].clone() };
    std::fs::write(&next, text).map_err(|e| format!("{}: {e}", next.display()))?;

    let previous = ap.config.clone();
    stop(ap);
    if let Err(refused) = start_weakening(ap, &next) {
        // Removed, so the newest live file is always the one the radio runs on.
        let _ = std::fs::remove_file(&next);
        stop(ap);
        if start(ap, &previous).is_err() {
            stop(ap);
            let base = ap.base.clone();
            let _ = start(ap, &base);
            ap.config = ap.base.clone();
        }
        return Err(refused);
    }
    ap.config = next;
    Ok(())
}

fn save(ap: &Ap) -> Result<(), String> {
    if ap.config == ap.base {
        return Ok(());
    }
    let live =
        std::fs::read_to_string(&ap.config).map_err(|e| format!("{}: {e}", ap.config.display()))?;
    let base =
        std::fs::read_to_string(&ap.base).map_err(|e| format!("{}: {e}", ap.base.display()))?;
    let next = config(&base, &settings_of(&live), ap.standards);
    if next == base {
        return Ok(());
    }
    let temp = ap.base.with_extension("new");
    std::fs::write(&temp, next).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &ap.base).map_err(|e| format!("{}: {e}", ap.base.display()))?;
    let _ = Command::new("sync").status();
    if let Some(cb) = ap.on_save.as_ref() {
        cb();
    }
    Ok(())
}

fn settings_of(text: &str) -> Wanted {
    let value = |key: &str| {
        text.lines()
            .rfind(|line| setting(line) == Some(key))
            .and_then(|line| line.split_once('='))
            .map(|(_, value)| value.trim().to_string())
    };
    Wanted {
        ssid: value("ssid"),
        country: value("country_code"),
        channel: value("channel").and_then(|c| c.parse().ok()),
        width: Some(if value("vht_oper_chwidth").as_deref() == Some("1") {
            80
        } else if value("ht_capab").is_some_and(|c| c.contains("[HT40")) {
            40
        } else {
            20
        }),
        passphrase: value("wpa_passphrase"),
        ac: Some(value("ieee80211ac").as_deref() == Some("1")),
        ax: value("ieee80211ax").map(|ax| ax == "1"),
    }
}

fn on(ap: &mut Ap) -> Result<(), String> {
    if running() {
        return Ok(());
    }
    driver();
    await_radio()?;
    // The address it kept from its first boot, set while the interface is still down.
    let _ = Command::new(LIVI_RADIO).arg("mac").status();
    let _ = Command::new("ifconfig").args([IFACE, "up"]).status();
    let config = ap.config.clone();
    start_weakening(ap, &config)
}

/// Rewrites `path` to the config the radio finally accepts.
fn start_weakening(ap: &mut Ap, path: &std::path::Path) -> Result<(), String> {
    let refused = match start(ap, path) {
        Ok(()) => return Ok(()),
        Err(refused) => refused,
    };
    let mut text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    while let Some((next, step)) = weaker(&text) {
        eprintln!("[wifid] radio refused ({refused}), trying {step}");
        stop(ap);
        std::fs::write(path, &next).map_err(|e| format!("{}: {e}", path.display()))?;
        if start(ap, path).is_ok() {
            return Ok(());
        }
        text = next;
    }
    Err(refused)
}

/// `ieee80211ax=0` stays in, so a saved config remembers that the radio refused it.
fn weaker(config: &str) -> Option<(String, &'static str)> {
    let value = |key: &str| {
        config
            .lines()
            .rfind(|line| setting(line) == Some(key))
            .and_then(|line| line.split_once('='))
            .map(|(_, value)| value.trim().to_string())
    };
    let channel: u32 = value("channel")?.parse().ok()?;
    let mut lines: Vec<String> = config.lines().map(str::to_string).collect();
    let mut set = |key: &str, to: Option<&str>| {
        let had = lines.iter().any(|line| setting(line) == Some(key));
        lines.retain(|line| setting(line) != Some(key));
        if let Some(to) = to.filter(|_| had) {
            lines.push(format!("{key}={to}"));
        }
    };
    let step = if value("vht_oper_chwidth").as_deref() == Some("1") {
        set("vht_oper_chwidth", Some("0"));
        set("vht_oper_centr_freq_seg0_idx", None);
        set("vht_capab", None);
        set("he_oper_chwidth", Some("0"));
        set("he_oper_centr_freq_seg0_idx", None);
        "40 MHz"
    } else if value("ieee80211ax").as_deref() == Some("1") {
        set("ieee80211ax", Some("0"));
        set("he_oper_chwidth", None);
        set("he_oper_centr_freq_seg0_idx", None);
        "802.11ac"
    } else if value("ieee80211ac").as_deref() == Some("1") {
        for key in ["ieee80211ac", "vht_capab", "vht_oper_chwidth", "vht_oper_centr_freq_seg0_idx"]
        {
            set(key, None);
        }
        "802.11n"
    } else if value("ht_capab").is_some_and(|c| c.contains("[HT40")) {
        set("ht_capab", Some("[SHORT-GI-20]"));
        "20 MHz"
    } else if band(channel) == "a" {
        let ie = apple_ie(6);
        set("hw_mode", Some("g"));
        set("channel", Some("6"));
        set("ht_capab", Some("[SHORT-GI-20]"));
        set("vendor_elements", Some(ie.as_str()));
        set("assocresp_elements", Some(ie.as_str()));
        "2.4 GHz channel 6"
    } else {
        return None;
    };
    Some((lines.join("\n") + "\n", step))
}

fn deauth() -> usize {
    if !running() {
        return 0;
    }
    match hostapd::deauth_all(&hostapd::ctrl(IFACE)) {
        Ok(count) => {
            println!("[wifid] deauthenticated {count} station(s)");
            count
        }
        Err(e) => {
            eprintln!("[wifid] deauth: {e}");
            0
        }
    }
}

fn watch<W: Write>(out: &mut W) {
    let ctrl = hostapd::ctrl(IFACE);
    loop {
        let attached = hostapd::watch(&ctrl, |station| {
            let line = match station {
                Some(Station::Joined(mac)) => format!("joined {mac}\n"),
                Some(Station::Left(mac)) => format!("left {mac}\n"),
                None => "\n".to_string(),
            };
            out.write_all(line.as_bytes()).is_ok()
        });
        if attached.is_ok() || out.write_all(b"\n").is_err() {
            return;
        }
        std::thread::sleep(WATCH_RETRY);
    }
}

fn off(ap: &mut Ap) {
    stop(ap);
    let _ = Command::new("ifconfig").args([IFACE, "down"]).status();
}

fn bluetooth(up: bool) -> Result<(), String> {
    if !up {
        // btd first: while a host tunnels, it holds hci0 and hci0 will not go down.
        livi_radio("bt-off", "btd and iapd would not stop")?;
        return livi_btd::hci::down(BT_DEV).map_err(|e| format!("{BT} would not go down: {e}"));
    }
    driver();
    // A boot with Bluetooth off started neither btd nor iapd, they only run once hci0 is up.
    livi_radio("bt", &format!("{BT} would not come up"))
}

fn livi_radio(command: &str, failed: &str) -> Result<(), String> {
    match Command::new(LIVI_RADIO).arg(command).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(failed.into()),
        Err(e) => Err(format!("{LIVI_RADIO}: {e}")),
    }
}

fn keep(ap: &Ap, radio: Radio, on: bool) {
    match radio::set(radio, on) {
        Ok(true) => {
            if let Some(cb) = ap.on_save.as_ref() {
                cb();
            }
        }
        Ok(false) => {}
        Err(e) => eprintln!("[wifid] {}: {e}", radio::PATH),
    }
}

/// With WiFi and Bluetooth both off at boot nothing loaded the driver.
fn driver() {
    let loaded = std::path::Path::new(&format!("/sys/class/net/{IFACE}")).exists()
        || std::path::Path::new(&format!("/sys/class/bluetooth/{BT}")).exists();
    if !loaded {
        let _ = Command::new(LIVI_RADIO).arg("driver").status();
    }
}

fn bt_up() -> bool {
    livi_btd::hci::is_up(BT_DEV)
}

/// The newest live file is the one the radio runs on.
pub fn ap_config_from(base: &std::path::Path, live: &[&std::path::Path]) -> Option<String> {
    let newest = live
        .iter()
        .copied()
        .filter_map(|file| {
            let modified = std::fs::metadata(file).and_then(|m| m.modified()).ok()?;
            Some((modified, file))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, file)| file)
        .unwrap_or(base);
    std::fs::read_to_string(newest).ok()
}

pub fn ap_name_from(base: &std::path::Path, live: &[&std::path::Path]) -> Option<String> {
    let text = ap_config_from(base, live)?;
    text.lines()
        .filter(|line| setting(line) == Some("ssid"))
        .filter_map(|line| line.split_once('=').map(|(_, v)| v.trim().to_string()))
        .find(|value| !value.is_empty())
}

fn status(ap: &Ap) -> String {
    let mut out = String::new();
    out.push_str(if running() { "state on\n" } else { "state off\n" });
    out.push_str(if bt_up() { "bt on\n" } else { "bt off\n" });
    let switch = |radio| if radio::enabled(radio) { "on" } else { "off" };
    out.push_str(&format!(
        "wifi-enabled {}\nbt-enabled {}\n",
        switch(Radio::Wifi),
        switch(Radio::Bt)
    ));
    if let Ok(mac) = std::fs::read_to_string(format!("/sys/class/net/{IFACE}/address")) {
        out.push_str(&format!("mac {}\n", mac.trim()));
    }
    if let Ok(mac) = std::fs::read_to_string(format!("/sys/class/bluetooth/{BT}/address")) {
        out.push_str(&format!("btmac {}\n", mac.trim()));
    }
    out.push_str(if ap.config == ap.base { "config fallback\n" } else { "config host\n" });
    if let Ok(text) = std::fs::read_to_string(&ap.config) {
        for line in text.lines() {
            if let Some(key @ ("ssid" | "country_code" | "channel" | "hw_mode")) = setting(line) {
                let value = line.split_once('=').map(|(_, v)| v).unwrap_or("");
                out.push_str(&format!("{key} {value}\n"));
            }
        }
    }
    let counter = |dir: &str| {
        std::fs::read_to_string(format!("/sys/class/net/{IFACE}/statistics/{dir}_bytes"))
            .ok()
            .map(|s| s.trim().to_string())
    };
    // RX is what the AP received from the phone (down), TX what it sent (up).
    if let Some(bytes) = counter("rx") {
        out.push_str(&format!("downbytes {bytes}\n"));
    }
    if let Some(bytes) = counter("tx") {
        out.push_str(&format!("upbytes {bytes}\n"));
    }
    out.push_str("ok\n");
    out
}

/// Seen from the car: down is phone to car, up is car to phone.
fn rates() -> String {
    match livi_wifi::stations(IFACE).rates {
        Some((down, up)) => format!("downrate {down}\nuprate {up}\nok\n"),
        None => "ok\n".into(),
    }
}

fn start(ap: &mut Ap, config: &std::path::Path) -> Result<(), String> {
    with_ctrl(config)?;
    let _ = std::fs::remove_file(&ap.log);
    let log = std::fs::File::create(&ap.log).map_err(|e| format!("{}: {e}", ap.log.display()))?;
    let errors = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(HOSTAPD)
        .arg(config)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(errors)
        .spawn()
        .map_err(|e| format!("hostapd: {e}"))?;

    let deadline = std::time::Instant::now() + START_TIMEOUT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(POLL);
        let text = std::fs::read_to_string(&ap.log).unwrap_or_default();
        if text.contains("AP-ENABLED") {
            ap.hostapd = Some(child);
            return Ok(());
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Err(complaint(&text));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("hostapd did not bring the radio up".into())
}

fn stop(ap: &mut Ap) {
    let _ = Command::new("killall").arg("hostapd").status();
    if let Some(mut child) = ap.hostapd.take() {
        let _ = child.wait();
    }
    for _ in 0..20 {
        if !running() {
            return;
        }
        std::thread::sleep(POLL);
    }
}

fn complaint(log: &str) -> String {
    const MARKERS: [&str; 5] = ["not allowed", "Could not", "Unable", "Invalid", "ailed"];
    log.lines()
        .map(str::trim)
        .find(|line| MARKERS.iter().any(|m| line.contains(m)))
        .or_else(|| log.lines().map(str::trim).rev().find(|line| !line.is_empty()))
        .unwrap_or("hostapd failed")
        .to_string()
}

fn running() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm"))
            && comm.trim() == "hostapd"
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "interface=wlan0\nssid=LIVI-Link\nhw_mode=a\nchannel=36\nieee80211ac=1\n\
        ht_capab=[HT40+][SHORT-GI-20][SHORT-GI-40]\nvht_capab=[SHORT-GI-80]\n\
        vht_oper_chwidth=1\nvht_oper_centr_freq_seg0_idx=42\nwpa_passphrase=livilink\n";

    const N_ONLY: Standards = Standards { vht: false, he: false };
    const AC_ONLY: Standards = Standards { vht: true, he: false };
    const AC_AX: Standards = Standards { vht: true, he: true };

    fn wanted(channel: u32, width: Option<u32>) -> Wanted {
        Wanted { channel: Some(channel), width, ..Wanted::default() }
    }

    fn lines(config: &str) -> Vec<&str> {
        config.lines().collect()
    }

    #[test]
    fn eighty_megahertz_gets_its_centre_where_the_block_needs_no_dfs() {
        let out = config(BASE, &wanted(149, Some(80)), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"vht_oper_chwidth=1"));
        assert!(out.contains(&"vht_oper_centr_freq_seg0_idx=155"));
        assert!(out.contains(&"ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]"));
        assert!(!out.contains(&"vht_oper_centr_freq_seg0_idx=42"));
    }

    #[test]
    fn eighty_megahertz_on_a_dfs_channel_stays_at_forty() {
        let out = config(BASE, &wanted(100, Some(80)), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"vht_oper_chwidth=0"));
        assert!(!out.iter().any(|l| l.starts_with("vht_oper_centr_freq_seg0_idx")));
    }

    #[test]
    fn a_host_that_names_no_width_gets_forty() {
        let out = config(BASE, &wanted(36, None), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"vht_oper_chwidth=0"));
        assert!(out.contains(&"ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]"));
    }

    #[test]
    fn twenty_megahertz_drops_the_secondary_channel() {
        let out = config(BASE, &wanted(36, Some(20)), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"ht_capab=[SHORT-GI-20]"));
        assert!(out.contains(&"vht_oper_chwidth=0"));
    }

    #[test]
    fn a_saved_config_keeps_its_width() {
        for width in [20, 40, 80] {
            let live = config(BASE, &wanted(36, Some(width)), AC_AX);
            assert_eq!(settings_of(&live).width, Some(width));
        }
    }

    #[test]
    fn a_base_that_lost_its_vht_lines_gets_them_back() {
        let worn = "interface=wlan0\nssid=LIVI\nieee80211n=1\nchannel=36\n\
            ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]\n";
        let out = config(worn, &wanted(36, Some(80)), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"ieee80211ac=1"));
        assert!(out.contains(&"vht_oper_chwidth=1"));
        assert!(out.contains(&"vht_oper_centr_freq_seg0_idx=42"));
    }

    #[test]
    fn a_radio_without_vht_never_gets_it() {
        let out = config(BASE, &wanted(36, Some(80)), N_ONLY);
        let out = lines(&out);
        assert!(!out.iter().any(|l| l.starts_with("ieee80211a") || l.starts_with("vht_")));
        assert!(!out.iter().any(|l| l.starts_with("he_")));
        assert!(out.contains(&"ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]"));
    }

    #[test]
    fn a_radio_without_he_gets_802_11ac_but_never_802_11ax() {
        let worn = "interface=wlan0\nchannel=36\nieee80211ax=1\nhe_oper_chwidth=1\n";
        let out = config(worn, &wanted(36, Some(80)), AC_ONLY);
        let out = lines(&out);
        assert!(out.contains(&"ieee80211ac=1"));
        assert!(out.contains(&"vht_oper_chwidth=1"));
        assert!(!out.iter().any(|l| l.starts_with("ieee80211ax") || l.starts_with("he_")));
    }

    #[test]
    fn eighty_megahertz_asks_for_802_11ax_on_the_same_block() {
        let out = config(BASE, &wanted(149, Some(80)), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"ieee80211ax=1"));
        assert!(out.contains(&"he_oper_chwidth=1"));
        assert!(out.contains(&"he_oper_centr_freq_seg0_idx=155"));
    }

    #[test]
    fn forty_megahertz_asks_for_802_11ax_without_a_centre() {
        let out = config(BASE, &wanted(36, Some(40)), AC_AX);
        let out = lines(&out);
        assert!(out.contains(&"ieee80211ax=1"));
        assert!(out.contains(&"he_oper_chwidth=0"));
        assert!(!out.iter().any(|l| l.starts_with("he_oper_centr_freq_seg0_idx")));
    }

    #[test]
    fn a_refusing_radio_is_asked_for_less_step_by_step() {
        let mut text = config(BASE, &wanted(36, Some(80)), AC_AX);
        let mut steps = Vec::new();
        while let Some((next, step)) = weaker(&text) {
            steps.push(step);
            text = next;
        }
        assert_eq!(steps, ["40 MHz", "802.11ac", "802.11n", "20 MHz", "2.4 GHz channel 6"]);
        let ie = format!("vendor_elements={}", apple_ie(6));
        let out = lines(&text);
        assert!(out.contains(&"hw_mode=g"));
        assert!(out.contains(&"channel=6"));
        assert!(out.contains(&"ht_capab=[SHORT-GI-20]"));
        assert!(out.contains(&ie.as_str()));
        assert!(!out.iter().any(|l| l.starts_with("ieee80211ac") || l.starts_with("vht_")));
        assert!(!out.iter().any(|l| l.starts_with("he_") || *l == "ieee80211ax=1"));
        assert!(out.contains(&"wpa_passphrase=livilink"));
    }

    #[test]
    fn forty_megahertz_keeps_802_11ax_until_the_radio_refuses_that_too() {
        let live = config(BASE, &wanted(36, Some(80)), AC_AX);
        let (forty, _) = weaker(&live).unwrap();
        let forty = lines(&forty);
        assert!(forty.contains(&"ieee80211ax=1"));
        assert!(forty.contains(&"he_oper_chwidth=0"));
        assert!(!forty.iter().any(|l| l.starts_with("he_oper_centr_freq_seg0_idx")));
    }

    #[test]
    fn a_config_the_radio_ran_without_ax_is_saved_without_asking_again() {
        let live = config(BASE, &wanted(36, Some(80)), AC_AX);
        let (forty, _) = weaker(&live).unwrap();
        let (ac, _) = weaker(&forty).unwrap();
        let saved = config(BASE, &settings_of(&ac), AC_AX);
        let saved = lines(&saved);
        assert!(saved.contains(&"ieee80211ax=0"));
        assert!(!saved.iter().any(|l| l.starts_with("he_")));
        assert!(saved.contains(&"ieee80211ac=1"));
    }

    #[test]
    fn a_config_saved_before_802_11ax_gets_it_tried() {
        let before = config(BASE, &Wanted { ax: Some(false), ..wanted(36, Some(80)) }, AC_AX)
            .replace("ieee80211ax=0\n", "");
        let out = config(BASE, &settings_of(&before), AC_AX);
        assert!(lines(&out).contains(&"ieee80211ax=1"));
    }

    #[test]
    fn a_config_the_radio_ran_without_ac_is_saved_without_it() {
        let live = config(BASE, &wanted(36, Some(80)), AC_AX);
        let (forty, _) = weaker(&live).unwrap();
        let (ac, _) = weaker(&forty).unwrap();
        let (n, _) = weaker(&ac).unwrap();
        let saved = config(BASE, &settings_of(&n), AC_AX);
        let saved = lines(&saved);
        assert!(!saved.iter().any(|l| l.starts_with("ieee80211ac") || l.starts_with("vht_")));
        assert!(!saved.iter().any(|l| l.starts_with("he_") || *l == "ieee80211ax=1"));
        assert!(saved.contains(&"ht_capab=[SHORT-GI-20][SHORT-GI-40][HT40+]"));
    }

    #[test]
    fn a_setting_this_dongle_does_not_know_is_dropped_not_refused() {
        let mut w = Wanted::default();
        assert!(remember(&mut w, "he_bss_color", "12").is_ok());
        assert!(remember(&mut w, "channel", "44").is_ok());
        assert_eq!(w.channel, Some(44));
    }

    #[test]
    fn only_the_three_widths_are_taken() {
        let mut w = Wanted::default();
        assert!(remember(&mut w, "width", "80").is_ok());
        assert_eq!(w.width, Some(80));
        assert!(remember(&mut w, "width", "160").is_err());
        assert!(remember(&mut w, "width", "wide").is_err());
    }

    fn iapd(answer: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let heard = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut order = String::new();
            BufReader::new(&stream).read_line(&mut order).unwrap();
            stream.write_all(answer.as_bytes()).unwrap();
            order
        });
        (at, heard)
    }

    #[test]
    fn down_is_a_command_of_its_own() {
        assert!(matches!(command("down"), Cmd::Down));
    }

    #[test]
    fn the_link_rates_are_a_command_of_their_own() {
        assert!(matches!(command("rates"), Cmd::Rates));
    }

    #[test]
    fn deauth_and_watch_are_commands() {
        assert!(matches!(command("deauth"), Cmd::Deauth));
        assert!(matches!(command("watch"), Cmd::Watch));
    }

    #[test]
    fn every_config_opens_the_control_socket_once() {
        let worn = format!("{BASE}ctrl_interface=/var/run/hostapd\n");
        let out = config(&worn, &wanted(36, Some(80)), AC_AX);
        assert_eq!(out.matches("ctrl_interface=").count(), 1);
        assert!(out.contains("ctrl_interface=/tmp/livi/hostapd\n"));
    }

    #[test]
    fn an_old_config_gets_the_control_socket_before_hostapd_starts() {
        let path = std::env::temp_dir().join(format!("hostapd-ctrl-{}.conf", std::process::id()));
        std::fs::write(&path, "interface=wlan0\nssid=LIVI").unwrap();
        with_ctrl(&path).unwrap();
        with_ctrl(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "interface=wlan0\nssid=LIVI\nctrl_interface=/tmp/livi/hostapd\n"
        );
        let _ = std::fs::remove_file(&path);
        assert!(with_ctrl(&path).is_err());
    }

    #[test]
    fn an_iap_order_goes_to_the_accessory_and_comes_back_whole() {
        assert!(matches!(command("iap targets aa bb"), Cmd::Iap("targets aa bb")));
        assert!(matches!(command("iap"), Cmd::Unknown("iap")));

        let (at, heard) = iapd("bonds 1\noffered on\ntargets 0\nok\n");
        assert_eq!(accessory(at, "status"), "bonds 1\noffered on\ntargets 0\nok\n");
        assert_eq!(heard.join().unwrap(), "status\n");

        let (at, _) = iapd("error not an address\n");
        assert_eq!(accessory(at, "disconnect x"), "error not an address\n");
    }

    #[test]
    fn an_accessory_that_is_gone_or_hangs_up_is_an_error() {
        let (at, _) = iapd("bonds 1\n");
        assert!(accessory(at, "status").starts_with("error accessory: "));
        let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert!(accessory(gone, "on").starts_with("error accessory: "));
    }
}
