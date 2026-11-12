//! App state and everything that changes it. Network results arrive as `worker::Update`s through
//! `App::apply`; the view (`ui.rs`) only reads.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use rune::{AppHandle, Image, ImageCache, TextEdit, Tx};

use crate::api::auth::{Identity, Tokens};
use crate::api::html::{self, Rendered};
use crate::api::teams::{ConvKind, Conversation, Message, Tenant};
use crate::api::now_ms;
use crate::store::{self, Account, TenantCache};
use crate::worker::{self, Command, Conn, Post, Update, Worker};

pub static HANDLE: OnceLock<AppHandle<App>> = OnceLock::new();

/// Runs `f` on the UI thread with the app state (no-op in headless runs without a handle).
pub fn ui(f: impl FnOnce(&mut App, &mut Tx) + Send + 'static) {
    if let Some(h) = HANDLE.get() {
        h.update(f);
    } else {
        crate::e2e::SINK.lock().unwrap().push(Box::new(f));
    }
}

pub enum Picture {
    Loading,
    Ready(Image),
    Failed,
}

/// The chat-service conversation id a reply chain is addressed by.
pub fn chain_key(conv: &str, root: &str) -> String {
    format!("{conv};messageid={root}")
}

pub struct Shown {
    pub msg: Message,
    pub rendered: Rendered,
    pub pending: bool,
    pub failed: bool,
}

impl Shown {
    pub fn new(msg: Message) -> Shown {
        let rendered = html::render(&msg.html);
        Shown { msg, rendered, pending: false, failed: false }
    }
}

#[derive(Default)]
pub struct Thread {
    /// Bumped whenever a server page replaces or extends the list.
    pub version: u64,
    pub list: Vec<Shown>,
    pub backward: Option<String>,
    pub loading: bool,
    pub loaded: bool,
}

impl Thread {
    fn upsert(&mut self, m: Message) -> bool {
        if let Some(i) = self.list.iter().position(|x| x.msg.id == m.id || (!m.client_message_id.is_empty() && x.msg.client_message_id == m.client_message_id)) {
            let changed = self.list[i].msg != m || self.list[i].pending;
            if changed {
                self.list[i] = Shown::new(m);
            }
            return changed;
        }
        let at = self.list.partition_point(|x| x.msg.time <= m.time);
        self.list.insert(at, Shown::new(m));
        true
    }
}

pub struct TenantState {
    pub tenant: Tenant,
    pub conn: Conn,
    pub me: String,
    pub me_name: String,
    pub convs: Vec<Conversation>,
    pub names: HashMap<String, String>,
    pub emails: HashMap<String, String>,
    pub members: HashMap<String, Vec<String>>,
    pub threads: HashMap<String, Thread>,
    pub selected: Option<String>,
    /// Open channel reply chain: root message id in the selected channel.
    pub reply_root: Option<String>,
    /// MRI → availability ("Available", "Busy", "Away", "Offline"…).
    pub presence: HashMap<String, String>,
    /// conversation → (name, until app time)
    pub typing: HashMap<String, (String, f64)>,
    pub error: Option<String>,
    /// Set when this organisation needs its own interactive sign-in (e.g. MFA).
    pub needs_sign_in: Option<String>,
    pub dirty_cache: bool,
}

impl TenantState {
    pub fn new(tenant: Tenant) -> TenantState {
        let cache = store::load_cache(&tenant.id);
        Self::from_cache(tenant, cache)
    }

    pub fn from_cache(tenant: Tenant, cache: TenantCache) -> TenantState {
        let mut threads = HashMap::new();
        for (id, msgs) in cache.messages {
            let list = msgs.into_iter().map(Shown::new).collect();
            threads.insert(id, Thread { list, ..Default::default() });
        }
        let mut convs = cache.conversations;
        sort_convs(&mut convs);
        TenantState {
            tenant,
            conn: Conn::Connecting,
            me: String::new(),
            me_name: String::new(),
            convs,
            names: cache.names,
            emails: HashMap::new(),
            members: HashMap::new(),
            threads,
            selected: None,
            reply_root: None,
            presence: HashMap::new(),
            typing: HashMap::new(),
            error: None,
            needs_sign_in: None,
            dirty_cache: false,
        }
    }

    pub fn conv(&self, id: &str) -> Option<&Conversation> {
        self.convs.iter().find(|c| c.id == id)
    }

    fn self_title(&self) -> String {
        if self.me_name.is_empty() { "Notes to self".into() } else { format!("{} (You)", self.me_name) }
    }

    pub fn unread(&self) -> usize {
        self.convs.iter().filter(|c| !c.hidden && c.unread(&self.me)).count()
    }

