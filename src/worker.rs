//! One worker per tenant: a poll thread (chat list, names, then the long-poll loop) and a command
//! thread (open a chat, load history, send, mark read). They share one token cache. The UI never
//! blocks on the network; everything comes back as an [`Update`].

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::api::auth::{self, Tokens};
use crate::api::teams::{self, ChatService, ConvKind, Conversation, Message, SkypeAuth, Tenant};

#[derive(Clone, Debug, PartialEq)]
pub enum Conn {
    Connecting,
    Live,
    Retrying(String),
}

#[derive(Debug)]
pub enum Update {
    Status { tenant: String, conn: Conn },
    Me { tenant: String, mri: String },
    Conversations { tenant: String, list: Vec<Conversation> },
    Conversation { tenant: String, conv: Conversation },
    Names { tenant: String, names: Vec<(String, String)>, emails: Vec<(String, String)> },
    Members { tenant: String, conv: String, members: Vec<String> },
    Messages { tenant: String, conv: String, msgs: Vec<Message>, backward: Option<String>, older: bool },
    Favorites { tenant: String, ids: Vec<String> },
    Image { url: String, bytes: Option<Vec<u8>> },
    Presence { tenant: String, list: Vec<(String, String)> },
    NeedsSignIn { tenant: String, reason: String },
    Sent { tenant: String, conv: String, client_id: String, error: Option<String> },
    Error { tenant: String, text: String },
    Refreshed { tenant: String, refresh_token: String },
    Tenants { list: Vec<Tenant> },
}

#[derive(Debug)]
pub enum Command {
    Open(String),
    Older(String, String),
    Send { conv: String, text: String, client_id: String, display_name: String },
    MarkRead(String, Message),
    Edit { conv: String, id: String, text: String, display_name: String },
    Delete { conv: String, id: String },
    React { conv: String, id: String, key: String, add: bool },
    Image(String),
    Typing(String),
    SendImage { conv: String, bytes: Vec<u8>, name: String, text: String, client_id: String, display_name: String },
    SendFile { conv: String, path: std::path::PathBuf, text: String, client_id: String, display_name: String, share_with: Vec<String> },
    Stop,
}

pub type Post = Arc<dyn Fn(Update) + Send + Sync>;

struct Session {
    tenant: String,
    is_home: bool,
    refresh_token: Mutex<String>,
    aad: Mutex<Option<Tokens>>,
    skype: Mutex<Option<SkypeAuth>>,
    presence: Mutex<Option<Tokens>>,
    graph: Mutex<Option<Tokens>>,
    me: Mutex<String>,
    post: Post,
    stopped: std::sync::atomic::AtomicBool,
}

