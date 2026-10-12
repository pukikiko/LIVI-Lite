//! The host's missing system packages, prompted like upstream's package check.
//!
//! The manifest is `scripts/install/packages.txt`, shared with the installers.
//! Every required entry is probed on this host; the missing ones are published
//! in the state and the UI asks what to do about them. Installing runs the
//! host's package manager as root through pkexec, like upstream does.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use livi_core_proto::message::PackagesChoice;
use livi_core_proto::state::{PackageItem, Packages, PackagesPhase};
use serde_json::json;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::server::Core;

/// Ships in the binary; `LIVI_PACKAGES_MANIFEST` names a file that wins over it.
const MANIFEST: &str = include_str!("../../../../../scripts/install/packages.txt");

/// A package installation on a slow mirror can take a while.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Core,
    Lite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    section: Section,
    name: String,
    probe: String,
    purpose: String,
    fedora: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Dnf,
}

/// What the manifest on this host says, one line per entry.
fn manifest() -> Vec<Entry> {
    let text = std::env::var("LIVI_PACKAGES_MANIFEST")
        .ok()
        .filter(|path| !path.is_empty())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_else(|| MANIFEST.to_string());
    parse_manifest(&text)
}

/// `<section>|<package>|<probe>|<purpose>|<fedora-package>`, `#` comments.
fn parse_manifest(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split('|');
            let section = match parts.next()? {
                "core" => Section::Core,
                "lite" => Section::Lite,
                _ => return None,
            };
            let name = parts.next()?.trim();
            let probe = parts.next()?.trim();
            if name.is_empty() || probe.is_empty() {
                return None;
            }
            Some(Entry {
                section,
                name: name.to_string(),
                probe: probe.to_string(),
                purpose: parts.next().unwrap_or("").trim().to_string(),
                fedora: parts.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

/// `core` everywhere, `lite` only where no desktop session backs the host.
fn required(entries: &[Entry], lite_host: bool) -> Vec<Entry> {
    entries.iter().filter(|e| e.section == Section::Core || lite_host).cloned().collect()
}

/// A display server in the environment counts as a session: labwc and friends
/// do not export XDG_CURRENT_DESKTOP, but any session hands us its display.
fn is_lite_host() -> bool {
    let set = |key: &str| std::env::var(key).is_ok_and(|v| !v.is_empty());
    lite_host_of(
        set("WAYLAND_DISPLAY") || set("DISPLAY"),
        set("XDG_CURRENT_DESKTOP"),
        Path::new("/usr/bin/gnome-session").exists(),
    )
}

fn lite_host_of(display: bool, desktop: bool, gnome_session: bool) -> bool {
    !display && !desktop && !gnome_session
}

/// One manifest probe. Unknown probe kinds count as present, so a typo never nags.
async fn probe_satisfied(probe: &str) -> bool {
    let Some((kind, arg)) = probe.split_once(':') else {
        return true;
    };
    if arg.is_empty() {
        return true;
    }
    match kind {
        "cmd" => {
            // Daemons live in sbin, which is often off a user's PATH.
            let path = format!("{}:/usr/sbin:/sbin", std::env::var("PATH").unwrap_or_default());
            Command::new("which")
                .arg(arg)
                .env("PATH", path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .is_ok_and(|status| status.success())
        }
        "gst" => Command::new("gst-inspect-1.0")
            .args(["--exists", arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success()),
        "file" => path_present(arg),
        _ => true,
    }
}

/// Existence test for a path holding at most one `*`, which stands in for a
/// single directory name. Libraries sit under a multiarch directory that
/// differs per host; Fedora keeps them flat in /usr/lib64.
fn path_present(pattern: &str) -> bool {
    let Some(star) = pattern.find('*') else {
        return Path::new(pattern).exists();
    };
    let Some(slash) = pattern[..star].rfind('/') else {
        return false;
    };
    let base = &pattern[..slash];
    let rest = pattern[star..].split_once('/').map_or("", |(_, rest)| rest);
    if Path::new(&format!("{base}64/{rest}")).exists() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| entry.path().join(rest).exists())
}

async fn missing_of(entries: &[Entry]) -> Vec<Entry> {
    let mut missing = Vec::new();
    for entry in entries {
        if !probe_satisfied(&entry.probe).await {
            missing.push(entry.clone());
        }
    }
    missing
}

async fn has_command(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Host package manager, apt before dnf.
async fn package_manager() -> Option<PackageManager> {
    if has_command("apt-get").await {
        return Some(PackageManager::Apt);
    }
    if has_command("dnf").await {
        return Some(PackageManager::Dnf);
    }
    None
}

/// Package names for the manager, deduped, empties dropped.
fn install_names(missing: &[Entry], pm: PackageManager) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in missing {
        let name = match pm {
            PackageManager::Apt => &entry.name,
            PackageManager::Dnf => &entry.fedora,
        };
        if !name.is_empty() && !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

fn manual_command(names: &[String], pm: PackageManager) -> String {
    let tool = match pm {
        PackageManager::Apt => "apt",
        PackageManager::Dnf => "dnf",
    };
    format!("sudo {tool} install {}", names.join(" "))
}

/// Runs the installation through pkexec, so the polkit agent asks for a password.
async fn install(names: &[String], pm: PackageManager) -> Result<(), String> {
    let line = match pm {
        PackageManager::Apt => format!("apt-get update && apt-get install -y {}", names.join(" ")),
        PackageManager::Dnf => format!("dnf install -y {}", names.join(" ")),
    };
    let running = Command::new("pkexec")
        .args(["bash", "-c", &format!("set -e\n{line}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    match tokio::time::timeout(INSTALL_TIMEOUT, running).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => match status.code() {
            Some(code) => Err(format!("pkexec exited with code {code}")),
            None => Err("pkexec was killed".into()),
        },
        Ok(Err(e)) => Err(format!("pkexec: {e}")),
        Err(_) => Err("the installation timed out".into()),
    }
}

fn items(missing: &[Entry]) -> Vec<PackageItem> {
    missing
        .iter()
        .map(|e| PackageItem { name: e.name.clone(), purpose: e.purpose.clone() })
        .collect()
}

fn publish(core: &Core, packages: Packages) {
    core.hub.update(|state| state.packages = packages);
}

/// The prompt's starting state, waiting on a decision.
fn prompt(missing: &[Entry], can_install: bool, manual: String) -> Packages {
    Packages {
        phase: PackagesPhase::Prompt,
        missing: items(missing),
        can_install,
        manual,
        error: None,
    }
}

/// Probes the host once and answers the prompt's choices.
pub async fn run(core: Arc<Core>, choices: mpsc::UnboundedReceiver<PackagesChoice>) {
    if !cfg!(target_os = "linux") {
        return;
    }
    run_required(core, choices, required(&manifest(), is_lite_host())).await;
}

async fn run_required(
    core: Arc<Core>,
    mut choices: mpsc::UnboundedReceiver<PackagesChoice>,
    required: Vec<Entry>,
) {
    if required.is_empty() {
        return;
    }
    let dismissed = core.hub.config().dismissed_packages;
    let mut missing = missing_of(&required).await;
    missing.retain(|e| !dismissed.contains(&e.name));
    if missing.is_empty() {
        return;
    }

    let pm = if has_command("pkexec").await { package_manager().await } else { None };
    let can_install = pm.is_some();
    let names = pm.map(|pm| install_names(&missing, pm)).unwrap_or_default();
    let manual = pm.map(|pm| manual_command(&names, pm)).unwrap_or_default();
    publish(&core, prompt(&missing, can_install, manual.clone()));

    loop {
        let Some(choice) = choices.recv().await else { return };
        match choice {
            PackagesChoice::Later => {
                publish(&core, Packages::default());
                return;
            }
            PackagesChoice::Seen => {
                publish(&core, Packages::default());
                return;
            }
            PackagesChoice::Never => {
                let mut next = core.hub.config().dismissed_packages;
                for entry in &missing {
                    if !next.contains(&entry.name) {
                        next.push(entry.name.clone());
                    }
                }
                if let Err(e) = core.set_config(&json!({ "dismissedPackages": next })).await {
                    eprintln!("[packages] the dismissal was not saved: {e}");
                }
                publish(&core, Packages::default());
                return;
            }
            PackagesChoice::Install => {
                let Some(pm) = pm else { continue };
                publish(
                    &core,
                    Packages {
                        phase: PackagesPhase::Installing,
                        missing: items(&missing),
                        can_install,
                        manual: manual.clone(),
                        error: None,
                    },
                );
                match install(&names, pm).await {
                    Ok(()) => {
                        let still = missing_of(&missing).await;
                        if still.is_empty() {
                            publish(
                                &core,
                                Packages {
                                    phase: PackagesPhase::Done,
                                    missing: Vec::new(),
                                    can_install,
                                    manual: manual.clone(),
                                    error: None,
                                },
                            );
                        } else {
                            publish(
                                &core,
                                Packages {
                                    phase: PackagesPhase::StillMissing,
                                    missing: items(&still),
                                    can_install,
                                    manual: manual.clone(),
                                    error: None,
                                },
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("[packages] the installation failed: {e}");
                        publish(
                            &core,
                            Packages {
                                phase: PackagesPhase::Failed,
                                missing: items(&missing),
                                can_install,
                                manual: manual.clone(),
                                error: Some(e),
                            },
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(section: Section, name: &str, probe: &str, fedora: &str) -> Entry {
        Entry {
            section,
            name: name.into(),
            probe: probe.into(),
            purpose: format!("{name} purpose"),
            fedora: fedora.into(),
        }
    }

    #[test]
    fn the_manifest_parses_its_lines_and_skips_the_rest() {
        let text = "\
# comment
core|bluez|cmd:bluetoothctl|Bluetooth pairing|bluez

lite|curl|cmd:curl|Downloading|curl
not-a-section|x|cmd:x||
core|no-probe||purpose|fedora
core|hostapd|cmd:hostapd|Wi-Fi access point|hostapd
";
        let entries = parse_manifest(text);
        assert_eq!(
            entries,
            vec![
                Entry {
                    section: Section::Core,
                    name: "bluez".into(),
                    probe: "cmd:bluetoothctl".into(),
                    purpose: "Bluetooth pairing".into(),
                    fedora: "bluez".into(),
                },
                Entry {
                    section: Section::Lite,
                    name: "curl".into(),
                    probe: "cmd:curl".into(),
                    purpose: "Downloading".into(),
                    fedora: "curl".into(),
                },
                Entry {
                    section: Section::Core,
                    name: "hostapd".into(),
                    probe: "cmd:hostapd".into(),
                    purpose: "Wi-Fi access point".into(),
                    fedora: "hostapd".into(),
                },
            ]
        );
    }

    #[test]
    fn the_embedded_manifest_is_readable() {
        let entries = parse_manifest(MANIFEST);
        assert!(entries.iter().any(|e| e.name == "bluez" && e.section == Section::Core));
        assert!(entries.iter().any(|e| e.name == "curl" && e.section == Section::Lite));
    }

    #[test]
    fn lite_entries_only_join_a_host_without_a_session() {
        let entries = vec![
            entry(Section::Core, "bluez", "cmd:bluez", "bluez"),
            entry(Section::Lite, "cage", "cmd:cage", "cage"),
        ];
        let desktop = required(&entries, lite_host_of(true, true, true));
        assert_eq!(desktop.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["bluez"]);
        let lite = required(&entries, lite_host_of(false, false, false));
        assert_eq!(lite.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["bluez", "cage"]);
        assert!(lite_host_of(false, false, false));
        assert!(!lite_host_of(false, true, false));
        assert!(!lite_host_of(true, false, false));
        assert!(!lite_host_of(false, false, true));
    }

    #[test]
    fn a_star_in_a_path_stands_for_one_directory() {
        let dir = crate::config_file::tests::TempDir::new();
        let libs = dir.0.join("lib");
        std::fs::create_dir_all(libs.join("x86_64-linux-gnu")).unwrap();
        std::fs::write(libs.join("x86_64-linux-gnu/libva.so.2"), "").unwrap();

        assert!(path_present(&format!("{}/libva.so.2", libs.join("*").display())));
        assert!(!path_present(&format!("{}/libnope.so.2", libs.join("*").display())));
        let direct = libs.join("x86_64-linux-gnu/libva.so.2");
        assert!(path_present(&direct.display().to_string()));
        assert!(!path_present(&format!("{}/missing/lib.so", dir.0.display())));
    }

    #[tokio::test]
    async fn probes_answer_for_commands_files_and_typos() {
        assert!(probe_satisfied("cmd:sh").await);
        assert!(!probe_satisfied("cmd:definitely-not-a-livi-command").await);
        let dir = crate::config_file::tests::TempDir::new();
        let file = dir.0.join("present");
        std::fs::write(&file, "").unwrap();
        assert!(probe_satisfied(&format!("file:{}", file.display())).await);
        assert!(!probe_satisfied(&format!("file:{}", dir.0.join("gone").display())).await);
        assert!(probe_satisfied("mystery:whatever").await);
        assert!(probe_satisfied("cmd:").await);
    }

    #[test]
    fn install_names_take_the_hosts_package_names_and_dedupe() {
        let missing = vec![
            entry(Section::Core, "libva2", "file:/x", "libva"),
            entry(Section::Core, "libva-drm2", "file:/y", "libva"),
            entry(Section::Core, "pipewire-pulse", "cmd:pipewire-pulse", "pipewire-pulseaudio"),
        ];
        assert_eq!(
            install_names(&missing, PackageManager::Apt),
            ["libva2", "libva-drm2", "pipewire-pulse"]
        );
        assert_eq!(install_names(&missing, PackageManager::Dnf), ["libva", "pipewire-pulseaudio"]);
        assert_eq!(
            manual_command(&install_names(&missing, PackageManager::Apt), PackageManager::Apt),
            "sudo apt install libva2 libva-drm2 pipewire-pulse"
        );
        assert_eq!(
            manual_command(&install_names(&missing, PackageManager::Dnf), PackageManager::Dnf),
            "sudo dnf install libva pipewire-pulseaudio"
        );
    }

    #[test]
    fn the_prompt_lists_what_is_missing() {
        let missing = vec![entry(Section::Core, "bluez", "cmd:bluetoothctl", "bluez")];
        let state = Packages {
            phase: PackagesPhase::Prompt,
            missing: items(&missing),
            can_install: true,
            manual: "sudo apt install bluez".into(),
            error: None,
        };
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            json!({
                "phase": "prompt",
                "missing": [{ "name": "bluez", "purpose": "bluez purpose" }],
                "canInstall": true,
                "manual": "sudo apt install bluez"
            })
        );
    }

    #[test]
    fn an_idle_prompt_carries_no_manual_or_error() {
        assert_eq!(
            serde_json::to_value(Packages::default()).unwrap(),
            json!({ "phase": "idle", "missing": [], "canInstall": false })
        );
        assert_eq!(
            serde_json::to_value(PackagesPhase::StillMissing).unwrap(),
            json!("stillMissing")
        );
    }

    use std::sync::Arc;

    use livi_core_proto::state::{Front, PerScreen};
    use tokio::sync::{Notify, watch};

    use crate::config_file::{ConfigFile, defaults};
    use crate::hub::Hub;

    fn core_in(dir: &crate::config_file::tests::TempDir) -> Arc<Core> {
        let livi = PerScreen { main: Front::Livi, dash: Front::Livi, aux: Front::Livi };
        let hub = Hub::new(livi_core_proto::state::State {
            front: livi,
            sessions: Default::default(),
            now_playing: Default::default(),
            telemetry: Default::default(),
            navigation: Default::default(),
            system: Default::default(),
            devices: Default::default(),
            update: Default::default(),
            packages: Default::default(),
            config: defaults(),
        });
        let file = ConfigFile::new(dir.0.join("config.json"), dir.0.join("backup.json"));
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let (senders, _asks, _dongle, _update, _packages) = crate::projection::ui_channels(livi);
        Arc::new(Core::new(hub, file, uid, Arc::new(Notify::new()), senders))
    }

    async fn wait_for_prompt(
        watch: &mut watch::Receiver<livi_core_proto::state::State>,
    ) -> Packages {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                {
                    let state = watch.borrow_and_update();
                    if state.packages.phase != PackagesPhase::Idle {
                        return state.packages.clone();
                    }
                }
                watch.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_prompt_laters_and_remembers_a_never() {
        let dir = crate::config_file::tests::TempDir::new();
        let core = core_in(&dir);
        let missing = vec![entry(
            Section::Core,
            "no-such-package",
            "cmd:definitely-not-a-livi-command",
            "no-such-package",
        )];
        let mut watch = core.hub.watch();

        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_required(core.clone(), rx, missing.clone()));
        let prompt = wait_for_prompt(&mut watch).await;
        assert_eq!(prompt.phase, PackagesPhase::Prompt);
        assert_eq!(
            prompt.missing,
            vec![PackageItem {
                name: "no-such-package".into(),
                purpose: "no-such-package purpose".into()
            }]
        );
        tx.send(PackagesChoice::Later).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert_eq!(watch.borrow().packages.phase, PackagesPhase::Idle);

        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_required(core.clone(), rx, missing.clone()));
        let _ = wait_for_prompt(&mut watch).await;
        tx.send(PackagesChoice::Never).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert!(core.hub.config().dismissed_packages.contains(&"no-such-package".into()));
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.0.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(saved["dismissedPackages"], json!(["no-such-package"]));

        let (_tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_required(core.clone(), rx, missing));
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert_eq!(watch.borrow().packages.phase, PackagesPhase::Idle);
    }
}
