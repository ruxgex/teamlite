//! The view: organisation rail, chat list, conversation. Reads `App`, never mutates it outside
//! handlers.

use chrono::{Datelike, Local, TimeZone};
use rune::tok::TextStyle;
use rune::{Color, Cx, El, Link, ScrollSpec, Span, col, el, rich, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, PaletteSeed, TextInput};

use crate::api::teams::{ConvKind, Conversation};
use crate::app::{App, Phase, Picture, Shown, TenantState, chain_key};
use crate::worker::Conn;

const RAIL_W: f32 = 64.0;
const LIST_W: f32 = 316.0;

pub fn kit() -> Kit {
    let mut k = Kit::from_palette(PaletteSeed {
        bg: rune::color::hex("#1b1c22").unwrap(),
        fg: rune::color::hex("#e9e9f0").unwrap(),
        accent: rune::color::hex("#7f85f5").unwrap(),
    });
    k.radius.sm = 6.0;
    k
}

fn accent() -> Color {
    rune::color::hex("#7f85f5").unwrap()
}

fn danger() -> Color {
    rune::color::hex("#ef4444").unwrap()
}

fn warning() -> Color {
    rune::color::hex("#eab308").unwrap()
}

fn ts(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight)
}

/// Stable avatar colour from an id.
fn hue(id: &str) -> Color {
    const P: [&str; 10] = ["#8b5cf6", "#3b82f6", "#0ea5e9", "#14b8a6", "#22c55e", "#eab308", "#f97316", "#ef4444", "#ec4899", "#6366f1"];
    let h = id.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    rune::color::hex(P[(h % P.len() as u32) as usize]).unwrap()
}

fn initials(name: &str) -> String {
    let mut it = name.split(|c: char| c.is_whitespace() || c == ',' || c == '›').filter(|w| w.chars().next().is_some_and(|c| c.is_alphanumeric()));
    let a = it.next().and_then(|w| w.chars().next());
    let b = it.next().and_then(|w| w.chars().next());
    [a, b].into_iter().flatten().flat_map(|c| c.to_uppercase()).collect()
}

fn avatar(id: &str, name: &str, size: f32, square: bool) -> El {
    let r = if square { size * 0.28 } else { size / 2.0 };
    el().size(size, size)
        .radius(r)
        .fill(hue(id).alpha(0.85))
        .center()
        .child(text(initials(name)).font(ts(size * 0.38, 600.0)).color(Color::WHITE))
}

pub fn when(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let Some(t) = Local.timestamp_millis_opt(ms).single() else { return String::new() };
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M").to_string()
    } else if (now.date_naive() - t.date_naive()).num_days() < 7 {
        t.format("%a").to_string()
    } else if t.year() == now.year() {
        t.format("%d %b").to_string()
    } else {
        t.format("%d/%m/%y").to_string()
    }
}

fn day_label(ms: i64) -> String {
    let Some(t) = Local.timestamp_millis_opt(ms).single() else { return String::new() };
    let now = Local::now();
    let days = (now.date_naive() - t.date_naive()).num_days();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => t.format("%A").to_string(),
        _ if t.year() == now.year() => t.format("%A %d %B").to_string(),
        _ => t.format("%d %B %Y").to_string(),
    }
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let _ = cx;
    let k = kit();
    let body = match s.phase {
        Phase::Ready if !s.tenants.is_empty() => main(s, &k),
        _ => sign_in(s, &k),
    };
    let mut root = col().w_full().h_full().fill(k.c.bg_base).on_window_focus(|s: &mut App, f, tx| s.focus_changed(f, tx))
        // A pasted image (screenshot, copied picture) is sent to the open chat.
        .on_paste_content(|s: &mut App, content: &rune::ClipboardContent, tx| {
            if let Some(img) = content.image() {
                s.send_image(img.png().to_vec(), "pasted-image.png".into(), tx);
            }
        })
        .child(body);
    if let Some((t, _)) = &s.toast {
        root = root.child(
            row().key("toast").absolute(0.0, 0.0).w_full().h_full().items_end().justify_center().pb(96.0).inert().child(
                el().px(16.0).py(10.0).radius(10.0).fill(k.c.surface_float).border(1.0, k.c.border_base).shadow(8.0, 24.0, Color::BLACK.alpha(0.35))
                    .max_w(520.0)
                    .child(text(t.clone()).font(ts(13.0, 450.0)).color(k.c.text_base).wrap()),
            ),
        );
    }
    root
}