impl Session {
    fn aad(&self) -> Result<Tokens> {
        if let Some(t) = self.aad.lock().unwrap().as_ref().filter(|t| t.fresh()) {
            return Ok(t.clone());
        }
        let rt = self.refresh_token.lock().unwrap().clone();
        let t = auth::refresh(&self.tenant, &rt, auth::SKYPE_SCOPE)?;
        if t.refresh_token != rt {
            *self.refresh_token.lock().unwrap() = t.refresh_token.clone();
            (self.post)(Update::Refreshed { tenant: self.tenant.clone(), refresh_token: t.refresh_token.clone() });
        }
        if let Ok(who) = auth::identity(&t.access_token) {
            let mri = format!("8:orgid:{}", who.oid);
            let mut me = self.me.lock().unwrap();
            if *me != mri {
                *me = mri.clone();
                (self.post)(Update::Me { tenant: self.tenant.clone(), mri });
            }
        }
        *self.aad.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    fn skype(&self) -> Result<SkypeAuth> {
        if let Some(s) = self.skype.lock().unwrap().as_ref().filter(|s| s.fresh()) {
            return Ok(s.clone());
        }
        let aad = self.aad()?;
        let s = teams::authz(&aad.access_token)?;
        *self.skype.lock().unwrap() = Some(s.clone());
        Ok(s)
    }

    fn presence_token(&self) -> Result<Tokens> {
        if let Some(t) = self.presence.lock().unwrap().as_ref().filter(|t| t.fresh()) {
            return Ok(t.clone());
        }
        let rt = self.refresh_token.lock().unwrap().clone();
        let t = auth::refresh(&self.tenant, &rt, "https://presence.teams.microsoft.com/.default")?;
        *self.presence.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    fn graph_token(&self) -> Result<Tokens> {
        if let Some(t) = self.graph.lock().unwrap().as_ref().filter(|t| t.fresh()) {
            return Ok(t.clone());
        }
        let rt = self.refresh_token.lock().unwrap().clone();
        let t = auth::refresh(&self.tenant, &rt, "https://graph.microsoft.com/.default")?;
        *self.graph.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    fn invalidate(&self) {
        *self.skype.lock().unwrap() = None;
        *self.aad.lock().unwrap() = None;
    }

    fn chat(&self) -> Result<ChatService> {
        let s = self.skype()?;
        Ok(ChatService::new(s, self.me.lock().unwrap().clone()))
    }

    /// Runs `f` against the chat service, renewing tokens once on an auth failure.
    fn with_chat<T>(&self, f: impl Fn(&ChatService) -> Result<T>) -> Result<T> {
        let chat = self.chat()?;
        match f(&chat) {
            Err(e) if format!("{e}").contains("HTTP 401") => {
                self.invalidate();
                f(&self.chat()?)
            }
            r => r,
        }
    }

    fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub struct Worker {
    pub commands: Sender<Command>,
}

pub fn spawn(tenant: &Tenant, refresh_token: String, post: Post) -> Worker {
    let session = Arc::new(Session {
        tenant: tenant.id.clone(),
        is_home: tenant.is_home,
        refresh_token: Mutex::new(refresh_token),
        aad: Mutex::new(None),
        skype: Mutex::new(None),
        presence: Mutex::new(None),
        graph: Mutex::new(None),
        me: Mutex::new(String::new()),
        post,
        stopped: Default::default(),
    });
    let (tx, rx) = channel();
    let s = session.clone();
    thread::Builder::new().name(format!("cmd-{}", &tenant.id[..8.min(tenant.id.len())])).spawn(move || commands(s, rx)).ok();
    let s = session;
    thread::Builder::new().name(format!("poll-{}", &tenant.id[..8.min(tenant.id.len())])).spawn(move || poll_loop(s)).ok();
    Worker { commands: tx }
}

fn commands(s: Arc<Session>, rx: Receiver<Command>) {
    let t = s.tenant.clone();
    while let Ok(cmd) = rx.recv() {
        let post = &s.post;
        match cmd {
            Command::Stop => {
                s.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            Command::Open(conv) => match s.with_chat(|c| c.messages(&conv, None)).inspect_err(|e| eprintln!("teamlite: open: {e:#}")) {
                Ok(page) => post(Update::Messages { tenant: t.clone(), conv, msgs: page.items, backward: page.backward, older: false }),
                // A chat that has never been used (e.g. notes-to-self) has no thread yet: empty.
                Err(e) if e.to_string().contains("HTTP 404") => post(Update::Messages { tenant: t.clone(), conv, msgs: Vec::new(), backward: None, older: false }),
                Err(e) => post(Update::Error { tenant: t.clone(), text: format!("Couldn't load messages: {e}") }),
            },
            Command::Older(conv, link) => match s.with_chat(|c| c.messages(&conv, Some(&link))) {
                Ok(page) => post(Update::Messages { tenant: t.clone(), conv, msgs: page.items, backward: page.backward, older: true }),
                Err(e) => post(Update::Error { tenant: t.clone(), text: format!("Couldn't load history: {e}") }),
            },
            Command::Send { conv, text, client_id, display_name } => {
                let html = crate::api::html::to_html(&text);
                let r = s.with_chat(|c| c.send(&conv, &html, &display_name, &client_id));
                if let Err(e) = &r {
                    eprintln!("teamlite: send: {e:#}");
                }
                post(Update::Sent { tenant: t.clone(), conv, client_id, error: r.err().map(|e| e.to_string()) });
            }
            Command::MarkRead(conv, msg) => {
                let _ = s.with_chat(|c| c.mark_read(&conv, &msg));
            }
            Command::Edit { conv, id, text, display_name } => {
                let html = crate::api::html::to_html(&text);
                let r = s.with_chat(|c| c.edit(&conv, &id, &html, &display_name));
                after_change(&s, &conv, r, "edit");
            }
            Command::Delete { conv, id } => {
                let r = s.with_chat(|c| c.delete(&conv, &id));
                after_change(&s, &conv, r, "delete");
            }
            Command::React { conv, id, key, add } => {
                let r = s.with_chat(|c| c.react(&conv, &id, &key, add));
                after_change(&s, &conv, r, "reaction");
            }
            Command::SendImage { conv, bytes, name, text, client_id, display_name } => {
                let size = imagesize::blob_size(&bytes).map(|d| (d.width as u32, d.height as u32)).unwrap_or((400, 300));
                let html = if text.trim().is_empty() { String::new() } else { crate::api::html::to_html(&text) };
                let r = s.with_chat(|c| {
                    let id = c.upload_image(&conv, &bytes, &name)?;
                    c.send_image(&conv, &id, size, &html, &display_name, &client_id)
                });
                if let Err(e) = &r {
                    eprintln!("teamlite: send image: {e:#}");
                }
                post(Update::Sent { tenant: t.clone(), conv: conv.clone(), client_id, error: r.as_ref().err().map(|e| e.to_string()) });
                if r.is_ok()
                    && let Ok(page) = s.with_chat(|c| c.messages(&conv, None))
                {
                    post(Update::Messages { tenant: t.clone(), conv, msgs: page.items, backward: page.backward, older: false });
                }
            }
            Command::SendFile { conv, path, text, client_id, display_name, share_with } => {
                let r: Result<()> = (|| {
                    let bytes = std::fs::read(&path)?;
                    if bytes.len() > 250 * 1024 * 1024 {
                        anyhow::bail!("files over 250 MB aren't supported");
                    }
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
                    let g = s.graph_token()?;
                    let up = crate::api::files::upload(&g.access_token, &name, &bytes)?;
                    if let Err(e) = crate::api::files::share(&g.access_token, &up.item_id, &share_with) {
                        eprintln!("teamlite: share: {e:#}");
                    }
                    let files = crate::api::files::files_property(&up);
                    let html = if text.trim().is_empty() { String::new() } else { crate::api::html::to_html(&text) };
                    s.with_chat(|c| c.send_file(&conv, &files, &html, &display_name, &client_id))
                })();
                let r = r.map_err(|e| {
                    let m = format!("{e:#}");
                    if m.contains("upload: HTTP 401") || m.contains("upload: HTTP 403") {
                        anyhow::anyhow!("this organisation doesn't let Teamlite use your OneDrive; use Open in Teams to send files here")
                    } else {
                        e
                    }
                });
                if let Err(e) = &r {
                    eprintln!("teamlite: send file: {e:#}");
                }
                post(Update::Sent { tenant: t.clone(), conv: conv.clone(), client_id, error: r.as_ref().err().map(|e| e.to_string()) });
                if r.is_ok()
                    && let Ok(page) = s.with_chat(|c| c.messages(&conv, None))
                {
                    post(Update::Messages { tenant: t.clone(), conv, msgs: page.items, backward: page.backward, older: false });
                }
            }
            Command::Typing(conv) => {
                if let Err(e) = s.with_chat(|c| c.typing(&conv)) {
                    eprintln!("teamlite: typing: {e:#}");
                } else if std::env::var_os("TEAMLITE_DEBUG").is_some() {
                    eprintln!("teamlite: typing sent");
                }
            }
            Command::Image(url) => {
                // A just-uploaded image can take a few seconds before its view is served.
                let mut bytes = None;
                for attempt in 0..4 {
                    match s.with_chat(|c| c.image(&url)) {
                        Ok(b) => {
                            bytes = Some(b);
                            break;
                        }
                        Err(e) if attempt < 3 => {
                            let _ = e;
                            thread::sleep(Duration::from_secs(2 << attempt));
                        }
                        Err(e) => eprintln!("teamlite: image: {e:#}"),
                    }
                }
                post(Update::Image { url, bytes });
            }
        }
    }
}

/// After an edit, delete or reaction: report a failure, or reload the thread to show the result.
fn after_change(s: &Session, conv: &str, r: Result<()>, what: &str) {
    match r {
        Ok(()) => {
            if let Ok(page) = s.with_chat(|c| c.messages(conv, None)) {
                (s.post)(Update::Messages { tenant: s.tenant.clone(), conv: conv.to_string(), msgs: page.items, backward: page.backward, older: false });
            }
        }
        Err(e) => {
            eprintln!("teamlite: {what}: {e:#}");
            (s.post)(Update::Error { tenant: s.tenant.clone(), text: format!("Couldn't apply {what}: {}", short(&e.to_string())) });
        }
    }
}

fn refresh_presence(s: &Session, convs: &[Conversation]) {
    let mut peers: Vec<String> = convs.iter().filter(|c| c.kind == ConvKind::OneOnOne && !c.peer.is_empty()).take(100).map(|c| c.peer.clone()).collect();
    peers.dedup();
    if peers.is_empty() {
        return;
    }
    if let Ok(t) = s.presence_token()
        && let Ok(list) = teams::presence(&t.access_token, &peers)
    {
        (s.post)(Update::Presence { tenant: s.tenant.clone(), list });
    }
}

fn resolve_names(s: &Session, chat: &ChatService, convs: &[Conversation], known: &mut HashSet<String>) {
    let t = s.tenant.clone();
    let mut wanted: Vec<String> = Vec::new();
    for c in convs {
        if c.kind == ConvKind::OneOnOne && !c.peer.is_empty() && !known.contains(&c.peer) {
            wanted.push(c.peer.clone());
        }
    }
    // Group chats without a topic are named after their members, like Teams does.
    let mut unnamed: Vec<&Conversation> = convs.iter().filter(|c| c.kind == ConvKind::Group && c.topic.is_empty()).collect();
    unnamed.sort_by_key(|c| (!c.pinned, std::cmp::Reverse(c.last_time())));
    unnamed.truncate(80);
    for c in unnamed {
        if s.stopped() {
            return;
        }
        if let Ok(members) = chat.thread_members(&c.id) {
            wanted.extend(members.iter().filter(|m| !known.contains(*m)).cloned());
            (s.post)(Update::Members { tenant: t.clone(), conv: c.id.clone(), members });
        }
    }
    wanted.sort();
    wanted.dedup();
    if wanted.is_empty() {
        return;
    }
    let Ok(aad) = s.aad() else { return };
    if let Ok(profiles) = teams::short_profiles(&chat.auth.middle_tier, &aad.access_token, &wanted) {
        let emails: Vec<(String, String)> = profiles.iter().filter(|p| !p.email.is_empty()).map(|p| (p.mri.clone(), p.email.clone())).collect();
        let names: Vec<(String, String)> = profiles.into_iter().map(|p| (p.mri, p.display_name)).collect();
        known.extend(names.iter().map(|(m, _)| m.clone()));
        (s.post)(Update::Names { tenant: t, names, emails });
    }
}

/// Needs an interactive sign-in for this tenant (MFA, consent, password change…).
fn needs_interaction(msg: &str) -> bool {
    ["AADSTS50076", "AADSTS50079", "AADSTS50158", "AADSTS50173", "AADSTS53003", "AADSTS65001", "AADSTS700082", "AADSTS50078", "interaction_required", "invalid_grant"]
        .iter()
        .any(|c| msg.contains(c))
}

fn poll_loop(s: Arc<Session>) {
    let t = s.tenant.clone();
    let post = s.post.clone();
    let mut backoff = 2u64;
    let mut known: HashSet<String> = HashSet::new();
    let mut tenants_sent = false;
    while !s.stopped() {
        post(Update::Status { tenant: t.clone(), conn: Conn::Connecting });
        let result: Result<()> = (|| {
            let mut chat = s.chat()?;
            if s.is_home && !tenants_sent {
                let aad = s.aad()?;
                let me_tid = auth::identity(&aad.access_token).map(|i| i.tid).unwrap_or_default();
                if let Ok(list) = teams::tenants(&chat.auth.middle_tier, &aad.access_token, &me_tid) {
                    post(Update::Tenants { list });
                    tenants_sent = true;
                }
            }
            let (mut convs, mut sync) = s.with_chat(|c| c.conversations())?;
            // Teams always offers the notes-to-self chat, even before it's used.
            if !convs.iter().any(|c| c.id == "48:notes") {
                convs.push(Conversation { id: "48:notes".into(), kind: ConvKind::SelfChat, ..Default::default() });
            }
            convs.sort_by_key(|c| std::cmp::Reverse(c.last_time()));
            let favorites: HashSet<String> = chat.favorites().unwrap_or_default().into_iter().collect();
            for c in &mut convs {
                c.pinned |= favorites.contains(&c.id);
            }
            post(Update::Conversations { tenant: t.clone(), list: convs.clone() });
            post(Update::Status { tenant: t.clone(), conn: Conn::Live });
            backoff = 2;
            resolve_names(&s, &chat, &convs, &mut known);
            refresh_presence(&s, &convs);
            let mut since_presence = 0u32;
            // Delta sync: each call returns only the conversations that changed (new message,
            // read state, rename…). Quick while things happen, slower when quiet.
            let mut quiet = 0u32;
            let mut since_fav = 0u32;
            while !s.stopped() {
                let secs = if quiet < 200 { 2 } else { 5 };
                sleep_while(&s, secs);
                if s.stopped() {
                    break;
                }
                if !s.skype.lock().unwrap().as_ref().is_some_and(|a| a.fresh()) {
                    chat = s.chat()?;
                }
                let (changed, next) = match chat.changes(&sync) {
                    Ok(r) => r,
                    Err(e) if e.to_string().starts_with("throttled") => {
                        let wait = e.to_string().rsplit(' ').next().and_then(|x| x.parse().ok()).unwrap_or(30);
                        sleep_while(&s, wait);
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                sync = next;
                since_fav += secs as u32;
                since_presence += secs as u32;
                for c in &changed {
                    if let Some(x) = convs.iter_mut().find(|x| x.id == c.id) {
                        *x = c.clone();
                    } else {
                        convs.push(c.clone());
                    }
                }
                if since_presence >= 60 {
                    since_presence = 0;
                    convs.sort_by_key(|c| std::cmp::Reverse(c.last_time()));
                    refresh_presence(&s, &convs);
                }
                let fav_refresh = since_fav >= 120;
                let favorites: Option<HashSet<String>> = if fav_refresh || !changed.is_empty() {
                    since_fav = 0;
                    chat.favorites().ok().map(|f| f.into_iter().collect())
                } else {
                    None
                };
                if changed.is_empty() {
                    quiet += 1;
                    if let Some(f) = favorites {
                        post(Update::Favorites { tenant: t.clone(), ids: f.into_iter().collect() });
                    }
                    continue;
                }
                quiet = 0;
                let fresh_peers: Vec<Conversation> = changed.iter().filter(|c| c.kind == ConvKind::OneOnOne && !known.contains(&c.peer)).cloned().collect();
                if !fresh_peers.is_empty() {
                    resolve_names(&s, &chat, &fresh_peers, &mut known);
                }
                for mut conv in changed {
                    if let Some(f) = &favorites {
                        conv.pinned |= f.contains(&conv.id);
                    }
                    post(Update::Conversation { tenant: t.clone(), conv });
                }
                if let Some(f) = favorites {
                    post(Update::Favorites { tenant: t.clone(), ids: f.into_iter().collect() });
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            let msg = format!("{e:#}");
            eprintln!("teamlite[{}]: {msg}", &t[..8.min(t.len())]);
            if msg.contains("HTTP 401") {
                s.invalidate();
            }
            post(Update::Status { tenant: t.clone(), conn: Conn::Retrying(short(&msg)) });
            if needs_interaction(&msg) {
                post(Update::NeedsSignIn { tenant: t.clone(), reason: short(&msg) });
                return; // the UI restarts this worker after the user signs in to the tenant
            }
            sleep_while(&s, backoff);
            backoff = (backoff * 2).min(60);
        }
    }
}

fn sleep_while(s: &Session, secs: u64) {
    for _ in 0..secs * 4 {
        if s.stopped() {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn short(s: &str) -> String {
    let line = s.lines().next().unwrap_or(s);
    line.chars().take(160).collect()
}
