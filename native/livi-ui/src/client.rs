//! Newline-delimited JSON client for the core's UI Unix socket.
//!
//! Requests carry an `id` and are answered with `{"id":N,"ok":...}`.
//! Fire-and-forget messages have no `id`. Core events are
//! `{"event":"name","args":[...]}`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

macro_rules! livi_log {
    ($($arg:tt)*) => {
        println!("[livi-ui] {}", format_args!($($arg)*))
    };
}
pub(crate) use livi_log;

const RETRY_INTERVAL: Duration = Duration::from_millis(100);
const INITIAL_WAIT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub type EventFn = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub type StatusFn = Arc<dyn Fn(bool) + Send + Sync>;

pub fn socket_path() -> String {
    if let Ok(path) = std::env::var("LIVI_UI_SOCK") {
        if !path.is_empty() {
            return path;
        }
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return format!("{dir}/livi-ui.sock");
        }
    }
    "/tmp/livi-ui.sock".to_string()
}

pub struct Client {
    writer: Mutex<Option<UnixStream>>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    connected: AtomicBool,
}

impl Client {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            writer: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            connected: AtomicBool::new(false),
        })
    }

    /// Send a message without expecting a reply.
    pub fn fire(&self, method: &str, params: Value) {
        let line = json!({ "method": method, "params": params }).to_string();
        self.write_line(&line);
    }

    /// Send a request and wait for its response.
    pub fn request_blocking(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let line = json!({ "id": id, "method": method, "params": params }).to_string();
        self.write_line(&line);
        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("timeout waiting for {method}"))
            }
        }
    }

    fn write_line(&self, line: &str) {
        let mut guard = self.writer.lock().unwrap();
        if let Some(stream) = guard.as_mut() {
            if stream.write_all(line.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
                return;
            }
            let _ = stream.flush();
        }
    }

    fn fail_pending(&self, message: &str) {
        let pending: Vec<_> = self.pending.lock().unwrap().drain().map(|(_, tx)| tx).collect();
        for tx in pending {
            let _ = tx.send(Err(message.to_string()));
        }
    }
}

/// Connect (with retries), then read lines until the peer drops, forever.
pub fn spawn_listener(client: Arc<Client>, on_event: EventFn, on_status: StatusFn) {
    let builder = thread::Builder::new().name("core-socket".to_string());
    builder
        .spawn(move || {
            let path = socket_path();
            livi_log!("waiting for core on {path}");
            let started = Instant::now();
            let mut last_failure_log: Option<Instant> = None;

            loop {
                match UnixStream::connect(&path) {
                    Ok(stream) => {
                        let reader = match stream.try_clone() {
                            Ok(reader) => reader,
                            Err(e) => {
                                livi_log!("socket clone failed: {e}");
                                thread::sleep(RETRY_INTERVAL);
                                continue;
                            }
                        };

                        *client.writer.lock().unwrap() = Some(stream);
                        client.connected.store(true, Ordering::SeqCst);
                        livi_log!("connected to core");
                        on_status(true);

                        let buffered = BufReader::new(reader);
                        for line in buffered.lines() {
                            let line = match line {
                                Ok(line) => line,
                                Err(_) => break,
                            };
                            if line.trim().is_empty() {
                                continue;
                            }
                            let value: Value = match serde_json::from_str(&line) {
                                Ok(value) => value,
                                Err(e) => {
                                    livi_log!("dropping malformed core line: {e}");
                                    continue;
                                }
                            };
                            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                                let result =
                                    if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                                    } else {
                                        Err(value
                                            .get("error")
                                            .and_then(Value::as_str)
                                            .unwrap_or("request failed")
                                            .to_string())
                                    };
                                if let Some(tx) = client.pending.lock().unwrap().remove(&id) {
                                    let _ = tx.send(result);
                                }
                            } else if let Some(event) = value.get("event").and_then(Value::as_str) {
                                let args = value.get("args").cloned().unwrap_or_else(|| json!([]));
                                on_event(event, args);
                            }
                        }

                        *client.writer.lock().unwrap() = None;
                        client.connected.store(false, Ordering::SeqCst);
                        client.fail_pending("core disconnected");
                        livi_log!("core disconnected; reconnecting");
                        on_status(false);
                    }
                    Err(e) => {
                        let now = Instant::now();
                        let first = last_failure_log.is_none();
                        let due = last_failure_log
                            .map(|last| now.duration_since(last) >= Duration::from_secs(10))
                            .unwrap_or(true);
                        if first || due {
                            if started.elapsed() < INITIAL_WAIT {
                                livi_log!("still waiting for core ({e})");
                            } else {
                                livi_log!("core still unreachable ({e})");
                            }
                            last_failure_log = Some(now);
                        }
                    }
                }
                thread::sleep(RETRY_INTERVAL);
            }
        })
        .expect("failed to spawn socket thread");
}

/// Run `request_blocking` on a worker and deliver the result on the UI thread.
pub fn request_async<F>(client: Arc<Client>, method: &'static str, params: Value, f: F)
where
    F: FnOnce(Result<Value, String>) + Send + 'static,
{
    let _ = thread::Builder::new().name(format!("req-{method}")).spawn(move || {
        let result = client.request_blocking(method, params);
        let _ = slint::invoke_from_event_loop(move || f(result));
    });
}