fn sign_in(s: &App, k: &Kit) -> El {
    let busy = s.phase == Phase::SigningIn;
    let mut card = col().key("signin").w(380.0).padding(32.0).gap(16.0).radius(16.0).fill(k.c.layer_01).border(1.0, k.c.border_muted).items_center()
        .child(el().size(56.0, 56.0).radius(14.0).fill(accent()).center().child(text("T").font(ts(28.0, 700.0)).color(Color::WHITE)))
        .child(text("Teamlite").font(ts(22.0, 650.0)).color(k.c.text_base))
        .child(text("A fast native client for Microsoft Teams chat.").font(ts(13.0, 400.0)).color(k.c.text_muted))
        .child(el().h(8.0))
        .child(
            Button::new(if busy { "Waiting for sign-in…" } else { "Sign in with Microsoft" })
                .variant(ButtonVariant::Submit)
                .on_press(|s: &mut App| s.sign_in())
                .build(k)
                .key("signin-button"),
        );
    if let Some(e) = &s.login_error {
        card = card.child(text(e.clone()).key("login-error").font(ts(12.0, 400.0)).color(danger()).wrap().max_w(316.0));
    }
    col().w_full().h_full().center().child(card)
}

fn main(s: &App, k: &Kit) -> El {
    let t = &s.tenants[s.active.min(s.tenants.len() - 1)];
    row().w_full().h_full().min_h(0.0)
        .child(rail(s, k))
        .child(chat_list(s, t, k))
        .child(conversation(s, t, k))
}

// ---------------------------------------------------------------- organisation rail

fn rail(s: &App, k: &Kit) -> El {
    let mut tiles = col().gap(10.0).items_center();
    for (i, t) in s.tenants.iter().enumerate() {
        let active = i == s.active;
        let unread = t.unread();
        let name = t.tenant.name.clone();
        let mut tile = el().key(format!("org-{i}")).size(42.0, 42.0).radius(12.0)
            .fill(if active { hue(&t.tenant.id) } else { hue(&t.tenant.id).alpha(0.35) })
            .center()
            .role("tab")
            .label(name.clone())
            .help(format!("{name}{}", if i < 9 { format!("  (Ctrl+{})", i + 1) } else { String::new() }))
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| s.switch_tenant(i, tx))
            .child(text(initials(&t.tenant.name)).font(ts(14.0, 650.0)).color(Color::WHITE.alpha(if active { 1.0 } else { 0.8 })));
        if active {
            tile = tile.border(2.0, Color::WHITE.alpha(0.9));
        }
        let status = match t.conn {
            _ if t.needs_sign_in.is_some() => Some(warning()),
            Conn::Live => None,
            Conn::Connecting => Some(warning()),
            Conn::Retrying(_) => Some(danger()),
        };
        let mut wrap = el().size(46.0, 46.0).center().child(tile);
        if unread > 0 {
            let label = if unread > 99 { "99+".to_string() } else { unread.to_string() };
            wrap = wrap.child(
                el().absolute(26.0, -2.0).h(18.0).min_w(18.0).px(5.0).radius(9.0).fill(danger()).center().border(2.0, k.c.bg_deep)
                    .child(text(label).font(ts(10.0, 700.0)).color(Color::WHITE)),
            );
        }
        if let Some(c) = status {
            wrap = wrap.child(el().absolute(34.0, 34.0).size(10.0, 10.0).radius(5.0).fill(c).border(2.0, k.c.bg_deep));
        }
        tiles = tiles.child(wrap);
    }
    let me = s.account.as_ref().map(|a| (a.identity.oid.clone(), a.identity.name.clone())).unwrap_or_default();
    let account = el().key("account").on_press(|s: &mut App| s.show_menu = !s.show_menu).cursor(rune::Cursor::Pointer)
        .help("Account").child(avatar(&me.0, &me.1, 34.0, false));
    col().key("rail").w(RAIL_W).h_full().fill(k.c.bg_deep).pt(14.0).pb(14.0).items_center()
        .child(tiles)
        .child(spacer())
        .child(account)
}

// ---------------------------------------------------------------- chat list

fn conn_label(t: &TenantState) -> (String, Option<Color>) {
    match &t.conn {
        Conn::Live => (String::new(), None),
        Conn::Connecting => ("Connecting…".into(), Some(rune::color::hex("#eab308").unwrap())),
        Conn::Retrying(e) => (format!("Offline — {e}"), Some(rune::color::hex("#ef4444").unwrap())),
    }
}

