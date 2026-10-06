//! Everything that touches a view runs on the main thread.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use livi_host_proto::plane_place;
use livi_video_player::Player;

use crate::control::{Line, Region};
use crate::main_thread::on_main;

unsafe extern "C" {
    fn livi_set_backdrop(parent: usize, r: f64, g: f64, b: f64);
}

#[derive(Clone, Copy)]
struct Placement {
    region: Region,
    shown: bool,
}

impl Default for Placement {
    fn default() -> Self {
        Self { region: Region::default(), shown: true }
    }
}

struct Drawn {
    tag: String,
    screen: &'static str,
    player: Arc<Player>,
}

#[derive(Default)]
struct Screens {
    /// The content view of each screen's window.
    windows: HashMap<String, usize>,
    /// By tag, kept for the planes that come later.
    placements: HashMap<String, Placement>,
    /// By plane id.
    drawn: HashMap<u32, Drawn>,
    gamma: Option<[f64; 5]>,
    backdrop: Option<[u8; 3]>,
}

static SCREENS: LazyLock<Mutex<Screens>> = LazyLock::new(Mutex::default);

fn screens() -> MutexGuard<'static, Screens> {
    SCREENS.lock().unwrap_or_else(|e| e.into_inner())
}

fn place(player: &Player, r: Region) {
    player.set_content_region(r.crop_l, r.crop_t, r.vis_w, r.vis_h, r.tier_w, r.tier_h);
}

fn paint(window: usize, [r, g, b]: [u8; 3]) {
    let c = |v: u8| f64::from(v) / 255.0;
    // SAFETY: the handle is a content view the UI registered and still holds.
    unsafe { livi_set_backdrop(window, c(r), c(g), c(b)) };
}

pub fn apply(line: Line) -> Option<String> {
    match line {
        Line::Claim(tag) => return Some(format!("bound {tag}\n")),
        Line::Place { tag, region } => on_main(|| {
            let mut s = screens();
            s.placements.entry(tag.clone()).or_default().region = region;
            s.drawn.values().filter(|d| d.tag == tag).for_each(|d| place(&d.player, region));
        }),
        Line::Show { tag, shown } => on_main(|| {
            let mut s = screens();
            s.placements.entry(tag.clone()).or_default().shown = shown;
            s.drawn.values().filter(|d| d.tag == tag).for_each(|d| d.player.set_visible(shown));
        }),
        Line::Backdrop(rgb) => on_main(|| {
            let mut s = screens();
            s.backdrop = Some(rgb);
            s.windows.values().for_each(|&w| paint(w, rgb));
        }),
        Line::Gamma(v @ [gamma, contrast, r, g, b]) => {
            let mut s = screens();
            s.gamma = Some(v);
            s.drawn.values().for_each(|d| d.player.set_gamma(gamma, contrast, r, g, b));
        }
    }
    None
}

pub fn create(id: u32, codec: &str, codec_data: &[u8]) -> Option<Arc<Player>> {
    let Some((tag, screen)) = plane_place(id) else {
        eprintln!("[video] plane 0x{id:x} belongs to no screen");
        return None;
    };
    let (player, replaced) = on_main(|| {
        let mut s = screens();
        let Some(&window) = s.windows.get(screen) else {
            eprintln!("[video] no {screen} window to draw plane 0x{id:x} in");
            return (None, None);
        };
        let Some(player) = Player::new(codec, window, codec_data).map(Arc::new) else {
            return (None, None);
        };
        let at = s.placements.get(&tag).copied().unwrap_or_default();
        place(&player, at.region);
        player.set_visible(at.shown);
        if let Some([gamma, contrast, r, g, b]) = s.gamma {
            player.set_gamma(gamma, contrast, r, g, b);
        }
        player.start();
        let replaced = s.drawn.insert(id, Drawn { tag, screen, player: player.clone() });
        (Some(player), replaced)
    });
    if let Some(old) = replaced {
        on_main(|| old.player.stop());
    }
    player
}

pub fn remove(id: u32, player: &Arc<Player>) {
    on_main(|| {
        let mut s = screens();
        if s.drawn.get(&id).is_some_and(|d| Arc::ptr_eq(&d.player, player)) {
            s.drawn.remove(&id);
        }
        drop(s);
        player.stop();
    });
}

/// `window` is 0 once the screen's window closed.
pub fn set_window(screen: &str, window: usize) {
    on_main(|| {
        let mut s = screens();
        let before = if window == 0 {
            s.windows.remove(screen)
        } else {
            s.windows.insert(screen.to_string(), window)
        };
        let gone: Vec<Arc<Player>> = if before.is_some_and(|b| b != window) {
            let ids: Vec<u32> =
                s.drawn.iter().filter(|(_, d)| d.screen == screen).map(|(id, _)| *id).collect();
            ids.iter().filter_map(|id| s.drawn.remove(id)).map(|d| d.player).collect()
        } else {
            Vec::new()
        };
        let backdrop = s.backdrop;
        drop(s);
        gone.iter().for_each(|p| p.stop());
        if let (true, Some(rgb)) = (window != 0, backdrop) {
            paint(window, rgb);
        }
    });
}
