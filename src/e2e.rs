//! `teamlite --e2e <org> <chat> <message>`: drives the real view in a headless Rune runtime fed by
//! the live workers. Selects the organisation and chat by clicking, types the message into the
//! composer, presses Send, and waits until the server echoes it back through the delta sync.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use rune::{Env, Key, KeyEvent, Mods, Runtime, Tx};

use crate::app::App;
use crate::worker::Conn;

type Job = Box<dyn FnOnce(&mut App, &mut Tx) + Send>;
pub static SINK: Mutex<Vec<Job>> = Mutex::new(Vec::new());

fn pump(rt: &mut Runtime<App>) {
    let jobs: Vec<Job> = std::mem::take(&mut *SINK.lock().unwrap());
    for j in jobs {
        rt.update(j);
    }
    // What the window's `poll` does every frame.
    rt.update(|s, tx| {
        let c = s.clock + 1.0 / 60.0;
        s.tick(c, tx);
    });
    rt.frame(1.0 / 60.0);
}

fn wait(rt: &mut Runtime<App>, what: &str, secs: u64, mut done: impl FnMut(&App) -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        pump(rt);
        if done(&rt.state) {
            println!("  ✓ {what} ({:.1}s)", start.elapsed().as_secs_f32());
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    println!("  ✗ {what}: timed out after {secs}s");
    false
}

fn click(rt: &mut Runtime<App>, key: &str) -> bool {
    // Let layout springs settle so the element is where a person would click it.
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
    match rt.find(key) {
        Some((x, y)) => {
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            pump(rt);
            true
        }
        None => {
            println!("  ✗ no element {key}");
            false
        }
    }
}

/// `--e2e-tenants`: clicks every organisation tile in turn and checks each one connects and
/// lists its chats; switching back and forth keeps each organisation's own state.
pub fn tenants() -> bool {
    let mut app = App::load();
    app.images = Some(rune::ImageCache::new(std::sync::Arc::new(|| {})));
    let mut rt = Runtime::new(app, crate::ui::view, (1180.0, 760.0), Env { system_dark: true, scale: 1.0 });
    rt.frame(0.0);
    rt.update(|s, _| s.start());
    if !wait(&mut rt, "organisations discovered", 60, |s| s.tenants.len() > 1) {
        return false;
    }
    let mut ok = true;
    let n = rt.state.tenants.len();
    for round in 0..2 {
        for i in 0..n {
            click(&mut rt, &format!("org-{i}"));
            let name = rt.state.tenants[i].tenant.name.clone();
            let done = wait(&mut rt, &format!("[{round}] {name}: active, connected, chats listed — or asking for its own sign-in"), 90, |s| {
                s.active == i && ((s.tenants[i].conn == Conn::Live && !s.tenants[i].convs.is_empty()) || s.tenants[i].needs_sign_in.is_some())
            });
            let t = &rt.state.tenants[i];
            let shown = rt.find("tenant-name").is_some();
            match &t.needs_sign_in {
                Some(r) => println!("      {name}: needs sign-in ({}) — banner shown: {}", r.chars().take(60).collect::<String>(), rt.find("tenant-signin-button").is_some()),
                None => println!("      {name}: {} chats, {} unread, me {}, header shown {shown}", t.convs.len(), t.unread(), if t.me.is_empty() { "?" } else { "ok" }),
            }
            ok &= done;
        }
    }
    ok
}

pub fn run(org: &str, chat: &str, message: &str) -> bool {
    let mut app = App::load();
    app.images = Some(rune::ImageCache::new(std::sync::Arc::new(|| {})));
    let mut rt = Runtime::new(app, crate::ui::view, (1180.0, 760.0), Env { system_dark: true, scale: 1.0 });
    rt.frame(0.0);
    rt.update(|s, _| s.start());
    let org_l = org.to_lowercase();
    let chat_l = chat.to_lowercase();
    println!("e2e: org~{org:?} chat~{chat:?}");
    if !wait(&mut rt, "organisations discovered", 60, |s| s.tenants.iter().any(|t| t.tenant.name.to_lowercase().contains(&org_l))) {
        return false;
    }
    let idx = rt.state.tenants.iter().position(|t| t.tenant.name.to_lowercase().contains(&org_l)).unwrap();
    println!("  organisations: {:?}", rt.state.tenants.iter().map(|t| t.tenant.name.as_str()).collect::<Vec<_>>());
    if !click(&mut rt, &format!("org-{idx}")) {
        return false;
    }
    if !wait(&mut rt, "organisation live with chats", 90, |s| s.active == idx && s.tenants[idx].conn == Conn::Live && !s.tenants[idx].convs.is_empty()) {
        return false;
    }
    let t = &rt.state.tenants[idx];
    let pinned: Vec<String> = t.convs.iter().filter(|c| c.pinned).map(|c| t.title(c)).collect();
    println!("  {} chats, pinned: {:?}, unread: {}", t.convs.len(), pinned, t.unread());
    let Some(conv) = t.convs.iter().find(|c| t.title(c).to_lowercase().contains(&chat_l)).map(|c| c.id.clone()) else {
        println!("  ✗ no chat titled like {chat:?}");
        return false;
    };
    // Filter the list with the search field, as a person would with a long list.
    click(&mut rt, "search");
    for ch in chat.chars() {
        rt.key(KeyEvent { key: Key::Text(ch.to_string()), mods: Mods::default() });
    }
    pump(&mut rt);
    if !click(&mut rt, &format!("chat-{conv}")) {
        return false;
    }
    let c2 = conv.clone();
    if !wait(&mut rt, "messages loaded", 30, |s| s.tenants[idx].threads.get(&c2).is_some_and(|th| th.loaded && !th.loading)) {
        return false;
    }
    let n = rt.state.tenants[idx].threads[&conv].list.len();
    println!("  {n} messages in view; header {:?}", rt.state.tenants[idx].conv(&conv).map(|c| rt.state.tenants[idx].title(c)));
    if message == "open:" {
        return true;
    }
    if let Some(needle) = message.strip_prefix("cleanup:") {
        // Delete our own messages containing `needle`, through the UI (select → Delete → Confirm).
        let ids: Vec<String> = rt.state.tenants[idx].threads[&conv].list.iter()
            .filter(|m| m.msg.from == rt.state.tenants[idx].me && !m.msg.deleted && m.rendered.plain().contains(needle))
            .map(|m| m.msg.id.clone()).collect();
        println!("  {} of my messages contain {needle:?}", ids.len());
        for id in ids {
            click(&mut rt, &format!("msg-{id}/msg-body"));
            click(&mut rt, "action-delete");
            click(&mut rt, "action-delete");
            let i = id.clone();
            if !server_has(&mut rt, idx, &conv, &format!("deleted {id}"), move |m| m.msg.id == i && m.msg.deleted) {
                return false;
            }
        }
        return true;
    }
    if message == "replies:" {
        // Read-only: open the reply chain of the newest post that has one.
        let roots: Vec<String> = rt.state.tenants[idx].threads[&conv].list.iter().rev().map(|m| m.msg.id.clone()).take(10).collect();
        for root in roots {
            if !click(&mut rt, &format!("replies-{root}")) {
                continue;
            }
            let key = crate::app::chain_key(&conv, &root);
            let k2 = key.clone();
            if !wait(&mut rt, "reply chain loaded", 30, |s| s.tenants[idx].threads.get(&k2).is_some_and(|t| t.loaded && !t.loading)) {
                return false;
            }
            let n = rt.state.tenants[idx].threads[&key].list.iter().filter(|m| m.msg.id != root).count();
            let shown = rt.find("replies").is_some();
            let target = rt.state.target();
            println!("  post {root}: {n} replies, thread shown {shown}, composer target is the chain: {}", target.as_deref() == Some(key.as_str()));
            if n > 0 {
                return shown && target.as_deref() == Some(key.as_str());
            }
            click(&mut rt, &format!("replies-{root}"));
        }
        println!("  no post with replies among the newest 10");
        return false;
    }
    if let Some(expect) = message.strip_prefix("listen:") {
        println!("  waiting for a message containing {expect:?} from another client…");
        let c5 = conv.clone();
        let e = expect.to_string();
        return wait(&mut rt, "message from another client shown in the open chat", 240, |s| {
            s.tenants[idx].threads.get(&c5).is_some_and(|th| th.list.iter().any(|m| m.rendered.plain().contains(&e)))
        });
    }
    click(&mut rt, "composer");
    for ch in message.chars() {
        rt.key(KeyEvent { key: Key::Text(ch.to_string()), mods: Mods::default() });
    }
    pump(&mut rt);
    if rt.state.composer.text != message {
        println!("  ✗ composer holds {:?}", rt.state.composer.text);
        return false;
    }
    let sent_at = Instant::now();
    click(&mut rt, "send");
    let c3 = conv.clone();
    let msg = message.to_string();
    let ok = wait(&mut rt, "server accepted the message", 30, |s| {
        s.tenants[idx].threads.get(&c3).is_some_and(|th| th.list.iter().any(|m| m.rendered.plain() == msg && !m.pending && !m.failed))
    });
    let failed = rt.state.tenants[idx].threads[&conv].list.iter().any(|m| m.failed);
    if !ok || failed {
        println!("  ✗ send failed");
        return false;
    }
    let c4 = conv.clone();
    let echoed = wait(&mut rt, "echo via delta sync (server id, in chat list)", 60, |s| {
        let t = &s.tenants[idx];
        let in_thread = t.threads.get(&c4).is_some_and(|th| th.list.iter().any(|m| m.rendered.plain() == msg && !m.msg.id.starts_with("pending-")));
        let in_list = t.conv(&c4).and_then(|c| c.last.as_ref()).is_some_and(|m| crate::api::html::preview(&m.html) == msg);
        in_thread && in_list
    });
    println!("  round trip {:.1}s", sent_at.elapsed().as_secs_f32());
    if !echoed {
        return false;
    }
    actions(&mut rt, idx, &conv, &msg)
}

/// Waits for the action's own follow-up, then re-reads the thread from the server and checks
/// `pred` against what the server returned (not the optimistic local copy).
fn server_has(rt: &mut Runtime<App>, idx: usize, conv: &str, what: &str, pred: impl Fn(&crate::app::Shown) -> bool) -> bool {
    // Let the worker finish the request (its own reload bumps the version).
    let v = version(&rt.state, idx, conv);
    let c = conv.to_string();
    wait(rt, &format!("{what}: request done"), 30, |s| version(s, idx, &c) > v);
    let v = version(&rt.state, idx, conv);
    let c2 = conv.to_string();
    rt.update(move |s, _| s.reload_thread(&c2));
    let c = conv.to_string();
    wait(rt, &format!("{what}: confirmed by a fresh server read"), 30, |s| {
        version(s, idx, &c) > v && s.tenants[idx].threads[&c].list.iter().any(&pred)
    })
}

fn version(s: &App, idx: usize, conv: &str) -> u64 {
    s.tenants[idx].threads.get(conv).map(|t| t.version).unwrap_or(0)
}

fn find_mine<'a>(s: &'a App, idx: usize, conv: &str, text: &str) -> Option<&'a crate::app::Shown> {
    s.tenants[idx].threads.get(conv)?.list.iter().find(|m| m.rendered.plain() == text && !m.msg.id.starts_with("pending-"))
}