fn chat_list(s: &App, t: &TenantState, k: &Kit) -> El {
    let q = s.search.text.trim().to_lowercase();
    let matches = |c: &Conversation| q.is_empty() || t.title(c).to_lowercase().contains(&q);
    let visible: Vec<&Conversation> = t.convs.iter().filter(|c| !c.hidden && c.kind != ConvKind::Other && matches(c)).collect();
    let pinned: Vec<&Conversation> = visible.iter().copied().filter(|c| c.pinned).collect();
    let chats: Vec<&Conversation> = visible.iter().copied().filter(|c| !c.pinned && c.kind != ConvKind::Channel).collect();
    let channels: Vec<&Conversation> = visible.iter().copied().filter(|c| !c.pinned && c.kind == ConvKind::Channel).collect();

    let mut list = col().key("chat-rows").w_full().pb(12.0);
    let section = |list: El, title: &str, convs: &[&Conversation]| -> El {
        if convs.is_empty() {
            return list;
        }
        let mut l = list.child(
            text(title.to_string()).font(ts(11.0, 650.0)).color(k.c.text_faint).px(16.0).pt(14.0).pb(6.0),
        );
        for c in convs.iter().take(400) {
            l = l.child(chat_row(t, c, k));
        }
        l
    };
    list = section(list, "PINNED", &pinned);
    list = section(list, "CHATS", &chats);
    list = section(list, "CHANNELS", &channels);
    if visible.is_empty() {
        let msg = if !q.is_empty() { "No chats match." } else if matches!(t.conn, Conn::Live) { "No chats yet." } else { "Loading chats…" };
        list = list.child(text(msg).font(ts(13.0, 400.0)).color(k.c.text_muted).px(16.0).pt(24.0));
    }

    let (status, dot) = conn_label(t);
    let mut header = col().px(16.0).pt(16.0).pb(10.0).gap(10.0).child(
        row().items_center().gap(8.0)
            .child(text(t.tenant.name.clone()).key("tenant-name").font(ts(18.0, 650.0)).color(k.c.text_base).truncate().shrink())
            .child(spacer()),
    );
    if let Some(c) = dot {
        header = header.child(
            row().items_center().gap(6.0)
                .child(el().size(7.0, 7.0).radius(4.0).fill(c))
                .child(text(status).font(ts(11.5, 450.0)).color(k.c.text_muted).truncate().shrink()),
        );
    }
    if let Some(reason) = &t.needs_sign_in {
        let tid = t.tenant.id.clone();
        let waiting = reason.starts_with("Waiting");
        header = header.child(
            col().key("tenant-signin").padding(12.0).gap(8.0).radius(10.0).fill(warning().alpha(0.12)).border(1.0, warning().alpha(0.4))
                .child(text(format!("{} needs you to sign in again (MFA or a policy of that organisation).", t.tenant.name)).font(ts(12.5, 500.0)).color(k.c.text_base).wrap())
                .child(text(reason.clone()).font(ts(11.0, 400.0)).color(k.c.text_muted).max_lines(2).wrap())
                .child(
                    Button::new(if waiting { "Waiting for sign-in…" } else { "Sign in to this organisation" })
                        .variant(ButtonVariant::Submit)
                        .on_press(move |s: &mut App| s.sign_in_tenant(tid.clone()))
                        .build(k)
                        .key("tenant-signin-button"),
                ),
        );
    }
    header = header.child(
        TextInput::new(&s.search)
            .placeholder("Search chats  (Ctrl+K)")
            .on_edit(|s: &mut App| &mut s.search)
            .build(k)
            .key("search"),
    );
    col().key("chat-list").w(LIST_W).h_full().min_h(0.0).fill(k.c.layer_01).border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_muted)
        .child(header)
        .child(list.scroll(ScrollSpec { stick_bottom: false }).grow(1.0).min_h(0.0))
}

fn chat_row(t: &TenantState, c: &Conversation, k: &Kit) -> El {
    let selected = t.selected.as_deref() == Some(c.id.as_str());
    let unread = c.unread(&t.me) && !selected;
    let title = t.title(c);
    let preview = match &c.last {
        Some(m) => {
            let p = crate::api::html::preview(&m.html);
            let who = if m.from == t.me {
                "You: ".to_string()
            } else if c.kind != ConvKind::OneOnOne && !m.from_name.is_empty() {
                format!("{}: ", m.from_name.split_whitespace().next().unwrap_or(""))
            } else {
                String::new()
            };
            if m.deleted { "This message has been deleted.".to_string() } else { format!("{who}{p}") }
        }
        None => String::new(),
    };
    let typing = t.typing.get(&c.id).map(|(n, _)| format!("{} is typing…", n.split_whitespace().next().unwrap_or(n)));
    let id = c.id.clone();
    let av_id = if c.kind == ConvKind::OneOnOne { c.peer.clone() } else { c.id.clone() };
    let square = matches!(c.kind, ConvKind::Channel | ConvKind::Meeting);
    let name_style = ts(13.5, if unread { 700.0 } else { 500.0 });
    let line2 = typing.clone().unwrap_or(preview);
    row().key(format!("chat-{}", c.id)).role("listitem").label(title.clone())
        .mx(8.0).px(8.0).py(8.0).gap(10.0).radius(8.0).items_center()
        .fill(if selected { k.c.layer_03 } else { Color::TRANSPARENT })
        .hover(if selected { Color::TRANSPARENT } else { k.c.hover })
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(move |s: &mut App, tx| s.open(&id, tx))
        .child(with_presence(avatar(&av_id, &title, 36.0, square), t.presence.get(&c.peer), 36.0, k))
        .child(
            col().grow(1.0).min_w(0.0).gap(2.0)
                .child(
                    row().items_center().gap(6.0)
                        .child(text(title).font(name_style).color(k.c.text_base).truncate().shrink().grow(1.0))
                        .child(text(when(c.last_time())).font(ts(11.0, 450.0)).color(if unread { k.c.text_base } else { k.c.text_faint })),
                )
                .child(
                    row().items_center().gap(6.0)
                        .child(text(line2).font(ts(12.5, if unread { 550.0 } else { 400.0 })).color(if typing.is_some() { accent() } else if unread { k.c.text_base } else { k.c.text_muted }).truncate().shrink().grow(1.0))
                        .child(if unread { el().size(8.0, 8.0).radius(4.0).fill(accent()) } else { el() }),
                ),
        )
}