    pub fn title(&self, c: &Conversation) -> String {
        match c.kind {
            ConvKind::SelfChat => return self.self_title(),
            ConvKind::Channel => {
                return if c.team.is_empty() { c.topic.clone() } else { format!("{} › {}", c.team, c.topic) };
            }
            _ => {}
        }
        if !c.topic.is_empty() {
            return c.topic.clone();
        }
        if c.kind == ConvKind::OneOnOne {
            if let Some(n) = self.names.get(&c.peer) {
                return n.clone();
            }
            if let Some(m) = c.last.as_ref().filter(|m| m.from == c.peer && !m.from_name.is_empty()) {
                return m.from_name.clone();
            }
            if c.peer.is_empty() || c.peer == self.me {
                return self.self_title();
            }
            return "Chat".into();
        }
        if let Some(members) = self.members.get(&c.id) {
            let names: Vec<String> = members
                .iter()
                .filter(|m| **m != self.me)
                .filter_map(|m| self.names.get(m))
                .map(|n| n.split_whitespace().next().unwrap_or(n).to_string())
                .collect();
            if !names.is_empty() {
                let more = members.len().saturating_sub(names.len() + 1);
                return if more > 0 { format!("{} +{more}", names.join(", ")) } else { names.join(", ") };
            }
        }
        "Group chat".into()
    }

    fn save_cache(&mut self) {
        if !self.dirty_cache {
            return;
        }
        self.dirty_cache = false;
        let mut messages = HashMap::new();
        // Keep the last 40 messages of the 30 most recent chats: enough to open instantly.
        for c in self.convs.iter().take(30) {
            if let Some(t) = self.threads.get(&c.id) {
                let start = t.list.len().saturating_sub(40);
                let msgs: Vec<Message> = t.list[start..].iter().filter(|s| !s.pending).map(|s| s.msg.clone()).collect();
                messages.insert(c.id.clone(), msgs);
            }
        }
        let cache = TenantCache { conversations: self.convs.clone(), messages, names: self.names.clone() };
        let id = self.tenant.id.clone();
        std::thread::spawn(move || store::save_cache(&id, &cache));
    }
}

pub fn sort_convs(convs: &mut [Conversation]) {
    convs.sort_by(|a, b| b.last_time().cmp(&a.last_time()));
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    SignedOut,
    SigningIn,
    Ready,
}

pub struct App {
    pub phase: Phase,
    pub account: Option<Account>,
    pub tenants: Vec<TenantState>,
    pub active: usize,
    pub search: TextEdit,
    pub composer: TextEdit,
    pub login_error: Option<String>,
    pub toast: Option<(String, f64)>,
    pub clock: f64,
    pub window_focused: bool,
    pub show_menu: bool,
    /// Message whose action bar is open (react, reply, edit, delete).
    pub selected_msg: Option<String>,
    /// Message waiting for a second click to confirm deletion.
    pub confirm_delete: Option<String>,
    /// (thread key, message id) being edited in the composer.
    pub editing: Option<(String, String)>,
    pub images: Option<ImageCache>,
    pub last_opened: Option<String>,
    /// App time of the last typing notice sent (at most one every 3 s).
    typing_sent: f64,
    pub pictures: HashMap<String, Picture>,
    workers: HashMap<String, Worker>,
    save_at: f64,
}

impl Default for App {
    fn default() -> Self {
        App {
            phase: Phase::SignedOut,
            account: None,
            tenants: Vec::new(),
            active: 0,
            search: TextEdit::new(""),
            composer: TextEdit::new(""),
            login_error: None,
            toast: None,
            clock: 0.0,
            window_focused: true,
            show_menu: false,
            selected_msg: None,
            confirm_delete: None,
            editing: None,
            images: None,
            typing_sent: -10.0,
            last_opened: None,
            pictures: HashMap::new(),
            workers: HashMap::new(),
            save_at: 0.0,
        }
    }
}

fn post() -> Post {
    Arc::new(|u: Update| ui(move |s, tx| s.apply(u, tx)))
}

impl App {
    /// Startup: the saved account (if any) shows its cached chats at once; workers start in
    /// `start` after the first frame.
    pub fn load() -> App {
        let mut app = App::default();
        if let Some(acc) = store::load_account() {
            app.set_account(acc);
        }
        app
    }

    fn set_account(&mut self, acc: Account) {
        let mut tenants = acc.tenants.clone();
        if tenants.is_empty() {
            tenants.push(Tenant { id: acc.identity.tid.clone(), name: "Home".into(), user_id: acc.identity.oid.clone(), is_home: true });
        }
        self.tenants = tenants.into_iter().filter(|t| !acc.muted_tenants.contains(&t.id) || t.is_home).map(TenantState::new).collect();
        for t in &mut self.tenants {
            t.me_name = acc.identity.name.clone();
        }
        self.active = self.tenants.iter().position(|t| t.tenant.id == acc.active_tenant).unwrap_or(0);
        self.account = Some(acc);
        self.phase = Phase::Ready;
    }

