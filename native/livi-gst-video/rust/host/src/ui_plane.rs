use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use livi_host_proto::{encode_reply, plane_body, ui_planes};

use crate::Plane;

const QUEUE: usize = 1024;

pub struct UiPlane {
    id: u32,
    out: SyncSender<Vec<u8>>,
    lost: AtomicBool,
}

impl UiPlane {
    pub fn open(path: &str, id: u32, codec: &str, codec_data: &[u8]) -> Option<Self> {
        let mut sock = UnixStream::connect(path)
            .inspect_err(|e| eprintln!("[gst-host] no UI for plane 0x{id:x} at {path}: {e}"))
            .ok()?;
        let (out, queued) = sync_channel::<Vec<u8>>(QUEUE);
        out.try_send(encode_reply(ui_planes::CREATE, id, &plane_body(codec, codec_data))).ok()?;
        // Writes run on their own thread so they never block the main loop, where audio runs.
        std::thread::Builder::new()
            .name(format!("ui-plane-{id:x}"))
            .spawn(move || {
                for msg in queued {
                    if sock.write_all(&msg).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self { id, out, lost: AtomicBool::new(false) })
    }

    fn send(&self, op: u8, rest: &[u8]) {
        if let Err(TrySendError::Full(_)) = self.out.try_send(encode_reply(op, self.id, rest))
            && !self.lost.swap(true, Ordering::Relaxed)
        {
            eprintln!("[gst-host] the UI stopped taking plane 0x{:x}, frames are lost", self.id);
        }
    }
}

impl Plane for UiPlane {
    fn start(&self) {}

    fn push(&self, nal: &[u8]) {
        self.send(ui_planes::FRAME, nal);
    }

    fn flush(&self) {
        self.send(ui_planes::FLUSH, &[]);
    }

    /// The UI takes the calibration from core.
    fn set_gamma(&self, _gamma: f64, _contrast: f64, _r: f64, _g: f64, _b: f64) {}
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    use livi_host_proto::{Framer, Message};

    use super::*;

    fn socket_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ui-plane-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn read_all(mut conn: UnixStream) -> Vec<Message> {
        // macOS refuses the option once the plane already hung up.
        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
        let mut bytes = Vec::new();
        conn.read_to_end(&mut bytes).unwrap();
        let mut framer = Framer::new();
        framer.push(&bytes);
        std::iter::from_fn(|| framer.next_message()).collect()
    }

    #[test]
    fn the_ui_gets_the_create_then_the_frames_until_the_plane_goes() {
        let path = socket_path("frames");
        let listener = UnixListener::bind(&path).unwrap();
        let plane = UiPlane::open(path.to_str().unwrap(), 0x7a00_0001, "h264", &[1, 2]).unwrap();
        plane.push(&[5, 5]);
        plane.flush();
        drop(plane);

        let (conn, _) = listener.accept().unwrap();
        let got = read_all(conn);
        let _ = std::fs::remove_file(&path);
        let ops: Vec<(u8, u32)> = got.iter().map(|m| (m.op, m.id)).collect();
        assert_eq!(
            ops,
            [
                (ui_planes::CREATE, 0x7a00_0001),
                (ui_planes::FRAME, 0x7a00_0001),
                (ui_planes::FLUSH, 0x7a00_0001)
            ]
        );
        assert_eq!(got[0].rest, plane_body("h264", &[1, 2]));
        assert_eq!(got[1].rest, [5, 5]);
    }

    #[test]
    fn without_a_ui_there_is_no_plane() {
        let path = socket_path("nobody");
        assert!(UiPlane::open(path.to_str().unwrap(), 1, "h264", &[]).is_none());
    }
}