// ---------------------------------------------------------------- conversation

fn conversation(s: &App, t: &TenantState, k: &Kit) -> El {
    let Some(conv) = t.selected.as_ref().and_then(|id| t.conv(id)) else {
        let hint = if t.convs.is_empty() { "" } else { "Pick a chat on the left." };
        let mut pane = col().key("empty").grow(1.0).h_full().center().gap(8.0)
            .child(text(if t.needs_sign_in.is_some() { format!("Not connected to {}", t.tenant.name) } else { format!("Signed in to {}", t.tenant.name) }).font(ts(16.0, 600.0)).color(k.c.text_muted))
            .child(text(hint).font(ts(13.0, 400.0)).color(k.c.text_faint));
        if s.show_menu {
            pane = pane.child(account_menu(s, k));
        }
        return pane;
    };
    let title = t.title(conv);
    let kind = match conv.kind {
        ConvKind::OneOnOne => "Chat",
        ConvKind::Group => "Group chat",
        ConvKind::Meeting => "Meeting chat",
        ConvKind::Channel => "Channel",
        ConvKind::SelfChat => "Your notes",
        ConvKind::Other => "",
    };
    let av_id = if conv.kind == ConvKind::OneOnOne { conv.peer.clone() } else { conv.id.clone() };
    let header = row().key("conv-header").h(60.0).px(20.0).gap(12.0).items_center().border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(with_presence(avatar(&av_id, &title, 32.0, matches!(conv.kind, ConvKind::Channel | ConvKind::Meeting)), t.presence.get(&conv.peer), 32.0, k))
        .child(
            col().gap(1.0).shrink().min_w(0.0)
                .child(text(title.clone()).key("conv-title").font(ts(15.5, 650.0)).color(k.c.text_base).truncate())
                .child(text(match t.presence.get(&conv.peer).filter(|_| conv.kind == ConvKind::OneOnOne) { Some(p) => presence_label(p), None => kind.to_string() }).font(ts(11.5, 450.0)).color(k.c.text_muted)),
        )
        .child(spacer())
        .child(header_actions(s, k));

    let empty = Default::default();
    let th = t.threads.get(&conv.id).unwrap_or(&empty);
    let mut msgs = col().key("message-rows").w_full().px(20.0).pt(12.0).pb(8.0);
    if th.backward.is_some() {
        msgs = msgs.child(
            row().justify_center().pb(8.0).child(
                Button::new(if th.loading { "Loading…" } else { "Load earlier messages" })
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.load_older())
                    .build(k)
                    .key("older"),
            ),
        );
    } else if th.loading && th.list.is_empty() {
        msgs = msgs.child(text("Loading messages…").font(ts(13.0, 400.0)).color(k.c.text_muted).pt(20.0));
    }
    let mut prev: Option<&Shown> = None;
    for m in th.list.iter().filter(|m| m.msg.is_chat_content() || m.msg.deleted) {
        let new_day = prev.is_none_or(|p| day_label(p.msg.time) != day_label(m.msg.time));
        if new_day {
            msgs = msgs.child(
                row().key(format!("day-{}", m.msg.id)).items_center().gap(12.0).py(12.0)
                    .child(el().h(1.0).grow(1.0).fill(k.c.border_muted))
                    .child(text(day_label(m.msg.time)).font(ts(11.5, 550.0)).color(k.c.text_faint))
                    .child(el().h(1.0).grow(1.0).fill(k.c.border_muted)),
            );
        }
        let channel = conv.kind == ConvKind::Channel;
        let grouped = !channel && !new_day && prev.is_some_and(|p| p.msg.from == m.msg.from && m.msg.time - p.msg.time < 5 * 60 * 1000);
        msgs = msgs.child(message(s, t, &conv.id, m, grouped, channel, k));
        if channel && t.reply_root.as_deref() == Some(m.msg.id.as_str()) {
            msgs = msgs.child(replies(s, t, &conv.id, &m.msg.id, k));
        }
        prev = Some(m);
    }
    let typing = t.typing.get(&conv.id).map(|(n, _)| format!("{n} is typing…")).unwrap_or_default();

    let mode = if s.editing.is_some() {
        Some("Editing your message")
    } else if t.reply_root.is_some() {
        Some("Replying in the open thread")
    } else {
        None
    };
    let mut status_row = row().key("compose-status").h(18.0).gap(10.0).items_center()
        .child(text(typing).key("typing").font(ts(11.5, 450.0)).color(k.c.text_muted));
    if let Some(m) = mode {
        status_row = status_row
            .child(text(m).key("compose-mode").font(ts(11.5, 600.0)).color(accent()))
            .child(text("Cancel (Esc)").key("compose-cancel").font(ts(11.5, 500.0)).color(k.c.text_muted).cursor(rune::Cursor::Pointer).on_press(|s: &mut App| s.cancel_compose_mode()));
    }
    let placeholder = match mode {
        Some(_) if s.editing.is_some() => "Edit the message, Enter to save",
        Some(_) => "Reply…  (Enter to send)",
        None => "Type a message  (Enter to send, Shift+Enter for a new line)",
    };
    let composer = col().key("composer-wrap").px(20.0).pt(4.0).pb(16.0).gap(4.0)
        .child(status_row)
        .child(
            row().gap(10.0).items_end()
                .child(
                    Button::new("+ Attach").variant(ButtonVariant::Ghost).on_press_tx(|s: &mut App, tx| s.attach(tx)).build(k).key("attach").help("Send a file or image (or paste an image with Ctrl+V)"),
                )
                .child(
                    // Rune's own field (not kit's TextArea) so it can take pasted images.
                    rune::text_input(&s.composer, ts(13.5, 400.0).lh(20.0))
                        .placeholder(placeholder, k.c.text_faint)
                        .on_edit(|s: &mut App| &mut s.composer)
                        .on_submit(|s: &mut App, tx| s.send(tx))
                        .on_input_change(|s: &mut App, _| s.typed())
                        .on_input_paste(|s: &mut App, content, tx| {
                            let Some(img) = content.image() else { return false };
                            s.send_image(img.png().to_vec(), "pasted-image.png".into(), tx);
                            true
                        })
                        .color(k.c.text_base)
                        .px(12.0)
                        .py(9.0)
                        .radius(10.0)
                        .fill(k.c.layer_02)
                        .border(1.0, k.c.border_base)
                        .max_h(200.0)
                        .key("composer")
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(
                    Button::new(if s.editing.is_some() { "Save" } else { "Send" })
                        .variant(ButtonVariant::Submit)
                        .on_press_tx(|s: &mut App, tx| s.send(tx))
                        .build(k)
                        .key("send"),
                ),
        );
    let mut pane = col().key("conversation").grow(1.0).h_full().min_w(0.0).min_h(0.0)
        .child(header)
        .child(msgs.scroll(ScrollSpec { stick_bottom: true }).key("messages").grow(1.0).min_h(0.0))
        .child(composer);
    if s.show_menu {
        pane = pane.child(account_menu(s, k));
    }
    pane
}

