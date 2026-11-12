//! Hands calls and meetings to `teamlite-call`, the embedded-Chromium Teams window (see
//! `src/bin/teamlite-call.rs`). When it isn't installed, links open in the browser as before.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = "teamlite-call";

/// Where the call window lives: `$TEAMLITE_CALL_BIN`, next to `teamlite`, or in
/// `~/.local/lib/teamlite/` (the CEF runtime is installed alongside it).
pub fn binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TEAMLITE_CALL_BIN").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from));
    let mut candidates = Vec::new();
    if let Some(d) = &exe_dir {
        candidates.push(d.join(BIN));
        candidates.push(d.join("../lib/teamlite").join(BIN));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/lib/teamlite").join(BIN));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn available() -> bool {
    binary().is_some()
}

/// Opens `url` (a Teams web link) in the call window. Returns false if it couldn't be started.
pub fn open(url: &str) -> bool {
    let Some(bin) = binary() else { return false };
    let child = Command::new(bin)
        .arg(format!("--url={url}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(mut c) => {
            // Reap it when it exits so it doesn't linger as a zombie.
            std::thread::spawn(move || {
                let _ = c.wait();
            });
            true
        }
        Err(_) => false,
    }
}
