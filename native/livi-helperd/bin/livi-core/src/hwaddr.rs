use std::fs;
use std::path::Path;
use std::process::Command;

const BT_SYSFS: &str = "/sys/class/bluetooth";
const NET_SYSFS: &str = "/sys/class/net";
pub const FALLBACK: &str = "AA:BB:CC:DD:EE:FF";

fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn mac_in(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let raw = raw.trim();
    is_mac(raw).then(|| raw.to_uppercase())
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// The LIVI Link's controller sits on vhci.
fn tunnelled(dir: &Path, hci: &str) -> bool {
    fs::canonicalize(dir.join(hci)).is_ok_and(|p| p.to_string_lossy().contains("/devices/virtual/"))
}

fn bt_mac_in(dir: &Path, adapter: &str) -> Result<Option<String>, ()> {
    let adapter = if adapter == livi_link_host::link::CHOICE {
        match entries(dir).into_iter().find(|n| tunnelled(dir, n)) {
            Some(hci) => hci,
            None => return Err(()),
        }
    } else {
        adapter.to_string()
    };
    let candidates = if adapter.is_empty() {
        entries(dir).into_iter().filter(|n| n.starts_with("hci")).collect()
    } else {
        vec![adapter]
    };
    Ok(candidates.iter().find_map(|name| mac_in(&dir.join(name).join("address"))))
}

/// BlueZ knows the address where sysfs does not show it.
fn busctl(adapter: &str) -> Option<String> {
    let out = Command::new("busctl")
        .args(["--system", "get-property", "org.bluez"])
        .arg(format!("/org/bluez/{adapter}"))
        .args(["org.bluez.Adapter1", "Address"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let quoted = text.split('"').nth(1)?;
    is_mac(quoted).then(|| quoted.to_uppercase())
}

pub fn bt_mac(adapter: &str) -> Option<String> {
    if let Ok(mac) = std::env::var("AA_BT_MAC") {
        return Some(mac);
    }
    // Without a tunnelled controller the dongle names its own.
    if adapter == livi_link_host::link::CHOICE && !cfg!(target_os = "linux") {
        return livi_link_host::ap::bt_mac()
            .map(|b| b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(":"));
    }
    match bt_mac_in(Path::new(BT_SYSFS), adapter) {
        Ok(Some(mac)) => Some(mac),
        Ok(None) => busctl(if adapter.is_empty() { "hci0" } else { adapter }),
        Err(()) => None,
    }
}

fn wifi_mac_in(dir: &Path, iface: &str) -> Option<String> {
    let candidates = if iface.is_empty() {
        entries(dir).into_iter().filter(|n| n.starts_with("wlan")).collect()
    } else {
        vec![iface.to_string()]
    };
    candidates.iter().find_map(|name| mac_in(&dir.join(name).join("address")))
}

fn wifi_interfaces_in(dir: &Path) -> Vec<String> {
    entries(dir).into_iter().filter(|n| dir.join(n).join("wireless").exists()).collect()
}

pub fn wifi_interfaces() -> Vec<String> {
    wifi_interfaces_in(Path::new(NET_SYSFS))
}

fn bt_adapters_in(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|n| {
            n.strip_prefix("hci")
                .is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
                && !tunnelled(dir, n)
        })
        .collect()
}

pub fn bt_adapters() -> Vec<String> {
    bt_adapters_in(Path::new(BT_SYSFS))
}

/// Asking the dongle blocks for up to its timeout.
pub fn accessory_id(wifi_interface: &str) -> Option<String> {
    if let Ok(mac) = std::env::var("AA_WIFI_BSSID") {
        return Some(mac);
    }
    if wifi_interface == livi_link_host::link::CHOICE {
        return livi_link_host::ap::mac().map(|m| m.to_uppercase());
    }
    wifi_mac_in(Path::new(NET_SYSFS), wifi_interface)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_file::tests::TempDir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn macs_are_six_hex_pairs() {
        assert!(is_mac("aa:bb:cc:dd:ee:ff"));
        assert!(!is_mac("aa:bb:cc:dd:ee"));
        assert!(!is_mac("aa:bb:cc:dd:ee:fg"));
        assert!(!is_mac("aab:b:cc:dd:ee:ff"));
    }

    #[test]
    fn bluetooth_comes_from_the_named_or_the_first_adapter() {
        let dir = TempDir::new();
        write(&dir.0.join("hci1/address"), "11:22:33:44:55:66\n");
        write(&dir.0.join("hci0/address"), "garbage");
        assert_eq!(bt_mac_in(&dir.0, "hci1"), Ok(Some("11:22:33:44:55:66".into())));
        assert_eq!(bt_mac_in(&dir.0, ""), Ok(Some("11:22:33:44:55:66".into())));
        assert_eq!(bt_mac_in(&dir.0, "hci9"), Ok(None));
        assert_eq!(bt_mac_in(&dir.0, livi_link_host::link::CHOICE), Err(()));
    }

    #[test]
    fn the_lists_hold_the_wifi_interfaces_and_the_local_controllers() {
        let dir = TempDir::new();
        let net = dir.0.join("net");
        fs::create_dir_all(net.join("wlan0/wireless")).unwrap();
        fs::create_dir_all(net.join("eth0")).unwrap();
        assert_eq!(wifi_interfaces_in(&net), ["wlan0"]);

        let bt = dir.0.join("bt");
        fs::create_dir_all(bt.join("hci1")).unwrap();
        fs::create_dir_all(bt.join("hci0:1")).unwrap();
        fs::create_dir_all(bt.join("hci")).unwrap();
        assert_eq!(bt_adapters_in(&bt), ["hci1"]);
    }

    #[test]
    fn wifi_comes_from_the_named_or_the_first_wlan() {
        let dir = TempDir::new();
        write(&dir.0.join("wlan1/address"), "aa:bb:cc:dd:ee:01");
        write(&dir.0.join("eth0/address"), "aa:bb:cc:dd:ee:02");
        assert_eq!(wifi_mac_in(&dir.0, ""), Some("AA:BB:CC:DD:EE:01".into()));
        assert_eq!(wifi_mac_in(&dir.0, "eth0"), Some("AA:BB:CC:DD:EE:02".into()));
        assert_eq!(wifi_mac_in(&dir.0, "wlan7"), None);
    }
}