/// Calls, meetings and anything else Teamlite doesn't do natively open in Teams on the web.
fn header_actions(s: &App, k: &Kit) -> El {
    let mut r = row().gap(6.0).items_center();
    if s.call_link(false).is_some() {
        r = r
            .child(Button::new("Call").variant(ButtonVariant::Ghost).on_press_tx(|s: &mut App, tx| { if let Some(u) = s.call_link(false) { s.open_link(&u, tx); } }).build(k).key("call").help("Audio call in Teams on the web"))
            .child(Button::new("Video").variant(ButtonVariant::Ghost).on_press_tx(|s: &mut App, tx| { if let Some(u) = s.call_link(true) { s.open_link(&u, tx); } }).build(k).key("video").help("Video call in Teams on the web"));
    }
    r.child(Button::new("Open in Teams").variant(ButtonVariant::Neutral).on_press_tx(|s: &mut App, tx| { if let Some(u) = s.web_link() { s.open_link(&u, tx); } }).build(k).key("open-web").help("Meetings, calls and files in Teams on the web"))
}

fn account_menu(s: &App, k: &Kit) -> El {
    let who = s.account.as_ref().map(|a| (a.identity.name.clone(), a.identity.upn.clone())).unwrap_or_default();
    el().key("account-menu").absolute(-(LIST_W) - RAIL_W + 70.0, 0.0).w_full().h_full().items_end().pb(12.0).child(
        col().w(260.0).padding(14.0).gap(10.0).radius(12.0).fill(k.c.surface_float).border(1.0, k.c.border_base).shadow(10.0, 30.0, Color::BLACK.alpha(0.4))
            .child(text(who.0).font(ts(14.0, 650.0)).color(k.c.text_base))
            .child(text(who.1).font(ts(12.0, 400.0)).color(k.c.text_muted))
            .child(text(format!("{} organisations", s.tenants.len())).font(ts(12.0, 400.0)).color(k.c.text_muted))
            .child(Button::new("Sign in again").variant(ButtonVariant::Neutral).on_press(|s: &mut App| { s.show_menu = false; s.sign_in() }).build(k).key("reauth"))
            .child(Button::new("Sign out").variant(ButtonVariant::Ghost).on_press(|s: &mut App| s.sign_out()).build(k).key("signout")),
    )
}

