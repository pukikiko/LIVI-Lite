//! Process launcher: spawn the Node core and, in launcher mode, the nested
//! LIVI compositor that will re-launch this binary with `--nested`.

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::client::{livi_log, socket_path};

pub struct CoreGuard(Child);

impl Drop for CoreGuard {
    fn drop(&mut self) {
        livi_log!("stopping core (pid {})", self.0.id());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The root the core resolves non-binary assets against (bundled GStreamer,
/// scripts): the nearest ancestor of the binary carrying `assets/`. In the
/// installed layout that is above `out/`, so the exe-dir parent is not enough.
fn app_root() -> Option<PathBuf> {
    let mut dir = exe_dir();
    for _ in 0..6 {
        if dir.join("assets").is_dir() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// Locate the bundled core script. `LIVI_CORE_JS` wins; otherwise look next to
/// the binary and in the installed/dev layouts.
fn core_script() -> Option<PathBuf> {
    if let Some(path) = env_nonempty("LIVI_CORE_JS") {
        return Some(PathBuf::from(path));
    }
    let dir = exe_dir();
    let candidates = [
        dir.join("livi-core.cjs"),
        dir.join("../core/livi-core.cjs"),
        dir.join("../lib/livi/core/livi-core.cjs"),
        // dev tree: native/livi-ui/target/{debug,release}/livi-ui -> repo/out/core
        dir.join("../../../../out/core/livi-core.cjs"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Spawn the Node core and keep a handle so it dies with the UI.
pub fn spawn_core() -> Option<CoreGuard> {
    let node = env_nonempty("LIVI_NODE_BIN").unwrap_or_else(|| "node".to_string());
    let script = match core_script() {
        Some(path) => path,
        None => {
            livi_log!("could not locate livi-core.cjs (set LIVI_CORE_JS)");
            return None;
        }
    };
    let sock = socket_path();

    let mut cmd = Command::new(&node);
    cmd.arg(&script);
    cmd.env("LIVI_UI_SOCK", &sock);
    cmd.env("LIVI_PARENT_PID", std::process::id().to_string());
    // Payloads the core execs (driver/, core/) live next to the binary: pin
    // `resourcesPath` to `out/` so the helper is found whatever APP_PATH says.
    if env_nonempty("LIVI_RESOURCES").is_none() {
        if let Some(resources) = exe_dir().parent() {
            cmd.env("LIVI_RESOURCES", resources);
        }
    }
    if env_nonempty("LIVI_APP_PATH").is_none() {
        // The bundled GStreamer lives in `assets/`, which may sit above `out/`.
        // Without it the core falls back to the system install and loses the
        // software H.264/H.265 decoders (`create player ... FAILED`).
        if let Some(root) = app_root().or_else(|| exe_dir().parent().map(Path::to_path_buf)) {
            cmd.env("LIVI_APP_PATH", root);
        }
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    match cmd.spawn() {
        Ok(child) => {
            livi_log!("spawned core: {} {} (LIVI_UI_SOCK={})", node, script.display(), sock);
            Some(CoreGuard(child))
        }
        Err(e) => {
            livi_log!("could not spawn core ({node} {}): {e}", script.display());
            None
        }
    }
}

fn compositor_binary() -> Option<PathBuf> {
    if let Some(path) = env_nonempty("LIVI_COMPOSITOR_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
        livi_log!("LIVI_COMPOSITOR_BIN={} does not exist", path.display());
    }
    let next_to_exe = exe_dir().join("livi-compositor");
    if next_to_exe.exists() {
        return Some(next_to_exe);
    }
    // installed layout: out/ui/livi-ui + out/compositor/livi-compositor
    let sibling = exe_dir().join("../compositor/livi-compositor");
    if sibling.exists() {
        return Some(sibling);
    }
    // dev tree: native/livi-ui/target/{debug,release}/livi-ui -> repo/out/compositor
    let dev = exe_dir().join("../../../../out/compositor/livi-compositor");
    if dev.exists() {
        return Some(dev);
    }
    let installed = PathBuf::from("/usr/lib/livi/livi-compositor");
    if installed.exists() {
        return Some(installed);
    }
    None
}

fn compositor_config() -> Option<serde_json::Value> {
    let path = if let Some(data_dir) = env_nonempty("LIVI_USER_DATA") {
        PathBuf::from(data_dir).join("config.json")
    } else {
        let home = env_nonempty("HOME")?;
        PathBuf::from(home).join(".config/LIVI/config.json")
    };
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Port of compositorBootstrap.ts: run the nested compositor that hosts this
/// UI and the video plane, then let the caller exit 0.
pub fn bootstrap_compositor() -> bool {
    let Some(compositor) = compositor_binary() else {
        return false;
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("livi-ui"));
    let inner = format!("'{}' --nested", exe.display());

    // The core drives video claim/placement/visibility and the backdrop/gamma over
    // this socket; without it the waylandsink plane waits forever for its claim.
    let runtime_dir = env_nonempty("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".to_string());
    let ctrl_socket = Path::new(&runtime_dir).join("livi-compositor.ctrl");

    let mut env_extra: Vec<(String, String)> = vec![
        ("LIVI_OUTPUT_APP_ID".into(), "dev.f-io.livi".into()),
        ("LIVI_SCREENS".into(), "main".into()),
        ("LIVI_COMPOSITOR".into(), "1".into()),
        ("LIVI_COMPOSITOR_CTRL".into(), ctrl_socket.to_string_lossy().into_owned()),
        ("RUST_BACKTRACE".into(), "1".into()),
    ];

    if let Some(config) = compositor_config() {
        let kiosk =
            config.pointer("/kiosk/main").and_then(serde_json::Value::as_bool).unwrap_or(false)
                || env_nonempty("LIVI_KIOSK").is_some();
        let width = config
            .get("mainScreenWidth")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
            .round() as i64;
        let height = config
            .get("mainScreenHeight")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
            .round() as i64;
        if width > 0 && height > 0 {
            let titlebar = if kiosk { 0 } else { 32 };
            env_extra.push(("LIVI_OUTPUT_SIZE".into(), format!("{width}x{}", height + titlebar)));
        }
    }

    let mut cmd = Command::new(&compositor);
    cmd.arg("-s");
    cmd.arg(&inner);
    for (key, value) in &env_extra {
        cmd.env(key, value);
    }
    cmd.env_remove("APPIMAGE");
    cmd.env_remove("APPDIR");
    cmd.env_remove("ARGV0");
    cmd.env_remove("OWD");
    cmd.stdin(Stdio::null());

    match open_log_file() {
        Some(file) => match file.try_clone() {
            Ok(err_file) => {
                cmd.stdout(Stdio::from(file));
                cmd.stderr(Stdio::from(err_file));
            }
            Err(_) => {
                cmd.stdout(Stdio::null());
                cmd.stderr(Stdio::null());
            }
        },
        None => {
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }
    }

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    match cmd.spawn() {
        Ok(child) => {
            livi_log!(
                "started compositor {} (pid {}) -> {}",
                compositor.display(),
                child.id(),
                inner
            );
            std::mem::forget(child); // detached: it outlives this launcher
            true
        }
        Err(e) => {
            livi_log!("failed to start compositor {}: {e}", compositor.display());
            false
        }
    }
}

fn open_log_file() -> Option<std::fs::File> {
    let home = env_nonempty("HOME")?;
    let dir = Path::new(&home).join(".config/LIVI/log");
    std::fs::create_dir_all(&dir).ok()?;
    OpenOptions::new().create(true).append(true).open(dir.join("compositor.log")).ok()
}
