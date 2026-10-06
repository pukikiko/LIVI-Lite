//! The kernel of the i.MX6ULL board goes in through the vendor U-Boot (see imx6ul-uboot): the web
//! flash stages it with this dongle's fuses, and U-Boot converts it on its next start.

use std::fs;
use std::process::Command;

const OCOTP: &str = "/sys/bus/nvmem/devices/imx-ocotp0/nvmem";
const SDIO: &str = "/sys/bus/sdio/devices";

/// The Wi-Fi/Bluetooth modules the board comes with and has a build for: SDIO device id, label,
/// firmware target.
const MODULES: [(&str, &str, &str); 3] = [
    ("0x9159", "i.MX6ULL + IW416", "imx6ul_iw416"),
    ("0xc822", "i.MX6ULL + RTL8822CS", "imx6ul_rtl8822cs"),
    ("0xb822", "i.MX6ULL + RTL8822BS", "imx6ul_rtl8822bs"),
];

/// An unknown module gets the IW416 build, which runs all but Wi-Fi and Bluetooth there.
pub fn module() -> (&'static str, &'static str) {
    let ids: Vec<String> = fs::read_dir(SDIO)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("device")).ok())
        .collect();
    module_of(&ids)
}

fn module_of(ids: &[String]) -> (&'static str, &'static str) {
    MODULES
        .iter()
        .find(|(id, ..)| ids.iter().any(|found| found.trim() == *id))
        .map_or(("i.MX6ULL + unknown Wi-Fi module", "imx6ul_iw416"), |&(_, model, target)| {
            (model, target)
        })
}

pub fn stage_kernel(zimage: &[u8], kernel: &[u8]) -> Result<Vec<u8>, String> {
    let fuses = fs::read(OCOTP)
        .ok()
        .and_then(|n| imx6ul_uboot::Fuses::from_ocotp(&n))
        .ok_or_else(|| format!("could not read the fuses from {OCOTP}"))?;
    let uboot = mtd("uboot")?;
    let uboot = fs::read(&uboot).map_err(|e| format!("read {uboot}: {e}"))?;
    imx6ul_uboot::stage(zimage, kernel, &uboot, fuses)
}

/// Makes U-Boot convert the staging blocks on its next start. It runs before the kernel write, so
/// a failed erase keeps the old kernel and any start after the write brings up the new one.
pub fn erase_env() -> Result<(), String> {
    let env = mtd("env")?;
    let erased = Command::new("/usr/sbin/flash_eraseall")
        .args(["-q", &env])
        .status()
        .is_ok_and(|s| s.success())
        && fs::read(&env).is_ok_and(|b| !b.is_empty() && b.iter().all(|&x| x == 0xff));
    if erased { Ok(()) } else { Err(format!("could not erase the environment block {env}")) }
}

fn mtd(name: &str) -> Result<String, String> {
    fs::read_dir("/sys/class/mtd")
        .into_iter()
        .flatten()
        .flatten()
        .find_map(|e| {
            let dev = e.file_name().into_string().ok()?;
            let n = dev.strip_prefix("mtd")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (fs::read_to_string(e.path().join("name")).ok()?.trim() == name)
                .then(|| format!("/dev/{dev}"))
        })
        .ok_or_else(|| format!("no {name} partition"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| format!("{id}\n")).collect()
    }

    #[test]
    fn the_module_picks_the_target() {
        assert_eq!(module_of(&ids(&["0x9159"])).1, "imx6ul_iw416");
        assert_eq!(module_of(&ids(&["0xc822"])), ("i.MX6ULL + RTL8822CS", "imx6ul_rtl8822cs"));
        assert_eq!(module_of(&ids(&["0xb822"])), ("i.MX6ULL + RTL8822BS", "imx6ul_rtl8822bs"));
    }

    #[test]
    fn an_unknown_module_stays_on_the_iw416_build() {
        assert_eq!(
            module_of(&ids(&["0x4354"])),
            ("i.MX6ULL + unknown Wi-Fi module", "imx6ul_iw416")
        );
        assert_eq!(module_of(&[]).1, "imx6ul_iw416");
    }
}