    /// Starts a worker for every tenant that doesn't have one (idempotent).
    pub fn start(&mut self) {
        let Some(acc) = &self.account else { return };
        for t in &self.tenants {
            if self.workers.contains_key(&t.tenant.id) {
                continue;
            }
            let Some(rt) = acc.refresh_for(&t.tenant.id) else { continue };
            let w = worker::spawn(&t.tenant, rt.to_string(), post());
            self.workers.insert(t.tenant.id.clone(), w);
        }
    }

    pub fn sign_in(&mut self) {
        if self.phase == Phase::SigningIn {
            return;
        }
        self.phase = Phase::SigningIn;
        self.login_error = None;
        let hint = self.account.as_ref().map(|a| a.identity.upn.clone()).filter(|u| !u.is_empty());
        std::thread::spawn(move || {
            let r = crate::login::interactive(hint.as_deref());
            ui(move |s, _| match r {
                Ok((tokens, who)) => s.signed_in(tokens, who),
                Err(e) => {
                    s.phase = if s.account.is_some() { Phase::Ready } else { Phase::SignedOut };
                    s.login_error = Some(format!("{e:#}"));
                }
            });
        });
    }

    fn signed_in(&mut self, tokens: Tokens, who: Identity) {
        let mut acc = self.account.take().filter(|a| a.identity.oid == who.oid).unwrap_or_default();
        acc.refresh.insert(who.tid.clone(), tokens.refresh_token.clone());
        if acc.active_tenant.is_empty() {
            acc.active_tenant = who.tid.clone();
        }
        acc.identity = who;
        store::save_account(&acc);
        self.stop_workers();
        self.set_account(acc);
        self.start();
    }

    pub fn sign_out(&mut self) {
        self.stop_workers();
        store::forget_account();
        *self = App::default();
    }

    fn stop_workers(&mut self) {
        for (_, w) in self.workers.drain() {
            let _ = w.commands.send(Command::Stop);
        }
    }

    pub fn active(&self) -> Option<&TenantState> {
        self.tenants.get(self.active)
    }

    fn tenant_mut(&mut self, id: &str) -> Option<&mut TenantState> {
        self.tenants.iter_mut().find(|t| t.tenant.id == id)
    }

    fn send_cmd(&self, tenant: &str, c: Command) {
        if let Some(w) = self.workers.get(tenant) {
            let _ = w.commands.send(c);
        }
    }

    pub fn switch_tenant(&mut self, i: usize, tx: &mut Tx) {
        if i >= self.tenants.len() || i == self.active {
            return;
        }
        self.active = i;
        self.search.clear();
        if let Some(acc) = &mut self.account {
            acc.active_tenant = self.tenants[i].tenant.id.clone();
            store::save_account(acc);
        }
        if let Some(sel) = self.tenants[i].selected.clone() {
            self.open(&sel, tx);
        }
        tx.instant();
    }

    pub fn open(&mut self, conv: &str, tx: &mut Tx) {
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let changed = t.selected.as_deref() != Some(conv);
        t.selected = Some(conv.to_string());
        let tenant = t.tenant.id.clone();
        let thread = t.threads.entry(conv.to_string()).or_default();
        if !thread.loaded && !thread.loading {
            thread.loading = true;
            self.send_cmd(&tenant, Command::Open(conv.to_string()));
        }
        self.mark_read(conv);
        if changed {
            if let Some(t) = self.tenants.get_mut(self.active) {
                t.reply_root = None;
            }
            self.editing = None;
            self.selected_msg = None;
            self.composer.clear();
            self.composer.focused = true;
            tx.scroll_to_bottom("messages");
        }
    }

    fn mark_read(&mut self, conv: &str) {
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let me = t.me.clone();
        let tenant = t.tenant.id.clone();
        let Some(c) = t.convs.iter_mut().find(|c| c.id == conv) else { return };
        if !c.unread(&me) {
            return;
        }
        if let Some(last) = c.last.clone() {
            c.read_horizon = last.time;
            self.send_cmd(&tenant, Command::MarkRead(conv.to_string(), last));
        }
    }

    pub fn load_older(&mut self) {
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let Some(conv) = t.selected.clone() else { return };
        let tenant = t.tenant.id.clone();
        let Some(th) = t.threads.get_mut(&conv) else { return };
        if th.loading {
            return;
        }
        if let Some(link) = th.backward.clone() {
            th.loading = true;
            self.send_cmd(&tenant, Command::Older(conv, link));
        }
    }

    /// Where the composer posts: the open reply chain, else the conversation.
    pub fn target(&self) -> Option<String> {
        let t = self.active()?;
        let conv = t.selected.clone()?;
        Some(match &t.reply_root {
            Some(root) => chain_key(&conv, root),
            None => conv,
        })
    }