fn presence_color(p: &str) -> Option<Color> {
    let hex = match p {
        "Available" | "AvailableIdle" => "#22c55e",
        "Busy" | "BusyIdle" | "DoNotDisturb" | "InACall" | "InAMeeting" | "Presenting" => "#ef4444",
        "Away" | "BeRightBack" => "#eab308",
        "Offline" | "PresenceUnknown" => "#71717a",
        _ => return None,
    };
    rune::color::hex(hex)
}

fn presence_label(p: &str) -> String {
    match p {
        "Available" | "AvailableIdle" => "Available".into(),
        "DoNotDisturb" => "Do not disturb".into(),
        "BeRightBack" => "Be right back".into(),
        "PresenceUnknown" => "Status unknown".into(),
        other => other.into(),
    }
}

fn with_presence(av: El, p: Option<&String>, size: f32, k: &Kit) -> El {
    let Some(c) = p.and_then(|p| presence_color(p)) else { return av };
    el().size(size, size).child(av).child(el().absolute(size - 11.0, size - 11.0).size(12.0, 12.0).radius(6.0).fill(c).border(2.0, k.c.layer_01))
}

const REACTIONS: [(&str, &str); 6] = [("like", "👍"), ("heart", "❤️"), ("laugh", "😆"), ("surprised", "😮"), ("sad", "😢"), ("angry", "😡")];

fn emoji_for(key: &str) -> String {
    REACTIONS.iter().find(|(k, _)| *k == key).map(|(_, e)| e.to_string()).unwrap_or_else(|| format!(":{key}:"))
}

fn pictures(s: &App, m: &Shown, k: &Kit) -> Option<El> {
    if m.rendered.images.is_empty() {
        return None;
    }
    let mut col_ = col().gap(6.0);
    for u in &m.rendered.images {
        let pic = match s.pictures.get(u) {
            Some(Picture::Ready(img)) if img.is_ready() => {
                let [w, h] = img.size().unwrap_or([320, 200]);
                let width = (w as f32).min(360.0).max(40.0);
                let height = width * h as f32 / w.max(1) as f32;
                let height = height.min(420.0);
                el().w(width).h(height).radius(8.0).pixels_fit(img.pixels(), rune::SurfaceFit::Fill)
            }
            Some(Picture::Failed) => text("🖼 image (couldn't load)").font(ts(12.5, 400.0)).color(k.c.text_faint),
            _ => el().w(220.0).h(140.0).radius(8.0).fill(k.c.layer_02).center().child(text("Loading image…").font(ts(12.0, 400.0)).color(k.c.text_faint)),
        };
        col_ = col_.child(pic);
    }
    Some(col_)
}

fn reactions_row(t: &TenantState, key: &str, m: &Shown, k: &Kit) -> Option<El> {
    if m.msg.reactions.is_empty() {
        return None;
    }
    let mut r = row().gap(6.0).flex_wrap();
    for (emo, users) in &m.msg.reactions {
        let mine = users.contains(&t.me);
        let (key, id, e) = (key.to_string(), m.msg.id.clone(), emo.clone());
        let who: Vec<String> = users.iter().map(|u| if *u == t.me { "You".to_string() } else { t.names.get(u).cloned().unwrap_or_else(|| "Someone".into()) }).collect();
        r = r.child(
            row().key(format!("react-{}-{emo}", m.msg.id)).gap(4.0).px(8.0).h(24.0).items_center().radius(12.0)
                .fill(if mine { accent().alpha(0.22) } else { k.c.layer_02 })
                .border(1.0, if mine { accent().alpha(0.7) } else { k.c.border_muted })
                .cursor(rune::Cursor::Pointer)
                .help(who.join(", "))
                .on_press(move |s: &mut App| s.react(&key, &id, &e))
                .child(text(emoji_for(emo)).font(ts(12.5, 400.0)))
                .child(text(users.len().to_string()).font(ts(11.5, 600.0)).color(k.c.text_base)),
        );
    }
    Some(r)
}

