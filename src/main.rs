//! Teamlite: a fast native Microsoft Teams chat client for Linux, drawn with Rune.

mod api;
mod app;
mod demo;
mod e2e;
mod login;
mod probe;
mod store;
mod ui;
mod worker;

use rune::{AppSpec, Key, KeyEvent, Tx};

use app::App;

fn poll(s: &mut App, clock: f64, tx: &mut Tx) -> bool {
    if !STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) && rune::tooling::headless_run().is_none() {
        s.start();
    }
    s.tick(clock, tx)
}

static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.is_command('k') {
        tx.focus("search", false);
        return true;
    }
    if ev.mods.cmd
        && let Key::Command(c) = &ev.key
        && let Some(d) = c.to_digit(10)
        && d >= 1
    {
        s.switch_tenant(d as usize - 1, tx);
        return true;
    }
    if ev.mods.alt && matches!(ev.key, Key::Up | Key::Down) {
        let Some(t) = s.active() else { return false };
        let ids: Vec<String> = t.convs.iter().filter(|c| !c.hidden).map(|c| c.id.clone()).collect();
        let cur = t.selected.as_ref().and_then(|id| ids.iter().position(|x| x == id));
        let next = match (ev.key.clone(), cur) {
            (Key::Up, Some(i)) => i.saturating_sub(1),
            (Key::Down, Some(i)) => (i + 1).min(ids.len().saturating_sub(1)),
            _ => 0,
        };
        if let Some(id) = ids.get(next).cloned() {
            s.open(&id, tx);
            tx.scroll_into_view(format!("chat-{id}"));
        }
        return true;
    }
    if matches!(ev.key, Key::Escape) {
        if s.show_menu {
            s.show_menu = false;
            return true;
        }
        if s.selected_msg.is_some() {
            s.selected_msg = None;
            s.confirm_delete = None;
            return true;
        }
        if s.editing.is_some() || s.active().is_some_and(|t| t.reply_root.is_some()) {
            s.cancel_compose_mode();
            return true;
        }
    }
    false
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "webview-login")]
    if args.first().map(String::as_str) == Some("--login-webview") {
        login::webview_main(args.get(1).map(String::as_str).unwrap_or(""));
    }
    if args.first().map(String::as_str) == Some("--probe") {
        if let Err(e) = probe::run(args.get(1).map(String::as_str)) {
            eprintln!("probe failed: {e:#}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--e2e-tenants") {
        let ok = e2e::tenants();
        println!("{}", if ok { "E2E PASS" } else { "E2E FAIL" });
        std::process::exit(if ok { 0 } else { 1 });
    }
    if args.first().map(String::as_str) == Some("--e2e") {
        let a = |i: usize| args.get(i).cloned().unwrap_or_default();
        let ok = e2e::run(&a(1), &a(2), &a(3));
        println!("{}", if ok { "E2E PASS" } else { "E2E FAIL" });
        std::process::exit(if ok { 0 } else { 1 });
    }
    if args.first().map(String::as_str) == Some("--probe-presence") {
        if let Err(e) = probe::presence(args.get(1).map(String::as_str).unwrap_or("")) {
            eprintln!("probe failed: {e:#}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--probe-delete") {
        let a = |i: usize| args.get(i).cloned().unwrap_or_default();
        if let Err(e) = probe::delete(&a(1), &a(2), &a(3)) {
            eprintln!("probe failed: {e:#}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--probe-scopes") {
        if let Err(e) = probe::scopes(args.get(1).map(String::as_str).unwrap_or("")) {
            eprintln!("probe failed: {e:#}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--probe-ams") {
        if let Err(e) = probe::ams(args.get(1).map(String::as_str).unwrap_or("")) {
            eprintln!("probe failed: {e:#}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--probe-msgs") {
        if let Err(e) = probe::messages(args.get(1).map(String::as_str).unwrap_or(""), args.get(2).map(String::as_str).unwrap_or("")) {
            eprintln!("probe failed: {e:#}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("--sign-out") {
        store::forget_account();
        println!("Signed out; local tokens and cache removed.");
        return;
    }

    rune::tooling::set_app_id("teamlite");
    let spec = AppSpec::<App> {
        poll: Some(poll),
        setup: Some(|s, waker| s.images = Some(rune::ImageCache::new(waker))),
        on_key: Some(on_key),
        min_size: Some((760.0, 480.0)),
        scenes: demo::SCENES,
        ..AppSpec::new("Teamlite", (1180.0, 760.0), App::default, ui::view)
    };
    let headless = args.iter().any(|a| a.starts_with("--snap") || a.starts_with("--script") || a.starts_with("--tree") || a == "--list-scenes");
    let app = rune::App::new(spec).init(move || if headless { App::default() } else { App::load() });
    let _ = app::HANDLE.set(app.handle());
    app.run();
}
