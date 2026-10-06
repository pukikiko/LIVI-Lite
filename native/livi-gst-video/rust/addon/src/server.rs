use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;

use livi_host_proto::{Framer, parse_plane_body, ui_planes};
use livi_video_player::Player;

use crate::{control, screens};

const CHUNK: usize = 65536;

fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path)
}

fn accept(name: &str, listener: UnixListener, serve: fn(UnixStream)) -> std::io::Result<()> {
    let name = name.to_string();
    std::thread::Builder::new().name(name.clone()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let _ = std::thread::Builder::new().name(name.clone()).spawn(move || serve(conn));
        }
    })?;
    Ok(())
}

pub fn serve(control: &Path, planes: &Path) -> std::io::Result<()> {
    accept("video-control", bind(control)?, serve_control)?;
    accept("video-planes", bind(planes)?, serve_plane)
}

fn serve_control(conn: UnixStream) {
    let Ok(mut answers) = conn.try_clone() else { return };
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else { return };
        let Some(answer) = control::parse(&line).and_then(screens::apply) else { continue };
        if answers.write_all(answer.as_bytes()).is_err() {
            return;
        }
    }
}

fn serve_plane(mut conn: UnixStream) {
    let mut framer = Framer::new();
    let mut chunk = vec![0u8; CHUNK];
    let mut drawn: Option<(u32, Arc<Player>)> = None;
    loop {
        let read = match conn.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        framer.push(&chunk[..read]);
        while let Some(m) = framer.next_message() {
            match m.op {
                ui_planes::CREATE => {
                    if let Some((id, player)) = drawn.take() {
                        screens::remove(id, &player);
                    }
                    drawn = parse_plane_body(&m.rest)
                        .and_then(|(codec, data)| screens::create(m.id, &codec, data))
                        .map(|player| (m.id, player));
                }
                ui_planes::FRAME => {
                    if let Some((_, player)) = &drawn {
                        player.push(&m.rest);
                    }
                }
                ui_planes::FLUSH => {
                    if let Some((_, player)) = &drawn {
                        player.flush();
                    }
                }
                _ => {}
            }
        }
    }
    if let Some((id, player)) = drawn {
        screens::remove(id, &player);
    }
}