fn action_bar(s: &App, t: &TenantState, key: &str, m: &Shown, channel_root: bool, k: &Kit) -> El {
    let mut bar = row().key("msg-actions").gap(4.0).items_center().py(4.0);
    for (emo, glyph) in REACTIONS {
        let (key, id) = (key.to_string(), m.msg.id.clone());
        bar = bar.child(
            el().key(format!("add-{emo}")).size(30.0, 28.0).radius(8.0).center().hover(k.c.hover).cursor(rune::Cursor::Pointer)
                .help(emo)
                .on_press(move |s: &mut App| s.react(&key, &id, emo))
                .child(text(glyph).font(ts(15.0, 400.0))),
        );
    }
    bar = bar.child(el().w(8.0));
    if channel_root {
        let id = m.msg.id.clone();
        bar = bar.child(Button::new("Reply").variant(ButtonVariant::Ghost).on_press(move |s: &mut App| s.toggle_replies(&id)).build(k).key("action-reply"));
    }
    let plain = m.rendered.plain();
    bar = bar.child(Button::new("Copy").variant(ButtonVariant::Ghost).on_press_tx(move |s: &mut App, tx| { tx.copy(plain.clone()); s.selected_msg = None; }).build(k).key("action-copy"));
    if m.msg.from == t.me && !m.msg.deleted && !m.pending {
        let (k1, id1) = (key.to_string(), m.msg.id.clone());
        let (k2, id2) = (key.to_string(), m.msg.id.clone());
        bar = bar
            .child(Button::new("Edit").variant(ButtonVariant::Ghost).on_press_tx(move |s: &mut App, tx| s.start_edit(&k1, &id1, tx)).build(k).key("action-edit"))
            .child(
                Button::new(if s.confirm_delete.as_deref() == Some(m.msg.id.as_str()) { "Confirm delete" } else { "Delete" })
                    .variant(ButtonVariant::Danger)
                    .on_press(move |s: &mut App| s.delete(&k2, &id2))
                    .build(k)
                    .key("action-delete"),
            );
    }
    bar
}

/// A channel post's reply chain, shown under it.
fn replies(s: &App, t: &TenantState, conv: &str, root: &str, k: &Kit) -> El {
    let key = chain_key(conv, root);
    let mut c = col().key("replies").ml(48.0).pl(12.0).gap(2.0).border_sides([0.0, 0.0, 0.0, 2.0], accent().alpha(0.5));
    match t.threads.get(&key) {
        Some(th) if !th.list.is_empty() => {
            for m in th.list.iter().filter(|m| m.msg.id != root && (m.msg.is_chat_content() || m.msg.deleted)) {
                c = c.child(message(s, t, &key, m, false, false, k));
            }
            if th.list.iter().all(|m| m.msg.id == root) {
                c = c.child(text("No replies yet.").font(ts(12.5, 400.0)).color(k.c.text_faint).py(6.0));
            }
        }
        Some(th) if th.loading => c = c.child(text("Loading replies…").font(ts(12.5, 400.0)).color(k.c.text_faint).py(6.0)),
        _ => c = c.child(text("No replies yet.").font(ts(12.5, 400.0)).color(k.c.text_faint).py(6.0)),
    }
    c
}