    pub fn send(&mut self, tx: &mut Tx) {
        let text = self.composer.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let display_name = self.account.as_ref().map(|a| a.identity.name.clone()).unwrap_or_default();
        if let Some((key, id)) = self.editing.take() {
            let tenant = self.active().map(|t| t.tenant.id.clone()).unwrap_or_default();
            if let Some(t) = self.tenants.get_mut(self.active)
                && let Some(th) = t.threads.get_mut(&key)
                && let Some(m) = th.list.iter_mut().find(|m| m.msg.id == id)
            {
                m.msg.html = html::to_html(&text);
                m.msg.edited = true;
                m.rendered = html::render(&m.msg.html);
                m.pending = true;
            }
            self.send_cmd(&tenant, Command::Edit { conv: key, id, text, display_name });
            self.composer.clear();
            return;
        }
        let Some(target) = self.target() else { return };
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let conv = t.selected.clone().unwrap_or_default();
        let client_id = format!("{}{}", now_ms(), rand::random::<u16>());
        let msg = Message {
            id: format!("pending-{client_id}"),
            conversation_id: conv.clone(),
            client_message_id: client_id.clone(),
            from: t.me.clone(),
            from_name: display_name.clone(),
            html: html::to_html(&text),
            message_type: "RichText/Html".into(),
            time: now_ms(),
            root: t.reply_root.clone().unwrap_or_default(),
            ..Default::default()
        };
        let th = t.threads.entry(target.clone()).or_default();
        let mut shown = Shown::new(msg.clone());
        shown.pending = true;
        th.list.push(shown);
        if let Some(c) = t.convs.iter_mut().find(|c| c.id == conv) {
            c.last = Some(msg.clone());
            c.read_horizon = msg.time;
        }
        sort_convs(&mut t.convs);
        let tenant = t.tenant.id.clone();
        let in_chain = t.reply_root.is_some();
        self.send_cmd(&tenant, Command::Send { conv: target, text, client_id, display_name });
        self.composer.clear();
        if !in_chain {
            tx.scroll_to_bottom("messages");
        }
    }

    /// Sends an image (from the attach dialog or the clipboard) with the composer's text as caption.
    pub fn send_image(&mut self, bytes: Vec<u8>, name: String, tx: &mut Tx) {
        if bytes.len() > 20 * 1024 * 1024 {
            self.toast("Images over 20 MB aren't supported".into(), tx);
            return;
        }
        if imagesize::blob_size(&bytes).is_err() {
            self.toast(format!("{name} isn't an image Teamlite can send"), tx);
            return;
        }
        let Some(target) = self.target() else { return };
        let text = std::mem::take(&mut self.composer.text);
        self.composer.clear();
        let display_name = self.account.as_ref().map(|a| a.identity.name.clone()).unwrap_or_default();
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let client_id = format!("{}{}", now_ms(), rand::random::<u16>());
        let caption = if text.trim().is_empty() { String::new() } else { html::to_html(text.trim()) };
        let msg = Message {
            id: format!("pending-{client_id}"),
            conversation_id: t.selected.clone().unwrap_or_default(),
            client_message_id: client_id.clone(),
            from: t.me.clone(),
            from_name: display_name.clone(),
            html: format!("{caption}<p>🖼 Sending {}…</p>", html_escape::encode_text(&name)),
            message_type: "RichText/Html".into(),
            time: now_ms(),
            ..Default::default()
        };
        let mut shown = Shown::new(msg);
        shown.pending = true;
        t.threads.entry(target.clone()).or_default().list.push(shown);
        let tenant = t.tenant.id.clone();
        self.send_cmd(&tenant, Command::SendImage { conv: target, bytes, name, text: text.trim().to_string(), client_id, display_name });
        tx.scroll_to_bottom("messages");
    }