/// React, edit and delete the message we sent, each confirmed by a fresh server page; then
/// check pictures and presence.
fn actions(rt: &mut Runtime<App>, idx: usize, conv: &str, text: &str) -> bool {
    let Some(id) = find_mine(&rt.state, idx, conv, text).map(|m| m.msg.id.clone()) else { return false };
    let me = rt.state.tenants[idx].me.clone();
    // Reaction
    click(rt, &format!("msg-{id}/msg-body"));
    let v0 = version(&rt.state, idx, conv);
    if !click(rt, "add-like") {
        return false;
    }
    let _ = v0;
    let (i, m2) = (id.clone(), me.clone());
    if !server_has(rt, idx, conv, "reaction 👍", move |m| m.msg.id == i && m.msg.reactions.iter().any(|(k, u)| k == "like" && u.contains(&m2))) {
        return false;
    }
    // Edit
    click(rt, &format!("msg-{id}/msg-body"));
    if !click(rt, "action-edit") {
        return false;
    }
    let edited = format!("{text} (edited)");
    let e2 = edited.clone();
    rt.update(move |s, _| s.composer.replace_text(&e2));
    pump(rt);
    click(rt, "send");
    let (i, e3) = (id.clone(), edited.clone());
    if !server_has(rt, idx, conv, "edit", move |m| m.msg.id == i && m.rendered.plain() == e3 && m.msg.edited) {
        return false;
    }
    // Delete (two clicks: confirm)
    click(rt, &format!("msg-{id}/msg-body"));
    click(rt, "action-delete");
    click(rt, "action-delete");
    let i = id.clone();
    if !server_has(rt, idx, conv, "delete", move |m| m.msg.id == i && m.msg.deleted) {
        return false;
    }
    // "Open in Teams" hands this chat to Teams on the web through its deep link.
    click(rt, "open-web");
    let link = rt.state.last_opened.clone().unwrap_or_default();
    let tid = rt.state.tenants[idx].tenant.id.clone();
    let expect = format!("https://teams.microsoft.com/l/chat/{}/conversations?tenantId={tid}", crate::api::teams::urlenc(conv));
    let reachable = crate::api::http().get(&link).send().map(|r| r.status().is_success()).unwrap_or(false);
    println!("  {} Open in Teams → {link} (expected form: {}, reachable: {reachable})", if link == expect && reachable { "✓" } else { "✗" }, link == expect);
    if link != expect || !reachable {
        return false;
    }
    // Send an image the way attach/paste do, confirm it on the server, see it render, delete it.
    if let Some(path) = std::env::var_os("TEAMLITE_E2E_IMAGE") {
        let bytes = std::fs::read(&path).unwrap_or_default();
        rt.update(move |s, tx| s.send_image(bytes, "teamlite-e2e.png".into(), tx));
        let me2 = me.clone();
        if !server_has(rt, idx, conv, "image message", move |m| m.msg.from == me2 && !m.pending && m.rendered.images.iter().any(|u| u.contains("/v1/objects/"))) {
            return false;
        }
        let shown = rt.state.tenants[idx].threads[conv].list.iter().rev().find(|m| m.msg.from == me && !m.rendered.images.is_empty()).map(|m| (m.msg.id.clone(), m.rendered.images[0].clone()));
        let Some((img_id, src)) = shown else { return false };
        let s2 = src.clone();
        if !wait(rt, "uploaded image downloads and renders", 60, |s| matches!(s.pictures.get(&s2), Some(crate::app::Picture::Ready(i)) if i.is_ready())) {
            return false;
        }
        click(rt, &format!("msg-{img_id}/msg-body"));
        click(rt, "action-delete");
        click(rt, "action-delete");
        let i = img_id.clone();
        if !server_has(rt, idx, conv, "image message deleted", move |m| m.msg.id == i && m.msg.deleted) {
            return false;
        }
    }
    // Send a file through OneDrive, as "+ Attach" does; confirm, delete, clean OneDrive.
    if std::env::var_os("TEAMLITE_E2E_FILE").is_some() {
        let name = format!("teamlite-e2e-{}.txt", crate::api::now_ms());
        let path = std::env::temp_dir().join(&name);
        let _ = std::fs::write(&path, "Teamlite file upload test.\n");
        rt.update(move |s, tx| s.send_path(path, tx));
        let (me3, n2) = (me.clone(), name.clone());
        if !server_has(rt, idx, conv, "file message", move |m| m.msg.from == me3 && !m.pending && m.msg.files.iter().any(|(f, u, _)| *f == n2 && u.contains("sharepoint.com"))) {
            return false;
        }
        let n3 = name.clone();
        let Some(fid) = rt.state.tenants[idx].threads[conv].list.iter().find(|m| m.msg.files.iter().any(|(f, _, _)| *f == n3)).map(|m| m.msg.id.clone()) else { return false };
        let shown = rt.find(&format!("msg-{fid}")).is_some();
        println!("  ✓ file chip shown in the chat: {shown}");
        click(rt, &format!("msg-{fid}/msg-body"));
        click(rt, "action-delete");
        click(rt, "action-delete");
        let i = fid.clone();
        if !server_has(rt, idx, conv, "file message deleted", move |m| m.msg.id == i && m.msg.deleted) {
            return false;
        }
        let tid = rt.state.tenants[idx].tenant.id.clone();
        if let Some(acc) = crate::store::load_account()
            && let Some(rtok) = acc.refresh_for(&tid)
            && let Ok(g) = crate::api::auth::refresh(&tid, rtok, "https://graph.microsoft.com/.default")
        {
            match crate::api::files::remove(&g.access_token, &name) {
                Ok(()) => println!("  ✓ test file moved to the OneDrive recycle bin"),
                Err(e) => println!("  ✗ OneDrive cleanup: {e}"),
            }
        }
    }
    // Pictures in this chat (if it has any) and presence for the organisation.
    let has_pictures = rt.state.tenants[idx].threads[conv].list.iter().any(|m| !m.rendered.images.is_empty());
    if has_pictures && !wait(rt, "an inline picture downloaded and decoded", 60, |s| s.pictures.values().any(|p| matches!(p, crate::app::Picture::Ready(img) if img.is_ready()))) {
        return false;
    }
    wait(rt, "presence received", 90, |s| !s.tenants[idx].presence.is_empty())
}
