pub fn run(args: Vec<String>) -> i32 {
    livid_main(&args)
}

// The vendor driver has no sysfs attributes for the MAC, so livi-httpd reads it from
// /tmp/livi/bt-mac.

use std::fs;
use std::io::Write;

const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_HCI: libc::c_int = 1;
// HCIDEVUP = _IOW('H', 201, int)          = 0x400448c9
// HCIGETDEVINFO = _IOR('H', 211, int)     = 0x800448d3 (with a struct hci_dev_info buffer)
const HCIDEVUP: libc::c_ulong = 0x400448c9;
const HCIGETDEVINFO: libc::c_ulong = 0x800448d3;

// struct hci_dev_info from <net/bluetooth/hci.h>, only the first ~28 bytes are read.
#[repr(C)]
struct HciDevInfo {
    dev_id: u16,
    name: [u8; 8],
    bdaddr: [u8; 6],
    flags: u32,
    dev_type: u8,
    _pad: [u8; 7],
    features: [u8; 8],
    _rest: [u8; 128],
}

fn livid_main(args: &[String]) -> i32 {
    let dev: libc::c_int = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let sock = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_RAW, BTPROTO_HCI) };
    if sock < 0 {
        eprintln!("livi-bt-up: socket(AF_BLUETOOTH): {}", std::io::Error::last_os_error());
        return 1;
    }

    let r = unsafe { libc::ioctl(sock, HCIDEVUP as _, dev) };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EALREADY) {
            eprintln!("livi-bt-up: HCIDEVUP hci{dev}: {e}");
            unsafe {
                libc::close(sock);
            }
            return 1;
        }
    }

    let mut info: HciDevInfo = unsafe { std::mem::zeroed() };
    info.dev_id = dev as u16;
    let r = unsafe { libc::ioctl(sock, HCIGETDEVINFO as _, &mut info as *mut _) };
    unsafe {
        libc::close(sock);
    }
    if r < 0 {
        eprintln!("livi-bt-up: HCIGETDEVINFO hci{dev}: {}", std::io::Error::last_os_error());
        return 2;
    }

    // bdaddr is stored little-endian, the MAC string is big-endian.
    let mac = format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        info.bdaddr[5],
        info.bdaddr[4],
        info.bdaddr[3],
        info.bdaddr[2],
        info.bdaddr[1],
        info.bdaddr[0]
    );
    // The kernel packs bus (low nibble) and dev_type (bits 4-5) into one byte. mgmt only
    // manages primary controllers and refuses an "amp" one.
    let bus = info.dev_type & 0x0f;
    let kind = match (info.dev_type >> 4) & 0x03 {
        0 => "primary",
        1 => "amp",
        _ => "?",
    };
    println!("livi-bt-up: hci{dev} up, MAC {mac}, bus {bus}, type {kind}");

    let _ = fs::create_dir_all("/tmp/livi");
    if let Ok(mut f) = fs::File::create("/tmp/livi/bt-mac") {
        let _ = writeln!(f, "{mac}");
    }
    0
}