    /// Sends any file: images inline (like Teams' paste), everything else through OneDrive.
    pub fn send_path(&mut self, path: std::path::PathBuf, tx: &mut Tx) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") {
            match std::fs::read(&path) {
                Ok(bytes) => self.send_image(bytes, name, tx),
                Err(e) => self.toast(format!("Couldn't read {name}: {e}"), tx),
            }
            return;
        }
        let Some(target) = self.target() else { return };
        let text = std::mem::take(&mut self.composer.text);
        self.composer.clear();
        let display_name = self.account.as_ref().map(|a| a.identity.name.clone()).unwrap_or_default();
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let conv = t.selected.clone().unwrap_or_default();
        // Who needs read access: the chat's members we know an email for (not channels: the
        // whole team reads the link through the organisation).
        let mut members: Vec<String> = t.members.get(&conv).cloned().unwrap_or_default();
        if let Some(c) = t.conv(&conv)
            && !c.peer.is_empty()
        {
            members.push(c.peer.clone());
        }
        let share_with: Vec<String> = members.iter().filter(|m| **m != t.me).filter_map(|m| t.emails.get(m).cloned()).collect();
        let client_id = format!("{}{}", now_ms(), rand::random::<u16>());
        let caption = if text.trim().is_empty() { String::new() } else { html::to_html(text.trim()) };
        let msg = Message {
            id: format!("pending-{client_id}"),
            conversation_id: conv,
            client_message_id: client_id.clone(),
            from: t.me.clone(),
            from_name: display_name.clone(),
            html: format!("{caption}<p>📎 Uploading {}…</p>", html_escape::encode_text(&name)),
            message_type: "RichText/Html".into(),
            time: now_ms(),
            ..Default::default()
        };
        let mut shown = Shown::new(msg);
        shown.pending = true;
        t.threads.entry(target.clone()).or_default().list.push(shown);
        let tenant = t.tenant.id.clone();
        self.send_cmd(&tenant, Command::SendFile { conv: target, path, text: text.trim().to_string(), client_id, display_name, share_with });
        tx.scroll_to_bottom("messages");
    }

    pub fn attach(&mut self, tx: &mut Tx) {
        let options = rune::FileDialogOptions { title: "Send a file".into(), ..Default::default() };
        tx.open_file(options, |s: &mut App, result, tx| match result {
            Ok(Some(path)) => s.send_path(path, tx),
            Ok(None) => {}
            Err(e) => s.toast(format!("File dialog unavailable: {e}"), tx),
        });
    }

    /// Opens a link in the browser (and remembers it, for tests).
    pub fn open_link(&mut self, url: &str, tx: &mut Tx) {
        self.last_opened = Some(url.to_string());
        let _ = tx.open_url(url);
    }

    /// Called as the composer changes: tells the chat someone is typing (throttled).
    pub fn typed(&mut self) {
        if self.editing.is_some() || self.composer.text.trim().is_empty() || self.clock - self.typing_sent < 3.0 {
            return;
        }
        let Some(target) = self.target() else { return };
        let Some(tenant) = self.active().map(|t| t.tenant.id.clone()) else { return };
        self.typing_sent = self.clock;
        self.send_cmd(&tenant, Command::Typing(target));
    }

    /// Teams web deep link for the open chat or channel (meetings, calls and files live there).
    pub fn web_link(&self) -> Option<String> {
        let t = self.active()?;
        let c = t.conv(t.selected.as_deref()?)?;
        let tid = &t.tenant.id;
        let id = crate::api::teams::urlenc(&c.id);
        Some(match c.kind {
            ConvKind::Channel => format!("https://teams.microsoft.com/l/channel/{id}/{}?tenantId={tid}", crate::api::teams::urlenc(&c.topic)),
            _ => format!("https://teams.microsoft.com/l/chat/{id}/conversations?tenantId={tid}"),
        })
    }

    /// Teams web deep link that starts a call with the 1:1 chat's other person.
    pub fn call_link(&self, video: bool) -> Option<String> {
        let t = self.active()?;
        let c = t.conv(t.selected.as_deref()?)?;
        if c.kind != ConvKind::OneOnOne {
            return None;
        }
        let email = t.emails.get(&c.peer)?;
        Some(format!("https://teams.microsoft.com/l/call/0/0?users={}{}", crate::api::teams::urlenc(email), if video { "&withVideo=true" } else { "" }))
    }

    /// Re-reads a thread's newest page from the server.
    pub fn reload_thread(&mut self, key: &str) {
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let tenant = t.tenant.id.clone();
        t.threads.entry(key.to_string()).or_default().loading = true;
        self.send_cmd(&tenant, Command::Open(key.to_string()));
    }

    pub fn select_message(&mut self, id: &str) {
        self.confirm_delete = None;
        self.selected_msg = if self.selected_msg.as_deref() == Some(id) { None } else { Some(id.to_string()) };
    }

    /// Toggles the user's reaction, showing it at once; the worker reloads the thread after.
    pub fn react(&mut self, key: &str, id: &str, emotion: &str) {
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let me = t.me.clone();
        let tenant = t.tenant.id.clone();
        let Some(m) = t.threads.get_mut(key).and_then(|th| th.list.iter_mut().find(|m| m.msg.id == id)) else { return };
        let had = m.msg.reactions.iter().any(|(k, users)| k == emotion && users.contains(&me));
        if had {
            for (k, users) in &mut m.msg.reactions {
                if k == emotion {
                    users.retain(|u| *u != me);
                }
            }
            m.msg.reactions.retain(|(_, u)| !u.is_empty());
        } else {
            match m.msg.reactions.iter_mut().find(|(k, _)| k == emotion) {
                Some((_, users)) => users.push(me),
                None => m.msg.reactions.push((emotion.to_string(), vec![me])),
            }
        }
        self.selected_msg = None;
        self.send_cmd(&tenant, Command::React { conv: key.to_string(), id: id.to_string(), key: emotion.to_string(), add: !had });
    }

    pub fn start_edit(&mut self, key: &str, id: &str, tx: &mut Tx) {
        let Some(t) = self.active() else { return };
        let Some(m) = t.threads.get(key).and_then(|th| th.list.iter().find(|m| m.msg.id == id)) else { return };
        let text = m.rendered.plain();
        self.composer.replace_text(&text);
        self.composer.focused = true;
        self.editing = Some((key.to_string(), id.to_string()));
        self.selected_msg = None;
        tx.focus("composer", false);
    }

    pub fn cancel_compose_mode(&mut self) {
        if self.editing.take().is_some() {
            self.composer.clear();
        }
        if let Some(t) = self.tenants.get_mut(self.active) {
            t.reply_root = None;
        }
    }

    pub fn delete(&mut self, key: &str, id: &str) {
        if self.confirm_delete.as_deref() != Some(id) {
            self.confirm_delete = Some(id.to_string());
            return;
        }
        self.confirm_delete = None;
        self.selected_msg = None;
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let tenant = t.tenant.id.clone();
        if let Some(m) = t.threads.get_mut(key).and_then(|th| th.list.iter_mut().find(|m| m.msg.id == id)) {
            m.msg.deleted = true;
        }
        self.send_cmd(&tenant, Command::Delete { conv: key.to_string(), id: id.to_string() });
    }

    /// Opens (or closes) a channel post's replies under it, and points the composer at them.
    pub fn toggle_replies(&mut self, root: &str) {
        let Some(t) = self.tenants.get_mut(self.active) else { return };
        let Some(conv) = t.selected.clone() else { return };
        if t.reply_root.as_deref() == Some(root) {
            t.reply_root = None;
            return;
        }
        t.reply_root = Some(root.to_string());
        let key = chain_key(&conv, root);
        let tenant = t.tenant.id.clone();
        let th = t.threads.entry(key.clone()).or_default();
        th.loading = true;
        self.selected_msg = None;
        self.send_cmd(&tenant, Command::Open(key));
    }

    /// Asks the worker for pictures in the open thread that aren't loaded yet.
    fn request_pictures(&mut self) {
        if self.images.is_none() {
            return;
        }
        let Some(t) = self.tenants.get(self.active) else { return };
        let Some(conv) = t.selected.clone() else { return };
        let mut keys = vec![conv.clone()];
        if let Some(r) = &t.reply_root {
            keys.push(chain_key(&conv, r));
        }
        let mut want = Vec::new();
        for k in keys {
            if let Some(th) = t.threads.get(&k) {
                for m in th.list.iter().rev().take(60) {
                    for u in &m.rendered.images {
                        if !self.pictures.contains_key(u) && !want.contains(u) {
                            want.push(u.clone());
                        }
                    }
                }
            }
        }
        let tenant = t.tenant.id.clone();
        for u in want.into_iter().take(8) {
            self.pictures.insert(u.clone(), Picture::Loading);
            self.send_cmd(&tenant, Command::Image(u));
        }
        // Bound the cache: drop the oldest finished pictures past 300.
        if self.pictures.len() > 300 {
            let drop: Vec<String> = self.pictures.iter().filter(|(_, p)| !matches!(p, Picture::Loading)).map(|(k, _)| k.clone()).take(self.pictures.len() - 300).collect();
            for k in drop {
                self.pictures.remove(&k);
            }
        }
    }

    pub fn total_unread(&self) -> usize {
        self.tenants.iter().map(|t| t.unread()).sum()
    }

    pub fn tick(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        self.request_pictures();
        let mut changed = false;
        for t in &mut self.tenants {
            let before = t.typing.len();
            t.typing.retain(|_, (_, until)| *until > clock);
            changed |= before != t.typing.len();
        }
        if self.toast.as_ref().is_some_and(|(_, until)| *until <= clock) {
            self.toast = None;
            changed = true;
        }
        if self.save_at > 0.0 && clock >= self.save_at {
            self.save_at = 0.0;
            for t in &mut self.tenants {
                t.save_cache();
            }
        }
        let n = self.total_unread();
        tx.set_title(if n > 0 { format!("({n}) Teamlite") } else { "Teamlite".into() });
        changed
    }

    fn schedule_save(&mut self, tx: &mut Tx) {
        if self.save_at == 0.0 {
            self.save_at = self.clock + 5.0;
            tx.wake_after(std::time::Duration::from_secs(6));
        }
    }

    pub fn apply(&mut self, u: Update, tx: &mut Tx) {
        let active_id = self.active().map(|t| t.tenant.id.clone()).unwrap_or_default();
        match u {
            Update::Status { tenant, conn } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    if conn == Conn::Live {
                        t.error = None;
                    }
                    t.conn = conn;
                }
            }
            Update::Me { tenant, mri } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.me = mri;
                }
            }
            Update::Conversations { tenant, list } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.convs = list;
                    sort_convs(&mut t.convs);
                    t.dirty_cache = true;
                    // Chats with new activity since the cached copy reload when opened.
                    for c in &t.convs {
                        if let Some(th) = t.threads.get_mut(&c.id)
                            && th.list.last().map(|s| s.msg.time) != c.last.as_ref().map(|m| m.time)
                        {
                            th.loaded = false;
                        }
                    }
                }
                self.schedule_save(tx);
                if tenant == active_id
                    && let Some(sel) = self.active().and_then(|t| t.selected.clone())
                {
                    self.open(&sel, tx);
                }
            }
            Update::Conversation { tenant, conv } => self.conversation_changed(tenant, conv, &active_id, tx),
            Update::Favorites { tenant, ids } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    for c in &mut t.convs {
                        c.pinned = c.starred || ids.contains(&c.id);
                    }
                }
            }
            Update::Image { url, bytes } => {
                let pic = match (bytes, &self.images) {
                    (Some(b), Some(cache)) => Picture::Ready(cache.load(b)),
                    _ => Picture::Failed,
                };
                self.pictures.insert(url, pic);
            }
            Update::Presence { tenant, list } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.presence.extend(list);
                }
            }
            Update::NeedsSignIn { tenant, reason } => {
                self.workers.remove(&tenant);
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.needs_sign_in = Some(reason);
                }
            }
            Update::Names { tenant, names, emails } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.names.extend(names);
                    t.emails.extend(emails);
                    t.dirty_cache = true;
                }
            }
            Update::Members { tenant, conv, members } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.members.insert(conv, members);
                }
            }
            Update::Messages { tenant, conv, msgs, backward, older } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    let th = t.threads.entry(conv.clone()).or_default();
                    th.loading = false;
                    th.loaded = true;
                    th.version += 1;
                    if !older {
                        // Replace with the server's newest page, keeping unsent messages.
                        let pending: Vec<Shown> = th.list.drain(..).filter(|s| s.pending || s.failed).collect();
                        th.list = msgs.into_iter().map(Shown::new).collect();
                        th.list.extend(pending);
                        th.backward = backward;
                    } else {
                        for m in msgs {
                            th.upsert(m);
                        }
                        th.backward = backward;
                    }
                    t.dirty_cache = true;
                }
                self.schedule_save(tx);
                if !older {
                    tx.scroll_to_bottom("messages");
                }
            }
            Update::Sent { tenant, conv, client_id, error } => {
                if let Some(t) = self.tenant_mut(&tenant)
                    && let Some(th) = t.threads.get_mut(&conv)
                    && let Some(s) = th.list.iter_mut().find(|s| s.msg.client_message_id == client_id)
                {
                    s.pending = false;
                    s.failed = error.is_some();
                }
                if let Some(e) = error {
                    self.toast(format!("Message not sent: {e}"), tx);
                }
            }
            Update::Error { tenant, text } => {
                if let Some(t) = self.tenant_mut(&tenant) {
                    t.error = Some(text.clone());
                }
                if tenant == active_id {
                    self.toast(text, tx);
                }
            }
            Update::Refreshed { tenant, refresh_token } => {
                if let Some(acc) = &mut self.account {
                    acc.refresh.insert(tenant, refresh_token);
                    let acc = acc.clone();
                    std::thread::spawn(move || store::save_account(&acc));
                }
            }
            Update::Tenants { list } => self.tenants_discovered(list),
        }
    }

    /// A conversation the delta sync reports as changed: new message, read elsewhere, renamed.
    fn conversation_changed(&mut self, tenant: String, conv: Conversation, active_id: &str, tx: &mut Tx) {
        let Some(t) = self.tenant_mut(&tenant) else { return };
        let selected = t.selected.as_deref() == Some(conv.id.as_str());
        let id = conv.id.clone();
        let new_last = conv.last.clone();
        let old_time = match t.convs.iter_mut().find(|c| c.id == conv.id) {
            Some(c) => {
                let old = c.last_time();
                let keep_last = if old > conv.last_time() { c.last.clone() } else { c.last.clone().or(conv.last.clone()) };
                let pinned = conv.pinned || (c.pinned && !c.starred);
                *c = Conversation { last: keep_last, pinned, ..conv };
                old
            }
            None => {
                t.convs.push(Conversation { last: None, ..conv });
                0
            }
        };
        sort_convs(&mut t.convs);
        t.dirty_cache = true;
        if let Some(m) = new_last.filter(|m| m.time > old_time) {
            // Several messages may have arrived since the last sync: reload the open thread.
            if let Some(th) = t.threads.get_mut(&id) {
                if selected && tenant == active_id {
                    th.loading = true;
                    let tid = tenant.clone();
                    self.send_cmd(&tid, Command::Open(id.clone()));
                    if let Some(root) = self.tenant_mut(&tenant).and_then(|t| t.reply_root.clone()) {
                        self.send_cmd(&tid, Command::Open(chain_key(&id, &root)));
                    }
                } else {
                    th.loaded = false;
                }
            }
            self.incoming(tenant, m, active_id, tx);
        }
        self.schedule_save(tx);
    }

    /// Interactive sign-in for one organisation (one that demands MFA or consent on its own).
    pub fn sign_in_tenant(&mut self, tenant: String) {
        let hint = self.account.as_ref().map(|a| a.identity.upn.clone()).filter(|u| !u.is_empty());
        if let Some(t) = self.tenant_mut(&tenant) {
            t.needs_sign_in = Some("Waiting for sign-in…".into());
        }
        std::thread::spawn(move || {
            let r = crate::login::interactive_in(&tenant, hint.as_deref());
            ui(move |s, tx| match r {
                Ok((tokens, _)) => {
                    if let Some(acc) = &mut s.account {
                        acc.refresh.insert(tenant.clone(), tokens.refresh_token);
                        store::save_account(acc);
                    }
                    if let Some(t) = s.tenant_mut(&tenant) {
                        t.needs_sign_in = None;
                    }
                    s.workers.remove(&tenant);
                    s.start();
                }
                Err(e) => {
                    if let Some(t) = s.tenant_mut(&tenant) {
                        t.needs_sign_in = Some(format!("{e:#}"));
                    }
                    s.toast(format!("Sign-in failed: {e:#}"), tx);
                }
            });
        });
    }

    fn tenants_discovered(&mut self, list: Vec<Tenant>) {
        let Some(acc) = &mut self.account else { return };
        if list.is_empty() || acc.tenants == list {
            return;
        }
        acc.tenants = list.clone();
        store::save_account(acc);
        let muted = acc.muted_tenants.clone();
        let active_id = self.tenants.get(self.active).map(|t| t.tenant.id.clone());
        for t in list {
            match self.tenants.iter_mut().find(|x| x.tenant.id == t.id) {
                Some(x) => x.tenant = t,
                None if !muted.contains(&t.id) => {
                    let mut ts = TenantState::new(t);
                    ts.me_name = acc.identity.name.clone();
                    self.tenants.push(ts)
                }
                None => {}
            }
        }
        // Home first, then by name.
        self.tenants.sort_by(|a, b| b.tenant.is_home.cmp(&a.tenant.is_home).then(a.tenant.name.to_lowercase().cmp(&b.tenant.name.to_lowercase())));
        if let Some(id) = active_id {
            self.active = self.tenants.iter().position(|t| t.tenant.id == id).unwrap_or(0);
        }
        self.start();
    }

    fn incoming(&mut self, tenant: String, msg: Message, active_id: &str, tx: &mut Tx) {
        let focused = self.window_focused;
        let Some(t) = self.tenant_mut(&tenant) else { return };
        let conv = msg.conversation_id.clone();
        let from_me = msg.from == t.me;
        let viewing = tenant == active_id && t.selected.as_deref() == Some(conv.as_str());
        t.typing.remove(&conv);
        if msg.is_chat_content() || msg.deleted {
            if let Some(th) = t.threads.get_mut(&conv) {
                th.upsert(msg.clone());
            }
            match t.convs.iter_mut().find(|c| c.id == conv) {
                Some(c) => {
                    if c.last_time() <= msg.time {
                        c.last = Some(msg.clone());
                    }
                    if from_me || (viewing && focused) {
                        c.read_horizon = c.read_horizon.max(msg.time);
                    }
                }
                None => t.convs.push(Conversation { id: conv.clone(), last: Some(msg.clone()), ..Default::default() }),
            }
            sort_convs(&mut t.convs);
            t.dirty_cache = true;
            if !from_me && !msg.deleted && !msg.edited && (!viewing || !focused) {
                let title = t.conv(&conv).map(|c| t.title(c)).unwrap_or_default();
                let who = if msg.from_name.is_empty() { t.names.get(&msg.from).cloned().unwrap_or_default() } else { msg.from_name.clone() };
                let org = t.tenant.name.clone();
                notify(&who, &title, &html::preview(&msg.html), &org);
            }
            if viewing && focused && !from_me {
                let tenant_id = tenant.clone();
                self.send_cmd(&tenant_id, Command::MarkRead(conv.clone(), msg));
            }
            if viewing {
                tx.scroll_to_bottom("messages");
            }
            self.schedule_save(tx);
        }
    }

    pub fn toast(&mut self, text: String, tx: &mut Tx) {
        self.toast = Some((text, self.clock + 5.0));
        tx.wake_after(std::time::Duration::from_millis(5100));
    }

    pub fn focus_changed(&mut self, focused: bool, tx: &mut Tx) {
        self.window_focused = focused;
        if focused
            && let Some(sel) = self.active().and_then(|t| t.selected.clone())
        {
            self.open(&sel, tx);
        }
    }
}

/// Desktop notification through the freedesktop notification service (`notify-send`).
fn notify(who: &str, chat: &str, body: &str, org: &str) {
    let title = if who.is_empty() || who == chat { chat.to_string() } else if chat.is_empty() { who.to_string() } else { format!("{who} · {chat}") };
    let body: String = body.chars().take(240).collect();
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=Teamlite", "--icon=internet-chat", "--category=im.received", "--hint=string:desktop-entry:teamlite"])
        .arg(&title)
        .arg(if org.is_empty() { body } else { format!("{body}\n— {org}") })
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut c| std::thread::spawn(move || c.wait()));
}
