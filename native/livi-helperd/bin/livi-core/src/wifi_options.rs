use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use livi_core_proto::config::{Config, WifiBand};

use crate::hub::Hub;

/// Without the radio's own list, only what every regulatory domain allows: no
/// DFS, no UNII-3.
const FALLBACK_24: [u32; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const FALLBACK_5: [u32; 4] = [36, 40, 44, 48];
const ALLOWED_24: [u32; 13] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
const ALLOWED_5: [u32; 9] = [36, 40, 44, 48, 149, 153, 157, 161, 165];
/// Under this a band is a short range device allowance, not a WLAN one.
const MIN_AP_DBM: f64 = 17.0;
const FALLBACK_COUNTRIES: [&str; 43] = [
    "DE", "AT", "CH", "NL", "BE", "LU", "FR", "GB", "IE", "IT", "ES", "PT", "PL", "CZ", "SK", "HU",
    "RO", "BG", "GR", "HR", "SI", "DK", "SE", "NO", "FI", "IS", "EE", "LV", "LT", "US", "CA", "MX",
    "BR", "AU", "NZ", "JP", "KR", "CN", "IN", "ZA", "AE", "TR", "UA",
];
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);

struct Channel {
    ch: u32,
    freq: u32,
    flags: String,
    dbm: f64,
}

struct Radio {
    country: String,
    channels: Vec<Channel>,
}

fn radio_in(listing: &str, phy: &str) -> Option<Radio> {
    let mut radio = Radio { country: String::new(), channels: Vec::new() };
    let mut current = "";
    for line in listing.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("country") => radio.country = words.next().unwrap_or_default().to_string(),
            Some("phy") => current = words.next().unwrap_or_default(),
            Some("chan") if phy.is_empty() || current == phy => {
                let mut num = || words.next().and_then(|w| w.parse::<u32>().ok());
                let (Some(ch), Some(freq)) = (num(), num()) else { continue };
                let flags = words.next().unwrap_or_default().to_string();
                let dbm = words.next().and_then(|w| w.parse().ok()).unwrap_or(0.0);
                radio.channels.push(Channel { ch, freq, flags, dbm });
            }
            _ => {}
        }
    }
    (!radio.channels.is_empty()).then_some(radio)
}

fn channels_in(radio: Option<Radio>, band: WifiBand, country: &str) -> Vec<u32> {
    let (allowed, fallback): (&[u32], &[u32]) = match band {
        WifiBand::Ghz5 => (&ALLOWED_5, &FALLBACK_5),
        WifiBand::Ghz24 => (&ALLOWED_24, &FALLBACK_24),
    };
    let Some(radio) = radio else { return fallback.to_vec() };
    // The driver knows the domain it is on, not the one that was just picked.
    if !country.is_empty()
        && !radio.country.is_empty()
        && !radio.country.eq_ignore_ascii_case(country)
    {
        return fallback.to_vec();
    }
    let in_band = |f: u32| match band {
        WifiBand::Ghz5 => (4900..5900).contains(&f),
        WifiBand::Ghz24 => (2400..2500).contains(&f),
    };
    let chans: BTreeSet<u32> = radio
        .channels
        .iter()
        .filter(|c| c.flags == "ok" && !(c.dbm > 0.0 && c.dbm < MIN_AP_DBM))
        .filter(|c| in_band(c.freq) && allowed.contains(&c.ch))
        .map(|c| c.ch)
        .collect();
    if chans.is_empty() { fallback.to_vec() } else { chans.into_iter().collect() }
}

fn countries_in(dump: &str) -> Vec<String> {
    let codes: BTreeSet<String> = dump
        .lines()
        .filter_map(|l| l.strip_prefix("country ")?.split(':').next())
        .filter(|c| c.len() == 2 && *c != "00")
        .map(str::to_string)
        .collect();
    if codes.is_empty() {
        let mut all: Vec<String> = FALLBACK_COUNTRIES.iter().map(|c| c.to_string()).collect();
        all.sort();
        return all;
    }
    codes.into_iter().collect()
}

fn phy_of(iface: &str) -> String {
    if iface.is_empty() {
        return String::new();
    }
    std::fs::read_to_string(format!("/sys/class/net/{iface}/phy80211/name"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// regdbdump lives in sbin, which a desktop session does not carry in its PATH.
fn tool(name: &str) -> String {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| Path::new(p).exists())
        .unwrap_or_else(|| name.to_string())
}

async fn countries() -> Vec<String> {
    let run = tokio::process::Command::new(tool("regdbdump"))
        .arg("/lib/firmware/regulatory.db")
        .kill_on_drop(true)
        .output();
    let dump = match tokio::time::timeout(TOOL_TIMEOUT, run).await {
        Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => String::new(),
    };
    countries_in(&dump)
}

async fn channels(cfg: &Config) -> Vec<u32> {
    let phy = phy_of(&cfg.wifi_interface);
    let listing = tokio::task::spawn_blocking(livi_wifi::listing).await.ok().and_then(Result::ok);
    let radio = listing.and_then(|l| radio_in(&l, &phy));
    channels_in(radio, cfg.wifi_type, &cfg.country)
}

pub async fn follow(hub: Arc<Hub>) {
    let countries = countries().await;
    hub.update(|s| s.system.wifi_countries = countries);
    let mut state = hub.watch();
    let mut last = None;
    loop {
        let cfg = state.borrow_and_update().config.clone();
        let key = (cfg.wifi_type, cfg.country.clone(), cfg.wifi_interface.clone());
        if last.as_ref() != Some(&key) {
            let chans = channels(&cfg).await;
            hub.update(|s| s.system.wifi_channels = chans);
            last = Some(key);
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "country DE\nphy phy0\nchan 1 2412 ok 20\nchan 6 2437 ok 20\n\
        chan 12 2467 no-ir 20\nchan 36 5180 ok 23\nchan 52 5260 ok 23\nchan 149 5745 ok 14\n\
        phy phy1\nchan 11 2462 ok 20\n";

    #[test]
    fn the_channels_are_those_the_radio_may_send_an_access_point_on() {
        let radio = || radio_in(LISTING, "phy0");
        assert_eq!(channels_in(radio(), WifiBand::Ghz24, "DE"), [1, 6]);
        // 52 is DFS, 149 is too weak there.
        assert_eq!(channels_in(radio(), WifiBand::Ghz5, "de"), [36]);
        assert_eq!(channels_in(radio_in(LISTING, ""), WifiBand::Ghz24, ""), [1, 6, 11]);
    }

    #[test]
    fn without_a_fitting_radio_the_safe_channels_stay() {
        assert_eq!(channels_in(None, WifiBand::Ghz5, "DE"), FALLBACK_5);
        assert_eq!(channels_in(radio_in(LISTING, "phy0"), WifiBand::Ghz24, "US"), FALLBACK_24);
        assert!(radio_in(LISTING, "phy9").is_none());
        let only_dfs = "phy phy0\nchan 52 5260 ok 23\n";
        assert_eq!(channels_in(radio_in(only_dfs, ""), WifiBand::Ghz5, ""), FALLBACK_5);
    }

    #[test]
    fn the_countries_come_from_the_database_or_the_fallback() {
        assert_eq!(
            countries_in("country 00: DFS-UNSET\ncountry DE: DFS-ETSI\ncountry AT:"),
            ["AT", "DE"]
        );
        let fallback = countries_in("");
        assert_eq!(fallback.len(), FALLBACK_COUNTRIES.len());
        assert!(fallback.windows(2).all(|w| w[0] < w[1]));
    }
}