fn message(s: &App, t: &TenantState, key: &str, m: &Shown, grouped: bool, channel_root: bool, k: &Kit) -> El {
    let mine = m.msg.from == t.me;
    let name = if mine {
        "You".to_string()
    } else if !m.msg.from_name.is_empty() {
        m.msg.from_name.clone()
    } else {
        t.names.get(&m.msg.from).cloned().unwrap_or_else(|| "Unknown".into())
    };
    let mut bubble = col().gap(6.0).max_w(680.0).shrink().min_w(0.0);
    if m.msg.deleted {
        bubble = bubble.child(text("This message has been deleted.").font(ts(13.5, 400.0).italic()).color(k.c.text_faint));
    } else {
        if let Some(q) = &m.rendered.quoted {
            bubble = bubble.child(
                el().pl(10.0).py(4.0).border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_strong)
                    .child(text(q.clone()).font(ts(12.5, 400.0)).color(k.c.text_muted).max_lines(3).wrap()),
            );
        }
        if !m.rendered.plain().trim().is_empty() || (m.rendered.images.is_empty() && m.msg.files.is_empty()) {
            bubble = bubble.child(rich_body(m, k));
        }
        if let Some(p) = pictures(s, m, k) {
            bubble = bubble.child(p);
        }
    }
    let mut file_chips = col().gap(6.0);
    for (name, url, size) in m.msg.files.iter().filter(|_| !m.msg.deleted) {
        let url = url.clone();
        let kb = if *size >= 1024 * 1024 { format!("{:.1} MB", *size as f64 / 1048576.0) } else { format!("{} KB", (*size).div_ceil(1024)) };
        file_chips = file_chips.child(
            row().key(format!("file-{}", name)).gap(10.0).px(12.0).py(8.0).radius(10.0).items_center().max_w(420.0)
                .fill(k.c.layer_02).border(1.0, k.c.border_muted).hover(k.c.hover).cursor(rune::Cursor::Pointer)
                .help("Open in the browser")
                .on_press_tx(move |s: &mut App, tx| s.open_link(&url, tx))
                .child(text("📎").font(ts(16.0, 400.0)))
                .child(col().min_w(0.0).shrink().child(text(name.clone()).font(ts(13.0, 600.0)).color(k.c.text_base).truncate()).child(text(kb).font(ts(11.0, 450.0)).color(k.c.text_faint))),
        );
    }
    let mut meta = row().gap(8.0).items_center();
    if !grouped {
        meta = meta.child(text(name.clone()).font(ts(13.0, 650.0)).color(k.c.text_base));
        meta = meta.child(text(when_exact(m.msg.time)).font(ts(11.0, 450.0)).color(k.c.text_faint));
    }
    if m.msg.edited && !m.msg.deleted {
        meta = meta.child(text("Edited").font(ts(11.0, 450.0)).color(k.c.text_faint));
    }
    if m.pending {
        meta = meta.child(text("Sending…").font(ts(11.0, 450.0)).color(k.c.text_faint));
    }
    if m.failed {
        meta = meta.child(text("Not sent").font(ts(11.0, 600.0)).color(danger()));
    }
    // Only the message itself selects; reactions and actions sit beside it, not inside it, so
    // their clicks don't reach the selection handler.
    let id = m.msg.id.clone();
    let body = col().key("msg-body").gap(3.0).min_w(0.0).cursor(rune::Cursor::Pointer)
        .on_press(move |s: &mut App| s.select_message(&id))
        .child(meta)
        .child(bubble);
    let mut content = col().gap(4.0).grow(1.0).min_w(0.0).child(body);
    if !m.msg.files.is_empty() && !m.msg.deleted {
        content = content.child(file_chips);
    }
    if !m.msg.deleted
        && let Some(r) = reactions_row(t, key, m, k)
    {
        content = content.child(r);
    }
    if channel_root && !m.msg.deleted {
        let open = t.reply_root.as_deref() == Some(m.msg.id.as_str());
        let id = m.msg.id.clone();
        content = content.child(
            text(if open { "Hide replies" } else { "Replies" }).key(format!("replies-{}", m.msg.id)).font(ts(12.0, 600.0)).color(accent())
                .cursor(rune::Cursor::Pointer).on_press(move |s: &mut App| s.toggle_replies(&id)),
        );
    }
    let selected = s.selected_msg.as_deref() == Some(m.msg.id.as_str()) && !m.msg.id.starts_with("pending-");
    if selected {
        content = content.child(action_bar(s, t, key, m, channel_root, k));
    }
    let left: El = if grouped { el().w(36.0) } else { avatar(&m.msg.from, &name, 36.0, false) };
    row().key(format!("msg-{}", if m.msg.id.is_empty() { &m.msg.client_message_id } else { &m.msg.id }))
        .gap(12.0).py(if grouped { 2.0 } else { 6.0 }).px(8.0).radius(8.0)
        .fill(if selected { k.c.layer_02 } else { Color::TRANSPARENT })
        .hover(k.c.hover)
        .opacity(if m.pending { 0.7 } else { 1.0 })
        .child(left)
        .child(content)
}

fn when_exact(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

fn rich_body(m: &Shown, k: &Kit) -> El {
    let mut s = String::new();
    let mut spans = Vec::new();
    let mut links = Vec::new();
    for r in &m.rendered.runs {
        let start = s.len();
        s.push_str(&r.text);
        let end = s.len();
        if start == end {
            continue;
        }
        let mut sp = Span::new(start, end);
        let mut styled = false;
        if r.bold {
            sp = sp.bold();
            styled = true;
        }
        if r.italic {
            sp = sp.italic();
            styled = true;
        }
        if r.code {
            sp = sp.mono();
            sp.bg = Some(k.c.layer_03);
            sp.bg_pad = 2.0;
            styled = true;
        }
        if r.mention {
            sp = sp.color(accent()).weight(600.0);
            styled = true;
        }
        if let Some(l) = &r.link
            && !l.is_empty()
        {
            sp = sp.color(rune::color::hex("#9ea4ff").unwrap());
            sp.underline = Some(rune::color::hex("#9ea4ff").unwrap().alpha(0.6));
            styled = true;
            links.push(Link::new(start..end, l.clone()));
        }
        if styled {
            spans.push(sp);
        }
    }
    if s.is_empty() {
        s = if m.rendered.has_image { "🖼 image".into() } else { " ".into() };
    }
    let mut body = rich(s, spans).font(ts(13.5, 400.0).lh(20.0)).color(k.c.text_base).wrap().selectable(true);
    for l in links {
        body = body.link(l);
    }
    body.on_link(|_: &mut App, url: &str, tx| {
        let _ = tx.open_url(url);
    })
}
