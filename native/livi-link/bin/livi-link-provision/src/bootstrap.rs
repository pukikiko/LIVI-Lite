// The wire writes files with no execute bit, so the bootstrap rides on /etc/profile, which rcS
// sources at boot.

use crate::wire;
use livi_session_io::usb::{UsbStream, open_pipe};
use tokio::io::AsyncWriteExt;

const TYPE_OPEN: u32 = 0x01;
const TYPE_SEND_FILE: u32 = 0x99;

pub const CARRIER: &str = "/etc/profile";
pub const BOOT_HOOK: &str = "/script/livi-boot.sh";

const BOOTSTRAP: &str = include_str!("../scripts/bootstrap.sh");
const CARRIER_BODY: &str = include_str!("../scripts/profile");
const HOOK_LINE: &str = "\ncase \"$0\" in */rcS) sh /script/livi-boot.sh ;; esac\n";

/// The geometry the dongle wants before it accepts messages.
fn open_payload() -> Vec<u8> {
    [800u32, 480, 30, 5, 49152, 2, 2].iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// `[nameLen][name\0][contentLen][content]`
fn send_file_payload(path: &str, content: &[u8]) -> Vec<u8> {
    let mut name = path.as_bytes().to_vec();
    name.push(0);
    let mut out = Vec::with_capacity(8 + name.len() + content.len());
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(&name);
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(content);
    out
}

pub fn write_file(path: &str, content: &[u8]) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    runtime.block_on(send(path, content))
}

/// Takes effect on the next boot.
pub fn boot_hook() -> Result<(), String> {
    write_file(BOOT_HOOK, BOOTSTRAP.as_bytes())?;
    write_file(CARRIER, format!("{CARRIER_BODY}{HOOK_LINE}").as_bytes())
}

pub fn carrier_body() -> &'static str {
    CARRIER_BODY
}

pub fn stock_dongle_once() -> bool {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(_) => return false,
    };
    runtime.block_on(async {
        match nusb::list_devices().await {
            Ok(devices) => devices
                .into_iter()
                .any(|d| livi_link_host::is_dongle(&d) && !livi_link_host::is_livi_link(&d)),
            Err(_) => false,
        }
    })
}

/// The vendor access point's usual addresses.
pub const VENDOR_AP_HOSTS: [&str; 2] = ["192.168.50.2", "192.168.43.1"];

/// A dongle on the bus that already calls itself LIVI Link: the bootstrap on the vendor firmware,
/// or LIVI Link itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOnUsb {
    Ncm,
    NoNcm,
}

pub fn livi_link_on_usb() -> Option<LinkOnUsb> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    runtime.block_on(async {
        let devices = nusb::list_devices().await.ok()?;
        let link = devices
            .into_iter()
            .find(|d| livi_link_host::is_dongle(d) && livi_link_host::is_livi_link(d))?;
        let interfaces = link.interfaces().map(|i| (i.class(), i.subclass()));
        Some(if carries_ncm(interfaces) { LinkOnUsb::Ncm } else { LinkOnUsb::NoNcm })
    })
}

/// NCM is a CDC communication interface of subclass 0x0d.
fn carries_ncm(interfaces: impl IntoIterator<Item = (u8, u8)>) -> bool {
    const CDC: u8 = 0x02;
    const NCM: u8 = 0x0d;
    interfaces.into_iter().any(|(class, subclass)| class == CDC && subclass == NCM)
}

pub fn scan() -> Vec<(u16, u16, String)> {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    runtime.block_on(async {
        match nusb::list_devices().await {
            Ok(devices) => devices
                .into_iter()
                .map(|d| {
                    (d.vendor_id(), d.product_id(), d.product_string().unwrap_or("").to_string())
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    })
}

async fn send(path: &str, content: &[u8]) -> Result<(), String> {
    // A dongle that has just been plugged in, or one that is re-enumerating, is briefly not in
    // the list at all.
    let mut info = None;
    for _ in 0..20 {
        if let Ok(devices) = nusb::list_devices().await {
            info = devices
                .into_iter()
                .find(|d| livi_link_host::is_dongle(d) && !livi_link_host::is_livi_link(d));
        }
        if info.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let info = info.ok_or("no dongle on the bus, unplug it and plug it back in")?;

    let dev = info.open().await.map_err(|e| format!("open dongle: {e}"))?;
    // Without those endpoints the dongle is not in the mode it boots into, so a fresh start is
    // what brings it back.
    let pipe =
        open_pipe(&dev).await.map_err(|e| format!("{e}, unplug the dongle and plug it back in"))?;
    let mut stream = UsbStream::new(pipe);

    stream
        .write_all(&wire::message(TYPE_OPEN, &open_payload()))
        .await
        .map_err(|e| format!("open handshake: {e}"))?;
    stream
        .write_all(&wire::message(TYPE_SEND_FILE, &send_file_payload(path, content)))
        .await
        .map_err(|e| format!("send {path}: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))?;
    // The write is pumped by a task, so give it a moment before the pipe closes with the stream.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cdc_ncm_interface_counts_as_ncm() {
        assert!(carries_ncm([(0x02, 0x0d), (0x0a, 0x00)]));
        assert!(!carries_ncm([(0x02, 0x06), (0x0a, 0x00)])); // ECM
        assert!(!carries_ncm([(0xff, 0x0d)]));
        assert!(!carries_ncm([]));
    }

    #[test]
    fn a_send_file_payload_carries_the_path_and_the_bytes() {
        let p = send_file_payload("/script/custom_init.sh", b"hi");
        assert_eq!(u32::from_le_bytes(p[..4].try_into().unwrap()), 23); // path plus its NUL
        assert_eq!(&p[4..26], b"/script/custom_init.sh");
        assert_eq!(p[26], 0);
        assert_eq!(u32::from_le_bytes(p[27..31].try_into().unwrap()), 2);
        assert_eq!(&p[31..], b"hi");
    }

    #[test]
    fn the_open_payload_is_the_geometry_the_dongle_expects() {
        let p = open_payload();
        assert_eq!(p.len(), 28);
        assert_eq!(u32::from_le_bytes(p[..4].try_into().unwrap()), 800);
        assert_eq!(u32::from_le_bytes(p[24..].try_into().unwrap()), 2); // work mode CarPlay
    }

    /// The host names the interface on first sight and keeps that name, so the bootstrap announces
    /// the product our own system announces.
    #[test]
    fn the_bootstrap_announces_the_product_our_system_announces() {
        let ours = include_str!("../../../../../scripts/livi-link/common/initramfs/net-up");
        assert!(BOOTSTRAP.contains("printf 'LIVI Link' > \"$A/iProduct\""));
        assert!(ours.contains("echo \"LIVI Link\" > $G/strings/0x409/product"));
    }
}
