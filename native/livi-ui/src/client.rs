//! Framed-JSON client for livi-core's control socket.
//!
//! The wire format is livi-core-proto's: a little-endian u32 byte count
//! followed by that many bytes of JSON. The client says `hello`, keeps a local
//! copy of the state from `welcome` and `patch` (asking for a resync when a
//! patch is missed) and hands every new snapshot to the UI thread.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use livi_core_proto::PROTOCOL;
use livi_core_proto::frame::{Decoder, encode};
use livi_core_proto::input::Input;
use livi_core_proto::message::{Action, FromCore, ToCore};
use livi_core_proto::patch;
use serde_json::Value;

macro_rules! livi_log {
    ($($arg:tt)*) => {
        println!("[livi-ui] {}", format_args!($($arg)*))
    };
}
pub(crate) use livi_log;

const RETRY: Duration = Duration::from_millis(500);
const READ_POLL: Duration = Duration::from_millis(250);

/// Core sets LIVI_CORE_SOCKET for the UI it starts; the fallbacks match
/// livi-core's own Paths::socket().
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("LIVI_CORE_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("livi").join("core.sock");
    }
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    std::env::temp_dir().join(format!("livi-{uid}")).join("core.sock")
}

pub enum CoreEvent {
    Welcome { version: String, state: Value },
    Patch { state: Value },
    Refused(String),
    Connected(bool),
    Spectrum(Vec<f32>),
}

struct Shared {
    stream: Mutex<Option<UnixStream>>,
    next_id: AtomicU64,
    last_path: Mutex<Option<String>>,
    link_speed: AtomicBool,
    spectrum: AtomicBool,
}

#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
}

impl Client {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                stream: Mutex::new(None),
                next_id: AtomicU64::new(1),
                last_path: Mutex::new(None),
                link_speed: AtomicBool::new(false),
                spectrum: AtomicBool::new(false),
            }),
        }
    }

    /// Silently dropped while core is away, a late input or action does harm.
    pub fn send(&self, msg: ToCore) {
        let Ok(frame) = encode(&msg) else {
            livi_log!("cannot encode {msg:?}");
            return;
        };
        let mut guard = self.shared.stream.lock().unwrap();
        let failed = match guard.as_mut() {
            Some(stream) => match stream.write_all(&frame) {
                Ok(()) => false,
                Err(e) => {
                    livi_log!("write failed: {e}");
                    true
                }
            },
            None => false,
        };
        if failed {
            *guard = None;
        }
    }

    pub fn act(&self, action: Action) {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed) as u32;
        self.send(ToCore::Action { id, action });
    }

    pub fn input(&self, input: Input) {
        self.send(ToCore::Input { input });
    }

    pub fn path(&self, path: &str) {
        *self.shared.last_path.lock().unwrap() = Some(path.to_string());
        self.send(ToCore::Path { path: path.to_string() });
    }

    pub fn link_speed(&self, on: bool) {
        self.shared.link_speed.store(on, Ordering::Relaxed);
        self.send(ToCore::LinkSpeed { on });
    }

    pub fn spectrum(&self, on: bool) {
        self.shared.spectrum.store(on, Ordering::Relaxed);
        self.send(ToCore::Spectrum { on });
    }

    pub fn resync(&self) {
        self.send(ToCore::Resync);
    }
}

pub fn spawn(client: Client, events: Sender<CoreEvent>) {
    let _ = thread::Builder::new().name("core-link".into()).spawn(move || run(client, events));
}

fn run(client: Client, events: Sender<CoreEvent>) {
    let shared = client.shared.clone();
    let path = socket_path();
    livi_log!("connecting to {}", path.display());
    let mut state: Option<Value> = None;
    let mut rev = 0u64;

    loop {
        match UnixStream::connect(&path) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(READ_POLL));
                let mut reader = match stream.try_clone() {
                    Ok(reader) => reader,
                    Err(e) => {
                        livi_log!("cannot duplicate the socket: {e}");
                        thread::sleep(RETRY);
                        continue;
                    }
                };
                *shared.stream.lock().unwrap() = Some(stream);
                client.send(ToCore::Hello { protocol: PROTOCOL, client: "livi-ui".into() });
                let _ = events.send(CoreEvent::Connected(true));
                livi_log!("connected");

                let mut decoder = Decoder::new();
                let mut buf = [0u8; 64 * 1024];
                'read: loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            decoder.push(&buf[..n]);
                            while let Some(msg) = decoder.next_message::<FromCore>() {
                                let msg = match msg {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        livi_log!("broken frame: {e}");
                                        break 'read;
                                    }
                                };
                                match msg {
                                    FromCore::Welcome { version, rev: r, state: s, .. } => {
                                        let value =
                                            serde_json::to_value(&*s).unwrap_or(Value::Null);
                                        state = Some(value.clone());
                                        rev = r;
                                        livi_log!("welcome from core {version} (rev {r})");
                                        let _ = events
                                            .send(CoreEvent::Welcome { version, state: value });
                                        // Re-assert what core can have missed.
                                        if let Some(path) = shared.last_path.lock().unwrap().clone()
                                        {
                                            client.send(ToCore::Path { path });
                                        }
                                        if shared.link_speed.load(Ordering::Relaxed) {
                                            client.send(ToCore::LinkSpeed { on: true });
                                        }
                                        if shared.spectrum.load(Ordering::Relaxed) {
                                            client.send(ToCore::Spectrum { on: true });
                                        }
                                    }
                                    FromCore::Patch { rev: r, ops } => match state.as_mut() {
                                        Some(value) if r == rev + 1 => {
                                            let mut ok = true;
                                            for op in &ops {
                                                if patch::apply(value, op).is_err() {
                                                    ok = false;
                                                    break;
                                                }
                                            }
                                            if ok {
                                                rev = r;
                                                let _ = events.send(CoreEvent::Patch {
                                                    state: value.clone(),
                                                });
                                            } else {
                                                livi_log!("patch did not apply, resyncing");
                                                client.resync();
                                            }
                                        }
                                        _ => {
                                            livi_log!("missed a patch, resyncing");
                                            client.resync();
                                        }
                                    },
                                    FromCore::Reply { id, error } => {
                                        if let Some(error) = error {
                                            livi_log!("action {id}: {error}");
                                        }
                                    }
                                    FromCore::Refused { reason } => {
                                        livi_log!("core refused us: {reason}");
                                        let _ = events.send(CoreEvent::Refused(reason));
                                        return;
                                    }
                                    FromCore::Spectrum { bands } => {
                                        let _ = events.send(CoreEvent::Spectrum(bands));
                                    }
                                }
                            }
                        }
                        Err(e)
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut => {}
                        Err(_) => break,
                    }
                }
                *shared.stream.lock().unwrap() = None;
                let _ = events.send(CoreEvent::Connected(false));
                livi_log!("disconnected");
            }
            Err(_) => {}
        }
        thread::sleep(RETRY);
    }
}
