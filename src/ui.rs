//! The view: organisation rail, chat list, conversation. Reads `App`, never mutates it outside
//! handlers.
//!
//! Visual language: Fluent 2 (Windows 11) dark, pushed further. A luminous brand-glow canvas sits
//! behind everything; the chat list and floating surfaces are Acrylic (backdrop blur + tint +
//! noise + top-lit stroke), the conversation is a Mica-like layer with Windows 11's rounded
//! top-left content corner.

use chrono::{Datelike, Local, TimeZone};
use rune::el::Shadow;
use rune::tok::TextStyle;
use rune::{Color, Cx, El, Link, LoopSpec, ScrollSpec, Span, Spring, SpringPair, Transition, col, el, rich, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, PaletteSeed, TextInput};

use crate::api::teams::{ConvKind, Conversation};
use crate::app::{App, Phase, Picture, Shown, TenantState, chain_key};
use crate::worker::Conn;

const RAIL_W: f32 = 68.0;
const LIST_W: f32 = 320.0;

// ---------------------------------------------------------------- tokens

fn hex(h: &str) -> Color {
    rune::color::hex(h).unwrap()
}

/// The canvas under every layer: a blue-black, never pure black.
fn canvas() -> Color {
    hex("#0c0d14")
}

/// Teams' dark brand foreground; the one accent.
fn accent() -> Color {
    hex("#7f85f5")
}

/// Brand background ramp, for fills that carry the accent (own bubbles, primary tiles).
fn brand_deep() -> Color {
    hex("#444791")
}

fn danger() -> Color {
    hex("#e5546b")
}

fn warning() -> Color {
    hex("#f2b544")
}

/// Mica-like content layer.
fn layer() -> Color {
    hex("#15161f").alpha(0.9)
}

/// Raised card on the layer (other people's bubbles, chips).
fn card() -> Color {
    hex("#232532")
}

/// Fluent accent button: brand fill, light ink, top-lit rim. Kit's `Submit` paints a white
/// button, so this restyles a `Neutral` one.
fn primary(b: El) -> El {
    b.fill(hex("#5b5fc7"))
        .linear_gradient(std::f32::consts::PI, [(0.0, hex("#6f74de")), (1.0, hex("#4f52b2"))])
        .border_gradient(1.0, Color::WHITE.alpha(0.28), Color::BLACK.alpha(0.15))
        .hover(hex("#6c71d8"))
        .box_shadow(Shadow { y: 4.0, blur: 14.0, color: hex("#5b5fc7").alpha(0.45), ..Shadow::default() })
}

/// Paper plane in the Fluent System Icons style (filled, 20-unit grid).
static SEND_ICON: rune::Icon = rune::Icon {
    name: "teamlite-send",
    view: 20.0,
    body: r#"<path d="M2.7 2.1 18 9.55c.55.27.55.63 0 .9L2.7 17.9c-.5.24-.85-.06-.72-.6L3.7 10.6 11 10 3.7 9.4 1.98 2.7c-.13-.54.22-.84.72-.6Z"/>"#,
};

/// Checkmark for saving an edit.
static SAVE_ICON: rune::Icon = rune::Icon {
    name: "teamlite-save",
    view: 20.0,
    body: r#"<path d="M7.6 15.4 3 10.8l1.4-1.4 3.2 3.2 8-8L17 6Z"/>"#,
};

/// Handset (Material-style, 24-unit grid).
static CALL_ICON: rune::Icon = rune::Icon {
    name: "teamlite-call",
    view: 24.0,
    body: r#"<path d="M6.62 10.79c1.44 2.83 3.76 5.14 6.59 6.59l2.2-2.2c.27-.27.67-.36 1.02-.24 1.12.37 2.33.57 3.57.57.55 0 1 .45 1 1V20c0 .55-.45 1-1 1-9.39 0-17-7.61-17-17 0-.55.45-1 1-1h3.5c.55 0 1 .45 1 1 0 1.25.2 2.45.57 3.57.11.35.03.74-.25 1.02l-2.2 2.2Z"/>"#,
};

/// Video camera.
static VIDEO_ICON: rune::Icon = rune::Icon {
    name: "teamlite-video",
    view: 24.0,
    body: r#"<path d="M17 10.5V7c0-.55-.45-1-1-1H4c-.55 0-1 .45-1 1v10c0 .55.45 1 1 1h12c.55 0 1-.45 1-1v-3.5l4 4v-11l-4 4Z"/>"#,
};

/// Open in a new window (external link).
static OPEN_EXTERNAL_ICON: rune::Icon = rune::Icon {
    name: "teamlite-open-external",
    view: 24.0,
    body: r#"<path d="M19 19H5V5h7V3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14c1.1 0 2-.9 2-2v-7h-2v7ZM14 3v2h3.59l-9.83 9.83 1.41 1.41L19 6.41V10h2V3h-7Z"/>"#,
};

/// Square icon-only button; `help` doubles as tooltip and accessible label.
fn icon_button(key: &'static str, icon: &'static rune::Icon, help: &'static str, color: Color) -> El {
    el().key(key).size(32.0, 32.0).radius(8.0).center()
        .role("button").label(help).help(help)
        .cursor(rune::Cursor::Pointer).press_scale(0.92)
        .child(rune::icon(icon, 18.0).color(color))
}

fn stroke() -> Color {
    Color::WHITE.alpha(0.07)
}

fn ts(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight)
}

/// Fluent's typography ramp, slightly heavier at the top.
fn display(size: f32) -> TextStyle {
    ts(size, 700.0).tracking(-size * 0.025)
}

fn caption() -> TextStyle {
    ts(12.0, 450.0).lh(16.0)
}

fn tabular(s: TextStyle) -> TextStyle {
    s.features("'tnum' 1")
}

pub fn kit() -> Kit {
    let mut k = Kit::from_palette(PaletteSeed { bg: hex("#1a1b25"), fg: hex("#f1f1f6"), accent: accent() });
    k.radius.sm = 6.0;
    k
}

/// Fluent Acrylic: blurs what's behind it, tints it, adds a top-lit stroke.
fn acrylic(e: El, tint: Color) -> El {
    e.backdrop_blur(36.0).fill(tint).border_gradient(1.0, Color::WHITE.alpha(0.13), Color::WHITE.alpha(0.04))
}

/// A soft elevation shadow tinted with the canvas hue rather than black.
fn elevation(e: El, y: f32, blur: f32) -> El {
    e.box_shadow(Shadow { y, blur, color: hex("#04050c").alpha(0.55), ..Shadow::default() })
        .box_shadow(Shadow { y: 1.0, blur: 3.0, color: hex("#04050c").alpha(0.35), ..Shadow::default() })
}

/// Avatar colours: Fluent's dark avatar pairs (deep shade fill, light tint ink) instead of
/// saturated blobs.
fn hue_pair(id: &str) -> (Color, Color) {
    const P: [(&str, &str); 10] = [
        ("#5c1d2b", "#f4b6c2"),
        ("#5a3d0c", "#f6d49a"),
        ("#0f4a3d", "#9fe2cd"),
        ("#133d6b", "#a9d3f2"),
        ("#3b2c70", "#cabff6"),
        ("#5a1f50", "#efb3e2"),
        ("#304a14", "#c5e59b"),
        ("#0b4458", "#95dcef"),
        ("#52300f", "#f0c6a0"),
        ("#2b2f70", "#b8bdf7"),
    ];
    let h = id.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let (bg, fg) = P[(h % P.len() as u32) as usize];
    (hex(bg), hex(fg))
}

fn initials(name: &str) -> String {
    let mut it = name.split(|c: char| c.is_whitespace() || c == ',' || c == '›').filter(|w| w.chars().next().is_some_and(|c| c.is_alphanumeric()));
    let a = it.next().and_then(|w| w.chars().next());
    let b = it.next().and_then(|w| w.chars().next());
    [a, b].into_iter().flatten().flat_map(|c| c.to_uppercase()).collect()
}

/// People are circles; chats with many people (channels, meetings) are rounded squares.
fn avatar(id: &str, name: &str, size: f32, square: bool) -> El {
    let r = if square { size * 0.26 } else { size / 2.0 };
    let (bg, fg) = hue_pair(id);
    el().size(size, size)
        .radius(r)
        .fill(bg)
        .border(1.0, fg.alpha(0.18))
        .center()
        .child(text(initials(name)).font(ts(size * 0.37, 650.0)).color(fg))
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

// ---------------------------------------------------------------- canvas

thread_local! {
    /// Fluent Acrylic's noise layer: a fixed grain texture, generated once.
    static NOISE: rune::Pixels = {
        let (w, h) = (640u32, 640u32);
        let mut seed = 0x9e37_79b9u32;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let v = (seed >> 24) as u8;
            rgba.extend_from_slice(&[255, 255, 255, v / 22]);
        }
        let p = rune::Pixels::new();
        p.set(w, h, rgba);
        p
    };
}

/// Brand glows under every layer; what the Acrylic surfaces blur.
fn backdrop() -> El {
    let glow = |center: [f32; 2], r: [f32; 2], c: Color| {
        el().absolute(0.0, 0.0).w_full().h_full().radial_gradient(center, r, [(0.0, c), (0.55, c.alpha(c.a * 0.35)), (1.0, c.alpha(0.0))])
    };
    let noise = NOISE.with(|p| el().absolute(0.0, 0.0).w_full().h_full().pixels_fit(p, rune::SurfaceFit::Fill));
    el().key("canvas").absolute(0.0, 0.0).w_full().h_full().inert()
        .child(glow([0.02, -0.05], [760.0, 620.0], hex("#5b5fc7").alpha(0.62)))
        .child(glow([0.30, 1.10], [820.0, 520.0], hex("#6b3fb8").alpha(0.40)))
        .child(glow([1.02, 0.0], [700.0, 460.0], hex("#2f6fd6").alpha(0.30)))
        .child(noise)
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let _ = cx;
    let k = kit();
    let body = match s.phase {
        Phase::Ready if !s.tenants.is_empty() => main(s, &k),
        _ => sign_in(s, &k),
    };
    let mut root = col().w_full().h_full().fill(canvas()).on_window_focus(|s: &mut App, f, tx| s.focus_changed(f, tx))
        // A pasted image (screenshot, copied picture) is sent to the open chat.
        .on_paste_content(|s: &mut App, content: &rune::ClipboardContent, tx| {
            if let Some(img) = content.image() {
                s.send_image(img.png().to_vec(), "pasted-image.png".into(), tx);
            }
        })
        .child(backdrop())
        .child(body);
    if s.show_menu && s.phase == Phase::Ready && !s.tenants.is_empty() {
        root = root.child(account_menu(s, &k));
    }
    if let Some((t, _)) = &s.toast {
        root = root.child(
            row().key("toast").absolute(0.0, 0.0).w_full().h_full().items_end().justify_center().pb(128.0).inert().child(
                elevation(acrylic(el(), hex("#262836").alpha(0.82)), 12.0, 40.0).px(18.0).py(12.0).radius(10.0)
                    .max_w(520.0)
                    .child(text(t.clone()).font(ts(13.5, 500.0)).color(k.c.text_base).wrap()),
            ),
        );
    }
    root
}

fn sign_in(s: &App, k: &Kit) -> El {
    let busy = s.phase == Phase::SigningIn;
    let mark = el().size(72.0, 72.0).radius(20.0)
        .fill(brand_deep())
        .linear_gradient(2.4, [(0.0, hex("#9aa0ff")), (1.0, hex("#4f52b2"))])
        .border_gradient(1.0, Color::WHITE.alpha(0.35), Color::WHITE.alpha(0.05))
        .box_shadow(Shadow { y: 14.0, blur: 40.0, color: hex("#5b5fc7").alpha(0.55), ..Shadow::default() })
        .center()
        .child(text("T").font(display(36.0)).color(Color::WHITE));
    let mut card = elevation(acrylic(col(), hex("#1b1c27").alpha(0.72)), 24.0, 64.0).key("signin").w(420.0).px(40.0).pt(44.0).pb(40.0).gap(12.0).radius(16.0).items_center()
        .child(mark)
        .child(el().h(10.0))
        .child(text("Teamlite").font(display(32.0)).color(k.c.text_base))
        .child(text("Your Teams chats, native on Linux.").font(ts(15.0, 450.0)).color(k.c.text_muted))
        .child(el().h(18.0))
        .child(
            primary(Button::new(if busy { "Waiting for sign-in…" } else { "Sign in with Microsoft" })
                .variant(ButtonVariant::Neutral)
                .on_press(|s: &mut App| s.sign_in())
                .build(k))
                .key("signin-button")
                .w_full()
                .h(40.0)
                .press_scale(0.97),
        )
        .child(text("Chats only. Calls and meetings open in Teams on the web.").font(caption()).color(k.c.text_faint).pt(6.0));
    if let Some(e) = &s.login_error {
        card = card.child(text(e.clone()).key("login-error").font(ts(12.5, 500.0)).color(danger()).wrap().max_w(340.0));
    }
    col().w_full().h_full().center().child(card)
}

fn main(s: &App, k: &Kit) -> El {
    let t = &s.tenants[s.active.min(s.tenants.len() - 1)];
    row().w_full().h_full().min_h(0.0)
        .child(rail(s))
        .child(chat_list(s, t, k))
        .child(conversation(s, t, k))
}

// ---------------------------------------------------------------- organisation rail

/// Fluent's selection indicator: a short accent pill on the leading edge.
fn indicator(h: f32) -> El {
    el().w(3.0).h(h).radius(1.5).fill(accent())
}

fn rail(s: &App) -> El {
    let mut tiles = col().gap(8.0).items_center();
    for (i, t) in s.tenants.iter().enumerate() {
        let active = i == s.active;
        let unread = t.unread();
        let name = t.tenant.name.clone();
        let (bg, fg) = hue_pair(&t.tenant.id);
        let mut tile = el().key(format!("org-{i}")).size(44.0, 44.0).radius(if active { 14.0 } else { 12.0 })
            .fill(if active { bg.mix(Color::WHITE, 0.08) } else { bg.alpha(0.55) })
            .border(1.0, if active { fg.alpha(0.45) } else { Color::WHITE.alpha(0.06) })
            .hover(bg.mix(Color::WHITE, 0.12))
            .press_scale(0.94)
            .center()
            .role("tab")
            .label(name.clone())
            .help(format!("{name}{}", if i < 9 { format!("  (Ctrl+{})", i + 1) } else { String::new() }))
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut App, tx| s.switch_tenant(i, tx))
            .child(text(initials(&t.tenant.name)).font(ts(15.0, 700.0)).color(fg.alpha(if active { 1.0 } else { 0.75 })));
        if active {
            tile = tile.box_shadow(Shadow { y: 6.0, blur: 18.0, color: bg.alpha(0.7), ..Shadow::default() });
        }
        let status = match t.conn {
            _ if t.needs_sign_in.is_some() => Some(warning()),
            Conn::Live => None,
            Conn::Connecting => Some(warning()),
            Conn::Retrying(_) => Some(danger()),
        };
        let mut wrap = el().size(48.0, 48.0).center().child(tile);
        if unread > 0 {
            let label = if unread > 99 { "99+".to_string() } else { unread.to_string() };
            wrap = wrap.child(
                el().absolute(28.0, -3.0).h(18.0).min_w(18.0).px(5.0).radius(9.0).fill(danger()).center().border(2.0, canvas())
                    .child(text(label).font(tabular(ts(10.5, 700.0))).color(Color::WHITE)),
            );
        }
        if let Some(c) = status {
            wrap = wrap.child(el().absolute(36.0, 36.0).size(11.0, 11.0).radius(5.5).fill(c).border(2.0, canvas()));
        }
        let lead = if active { indicator(20.0) } else { el().w(3.0) };
        tiles = tiles.child(row().w(RAIL_W).items_center().child(lead).child(spacer()).child(wrap).child(spacer()).child(el().w(3.0)));
    }
    let me = s.account.as_ref().map(|a| (a.identity.oid.clone(), a.identity.name.clone())).unwrap_or_default();
    let account = el().key("account").radius(20.0).padding(3.0)
        .border(1.0, if s.show_menu { accent() } else { Color::TRANSPARENT })
        .hover(Color::WHITE.alpha(0.08))
        .press_scale(0.94)
        .on_press(|s: &mut App| s.show_menu = !s.show_menu).cursor(rune::Cursor::Pointer)
        .role("button").label("Account").help("Account")
        .child(avatar(&me.0, &me.1, 34.0, false));
    col().key("rail").w(RAIL_W).h_full().pt(16.0).pb(16.0).items_center()
        .child(tiles)
        .child(spacer())
        .child(account)
}

// ---------------------------------------------------------------- chat list

fn conn_label(t: &TenantState) -> (String, Option<Color>) {
    match &t.conn {
        Conn::Live => (String::new(), None),
        Conn::Connecting => ("Connecting…".into(), Some(warning())),
        Conn::Retrying(e) => (format!("Offline — {e}"), Some(danger())),
    }
}

fn chat_list(s: &App, t: &TenantState, k: &Kit) -> El {
    let q = s.search.text.trim().to_lowercase();
    let matches = |c: &Conversation| q.is_empty() || t.title(c).to_lowercase().contains(&q);
    let visible: Vec<&Conversation> = t.convs.iter().filter(|c| !c.hidden && c.kind != ConvKind::Other && matches(c)).collect();
    let pinned: Vec<&Conversation> = visible.iter().copied().filter(|c| c.pinned).collect();
    let chats: Vec<&Conversation> = visible.iter().copied().filter(|c| !c.pinned && c.kind != ConvKind::Channel).collect();
    let channels: Vec<&Conversation> = visible.iter().copied().filter(|c| !c.pinned && c.kind == ConvKind::Channel).collect();

    let mut list = col().key("chat-rows").w_full().pb(16.0);
    let section = |list: El, title: &str, convs: &[&Conversation]| -> El {
        if convs.is_empty() {
            return list;
        }
        let mut l = list.child(
            row().px(20.0).pt(16.0).pb(6.0).gap(8.0).items_center()
                .child(text(title.to_string()).font(ts(12.0, 650.0)).color(k.c.text_muted))
                .child(text(convs.len().to_string()).font(tabular(ts(11.0, 500.0))).color(k.c.text_faint)),
        );
        for c in convs.iter().take(400) {
            l = l.child(chat_row(t, c, k));
        }
        l
    };
    list = section(list, "Pinned", &pinned);
    list = section(list, "Chats", &chats);
    list = section(list, "Channels", &channels);
    if visible.is_empty() {
        let (head, sub) = if !q.is_empty() {
            ("No chats match", "Try part of a name or a group's topic.")
        } else if matches!(t.conn, Conn::Live) {
            ("No chats yet", "New conversations show up here as they arrive.")
        } else {
            ("Loading chats…", "Fetching the list from this organisation.")
        };
        list = list.child(
            col().px(20.0).pt(28.0).gap(4.0)
                .child(text(head).font(ts(14.0, 600.0)).color(k.c.text_base))
                .child(text(sub).font(ts(12.5, 400.0)).color(k.c.text_muted).wrap()),
        );
    }

    let (status, dot) = conn_label(t);
    let mut header = col().px(20.0).pt(22.0).pb(12.0).gap(12.0).child(
        row().items_center().gap(8.0)
            .child(text(t.tenant.name.clone()).key("tenant-name").font(display(24.0)).color(k.c.text_base).truncate().shrink())
            .child(spacer()),
    );
    if let Some(c) = dot {
        header = header.child(
            row().items_center().gap(8.0).px(10.0).py(5.0).radius(8.0).fill(c.alpha(0.1)).border(1.0, c.alpha(0.25)).self_start().max_w(LIST_W - 40.0).min_w(0.0)
                .child(el().size(7.0, 7.0).radius(3.5).fill(c))
                .child(text(status).font(ts(12.0, 550.0)).color(k.c.text_base).truncate().shrink()),
        );
    }
    if let Some(reason) = &t.needs_sign_in {
        let tid = t.tenant.id.clone();
        let waiting = reason.starts_with("Waiting");
        header = header.child(
            col().key("tenant-signin").padding(14.0).gap(8.0).radius(10.0).fill(warning().alpha(0.1)).border_gradient(1.0, warning().alpha(0.5), warning().alpha(0.2))
                .child(text(format!("{} needs you to sign in again (MFA or a policy of that organisation).", t.tenant.name)).font(ts(13.0, 550.0)).color(k.c.text_base).wrap())
                .child(text(reason.clone()).font(caption()).color(k.c.text_muted).max_lines(2).wrap())
                .child(
                    primary(Button::new(if waiting { "Waiting for sign-in…" } else { "Sign in to this organisation" })
                        .variant(ButtonVariant::Neutral)
                        .on_press(move |s: &mut App| s.sign_in_tenant(tid.clone()))
                        .build(k))
                        .key("tenant-signin-button")
                        .press_scale(0.97),
                ),
        );
    }
    header = header.child(
        TextInput::new(&s.search)
            .placeholder("Search chats   Ctrl+K")
            .on_edit(|s: &mut App| &mut s.search)
            .build(k)
            .key("search"),
    );
    acrylic(col(), hex("#161722").alpha(0.55)).key("chat-list").w(LIST_W).h_full().min_h(0.0).mt(8.0).corners([12.0, 0.0, 0.0, 0.0])
        .child(header)
        .child(list.scroll(ScrollSpec { stick_bottom: false }).grow(1.0).min_h(0.0).clip_fade(0.0, 8.0, 0.0, 16.0))
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
    let name_style = ts(14.0, if unread { 700.0 } else { 550.0 });
    let line2 = typing.clone().unwrap_or(preview);
    let ring = if selected { hex("#2a2c3c") } else { hex("#14151f") };
    let item = row().key(format!("chat-{}", c.id)).role("listitem").label(title.clone())
        .grow(1.0).min_w(0.0).px(10.0).py(9.0).gap(12.0).radius(8.0).items_center()
        .fill(if selected { Color::WHITE.alpha(0.085) } else { Color::TRANSPARENT })
        .hover(if selected { Color::WHITE.alpha(0.11) } else { Color::WHITE.alpha(0.05) })
        .press(Color::WHITE.alpha(0.03))
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(move |s: &mut App, tx| s.open(&id, tx))
        .child(with_presence(avatar(&av_id, &title, 38.0, square), t.presence.get(&c.peer), 38.0, ring))
        .child(
            col().grow(1.0).min_w(0.0).gap(3.0)
                .child(
                    row().items_center().gap(6.0)
                        .child(text(title).font(name_style).color(k.c.text_base).truncate().shrink().grow(1.0))
                        .child(text(when(c.last_time())).font(tabular(ts(11.5, if unread { 650.0 } else { 450.0 }))).color(if unread { accent() } else { k.c.text_faint })),
                )
                .child(
                    row().items_center().gap(8.0)
                        .child(text(line2).font(ts(12.5, if unread { 550.0 } else { 400.0 })).color(if typing.is_some() { accent() } else if unread { k.c.text_base } else { k.c.text_muted }).truncate().shrink().grow(1.0))
                        .child(if unread { el().size(8.0, 8.0).radius(4.0).fill(accent()).box_shadow(Shadow { blur: 8.0, color: accent().alpha(0.8), ..Shadow::default() }) } else { el() }),
                ),
        );
    row().mx(6.0).items_center().gap(3.0)
        .child(if selected { indicator(18.0) } else { el().w(3.0) })
        .child(item)
}

// ---------------------------------------------------------------- conversation

fn content_layer() -> El {
    col().key("conversation").grow(1.0).h_full().min_w(0.0).min_h(0.0).mt(8.0)
        .fill(layer())
        .border_sides([1.0, 0.0, 0.0, 1.0], stroke())
}

fn conversation(s: &App, t: &TenantState, k: &Kit) -> El {
    let Some(conv) = t.selected.as_ref().and_then(|id| t.conv(id)) else {
        let (head, sub) = if t.needs_sign_in.is_some() {
            (format!("Not connected to {}", t.tenant.name), "Sign in to this organisation from the panel on the left.".to_string())
        } else if t.convs.is_empty() {
            (format!("Signed in to {}", t.tenant.name), "Your chats will appear on the left in a moment.".to_string())
        } else {
            ("Pick a conversation".to_string(), format!("{} chats in {}. Alt+↑/↓ moves between them.", t.convs.iter().filter(|c| !c.hidden).count(), t.tenant.name))
        };
        return content_layer().child(
            col().key("empty").grow(1.0).w_full().center().gap(10.0)
                .child(
                    el().size(64.0, 64.0).radius(18.0).fill(brand_deep().alpha(0.35)).border(1.0, accent().alpha(0.3)).center().mb(8.0)
                        .child(text("💬").font(ts(28.0, 400.0))),
                )
                .child(text(head).font(display(22.0)).color(k.c.text_base))
                .child(text(sub).font(ts(13.5, 400.0)).color(k.c.text_muted).wrap().max_w(380.0)),
        );
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
    let presence = t.presence.get(&conv.peer).filter(|_| conv.kind == ConvKind::OneOnOne);
    let sub = match presence {
        Some(p) => row().gap(6.0).items_center()
            .child(el().size(8.0, 8.0).radius(4.0).fill(presence_color(p).unwrap_or(k.c.text_faint)))
            .child(text(presence_label(p)).font(ts(12.0, 500.0)).color(k.c.text_muted)),
        None => row().child(text(kind.to_string()).font(ts(12.0, 500.0)).color(k.c.text_muted)),
    };
    // Keyed per conversation: a switch swaps the header and messages as one unit.
    let header = row().key(format!("conv-header-{}", conv.id)).transition(swap_in()).h(68.0).px(24.0).gap(14.0).items_center().border_sides([0.0, 0.0, 1.0, 0.0], stroke())
        .child(avatar(&av_id, &title, 38.0, matches!(conv.kind, ConvKind::Channel | ConvKind::Meeting)))
        .child(
            col().gap(2.0).shrink().min_w(0.0)
                .child(text(title.clone()).key("conv-title").font(display(19.0)).color(k.c.text_base).truncate())
                .child(sub),
        )
        .child(spacer())
        .child(header_actions(s, k));

    let empty = Default::default();
    let th = t.threads.get(&conv.id).unwrap_or(&empty);
    let compose_lines = s.composer.text.lines().count().clamp(1, 8) as f32;
    let mut msgs = col().key(format!("message-rows-{}", conv.id)).transition(swap_in()).w_full().px(24.0).pt(12.0);
    if th.backward.is_some() {
        msgs = msgs.child(
            row().justify_center().pb(8.0).child(
                Button::new(if th.loading { "Loading…" } else { "Load earlier messages" })
                    .variant(ButtonVariant::Outline)
                    .on_press(|s: &mut App| s.load_older())
                    .build(k)
                    .key("older"),
            ),
        );
    } else if th.loading && th.list.is_empty() {
        msgs = msgs.child(skeleton());
    }
    let mut prev: Option<&Shown> = None;
    for m in th.list.iter().filter(|m| m.msg.is_chat_content() || m.msg.deleted) {
        let new_day = prev.is_none_or(|p| day_label(p.msg.time) != day_label(m.msg.time));
        if new_day {
            msgs = msgs.child(
                row().key(format!("day-{}", m.msg.id)).justify_center().py(14.0).child(
                    el().px(12.0).py(4.0).radius(10.0).fill(Color::WHITE.alpha(0.05)).border(1.0, stroke())
                        .child(text(day_label(m.msg.time)).font(ts(11.5, 600.0)).color(k.c.text_muted)),
                ),
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
    // Room under the floating composer so the newest message clears it.
    msgs = msgs.child(el().key("composer-clearance").h(124.0 + (compose_lines - 1.0) * 20.0));
    let typing = t.typing.get(&conv.id).map(|(n, _)| format!("{n} is typing…")).unwrap_or_default();

    let mode = if s.editing.is_some() {
        Some("Editing your message")
    } else if t.reply_root.is_some() {
        Some("Replying in the open thread")
    } else {
        None
    };
    let mut status_row = row().key("compose-status").h(18.0).px(6.0).gap(10.0).items_center()
        .child(text(typing).key("typing").font(ts(11.5, 500.0)).color(accent()));
    if let Some(m) = mode {
        status_row = status_row
            .child(indicator(12.0))
            .child(text(m).key("compose-mode").font(ts(11.5, 650.0)).color(k.c.text_base))
            .child(text("Cancel  Esc").key("compose-cancel").font(ts(11.5, 500.0)).color(k.c.text_muted).cursor(rune::Cursor::Pointer).on_press(|s: &mut App| s.cancel_compose_mode()));
    }
    let placeholder = match mode {
        Some(_) if s.editing.is_some() => "Edit the message, Enter to save",
        Some(_) => "Reply…  (Enter to send)",
        None => "Type a message  (Enter to send, Shift+Enter for a new line)",
    };
    let has_text = !s.composer.text.trim().is_empty();
    let field = elevation(acrylic(row(), hex("#202230").alpha(0.78)), 10.0, 36.0).key("composer-card").radius(12.0).px(6.0).py(6.0).gap(6.0).items_end()
        .focus_within_ring(2.0, 0.0, accent().alpha(0.85))
        .child(
            el().key("attach").size(36.0, 36.0).radius(8.0).center()
                .hover(Color::WHITE.alpha(0.08)).press(Color::WHITE.alpha(0.04)).press_scale(0.92).cursor(rune::Cursor::Pointer)
                .role("button").label("Attach").help("Send a file or image (or paste an image with Ctrl+V)")
                .on_press_tx(|s: &mut App, tx| s.attach(tx))
                .child(text("+").font(ts(22.0, 400.0)).color(k.c.text_muted)),
        )
        .child(
            // Rune's own field (not kit's TextArea) so it can take pasted images.
            rune::text_input(&s.composer, ts(14.0, 400.0).lh(20.0))
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
                .px(8.0)
                .py(8.0)
                .no_focus_ring()
                .max_h(200.0)
                .key("composer")
                .grow(1.0)
                .min_w(0.0),
        )
        .child({
            let (glyph, label) = if s.editing.is_some() { (&SAVE_ICON, "Save") } else { (&SEND_ICON, "Send") };
            let b = el().key("send").size(36.0, 36.0).radius(8.0).center()
                .role("button").label(label).help(format!("{label}  (Enter)"))
                .cursor(rune::Cursor::Pointer).press_scale(0.92)
                .on_press_tx(|s: &mut App, tx| s.send(tx))
                .child(rune::icon(glyph, 20.0).color(if has_text { Color::WHITE } else { k.c.text_faint }));
            if has_text { primary(b) } else { b.hover(Color::WHITE.alpha(0.08)) }
        });
    let composer = col().key("composer-wrap").pin(Some(20.0), None, Some(20.0), Some(18.0)).gap(6.0)
        .child(status_row)
        .child(field);
    let stage = col().grow(1.0).w_full().min_h(0.0)
        .child(msgs.scroll(ScrollSpec { stick_bottom: true }).key("messages").grow(1.0).min_h(0.0))
        .child(composer);
    content_layer()
        .child(header)
        .child(stage)
}

/// Shape-matched placeholder while the first page of messages loads.
fn skeleton() -> El {
    // (own message?, bubble line widths). Mirrors real rows: name/time line, then a bubble.
    const ROWS: [(bool, &[f32]); 6] = [
        (false, &[300.0, 180.0]),
        (true, &[220.0]),
        (false, &[380.0]),
        (false, &[260.0, 340.0, 120.0]),
        (true, &[300.0, 160.0]),
        (false, &[200.0]),
    ];
    let shade = Color::WHITE.alpha(0.07);
    let bar = |w: f32, h: f32| el().w(w).h(h).radius(h / 2.0).fill(shade);
    let mut c = col().key("skeleton").gap(18.0).pt(12.0).pb(24.0).inert().label("Loading messages");
    for (i, (mine, lines)) in ROWS.into_iter().enumerate() {
        let mut bubble = col().gap(8.0).px(14.0).py(12.0)
            .corners(if mine { [14.0, 4.0, 14.0, 14.0] } else { [4.0, 14.0, 14.0, 14.0] })
            .fill(if mine { hex("#33356a").alpha(0.35) } else { card().alpha(0.6) });
        for &w in lines {
            bubble = bubble.child(bar(w, 9.0));
        }
        let body = col().gap(6.0).child(bar(if mine { 44.0 } else { 96.0 + i as f32 * 9.0 }, 8.0)).child(bubble);
        let r = if mine {
            row().justify_end().child(body.items_end())
        } else {
            row().gap(12.0).items_start().child(el().size(36.0, 36.0).radius(18.0).fill(shade)).child(body)
        };
        // A slow breathing pulse, staggered top to bottom so it reads as a gentle wave.
        c = c.child(r.key(format!("sk-{i}")).looping(LoopSpec::opacity(1.4, i as f64 * 0.12, 0.45, 1.0)));
    }
    c
}

/// How a conversation's header and messages arrive on a switch: a quick fade with a 6 pt rise.
/// The outgoing chat leaves at once, so the two never overlap.
fn swap_in() -> Transition {
    Transition::asymmetric(Transition::offset(0.0, 6.0).combined(Transition::opacity()), Transition::opacity())
        .springs(SpringPair::new(Spring::new(0.16, 0.0), Spring::NONE))
}

/// Calls, meetings and anything else Teamlite doesn't draw itself open in Teams on the web.
fn header_actions(s: &App, k: &Kit) -> El {
    let mut r = row().gap(10.0).items_center();
    if s.call_link(false).is_some() {
        r = r.child(
            row().gap(2.0).padding(3.0).radius(10.0).fill(Color::WHITE.alpha(0.05)).border(1.0, stroke()).items_center()
                .child(icon_button("call", &CALL_ICON, "Audio call in Teams on the web", k.c.text_base).hover(Color::WHITE.alpha(0.08))
                    .on_press_tx(|s: &mut App, tx| { if let Some(u) = s.call_link(false) { s.open_link(&u, tx); } }))
                .child(el().w(1.0).h(16.0).fill(stroke()))
                .child(icon_button("video", &VIDEO_ICON, "Video call in Teams on the web", k.c.text_base).hover(Color::WHITE.alpha(0.08))
                    .on_press_tx(|s: &mut App, tx| { if let Some(u) = s.call_link(true) { s.open_link(&u, tx); } })),
        );
    }
    // Solid brand fill, no gradient.
    r.child(icon_button("open-web", &OPEN_EXTERNAL_ICON, "Open in Teams on the web", Color::WHITE)
        .size(38.0, 38.0).radius(10.0).fill(hex("#5b5fc7")).hover(hex("#6c71d8"))
        .on_press_tx(|s: &mut App, tx| { if let Some(u) = s.web_link() { s.open_link(&u, tx); } }))
}

/// Flyout above the account button, in Acrylic like Windows 11 flyouts.
fn account_menu(s: &App, k: &Kit) -> El {
    let who = s.account.as_ref().map(|a| (a.identity.oid.clone(), a.identity.name.clone(), a.identity.upn.clone())).unwrap_or_default();
    let n = s.tenants.len();
    elevation(acrylic(col(), hex("#202230").alpha(0.86)), 16.0, 48.0).key("account-menu").pin(Some(RAIL_W - 4.0), None, None, Some(14.0))
        .w(288.0).padding(16.0).gap(12.0).radius(12.0)
        .child(
            row().gap(12.0).items_center()
                .child(avatar(&who.0, &who.1, 44.0, false))
                .child(
                    col().gap(2.0).min_w(0.0).shrink()
                        .child(text(who.1).font(ts(15.0, 650.0)).color(k.c.text_base).truncate())
                        .child(text(who.2).font(caption()).color(k.c.text_muted).truncate()),
                ),
        )
        .child(el().h(1.0).w_full().fill(stroke()))
        .child(text(format!("{n} {}", if n == 1 { "organisation" } else { "organisations" })).font(caption()).color(k.c.text_muted))
        .child(
            row().gap(8.0)
                .child(Button::new("Sign in again").variant(ButtonVariant::Neutral).on_press(|s: &mut App| { s.show_menu = false; s.sign_in() }).build(k).key("reauth").grow(1.0))
                .child(Button::new("Sign out").variant(ButtonVariant::Danger).on_press(|s: &mut App| s.sign_out()).build(k).key("signout").grow(1.0)),
        )
}

fn presence_color(p: &str) -> Option<Color> {
    // Teams' presence colours.
    let hex = match p {
        "Available" | "AvailableIdle" => "#6bb700",
        "Busy" | "BusyIdle" | "DoNotDisturb" | "InACall" | "InAMeeting" | "Presenting" => "#d74654",
        "Away" | "BeRightBack" => "#f8b52f",
        "Offline" | "PresenceUnknown" => "#8a8886",
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

/// `ring` is the surface the avatar sits on, so the dot looks cut out of it.
fn with_presence(av: El, p: Option<&String>, size: f32, ring: Color) -> El {
    let Some(c) = p.and_then(|p| presence_color(p)) else { return av };
    el().size(size, size).child(av).child(el().absolute(size - 12.0, size - 12.0).size(13.0, 13.0).radius(6.5).fill(c).border(2.5, ring))
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
            Some(Picture::Failed) => text("🖼 Image couldn't load").font(ts(12.5, 450.0)).color(k.c.text_faint),
            _ => el().w(240.0).h(150.0).radius(8.0).fill(Color::WHITE.alpha(0.04)).border(1.0, stroke())
                .linear_gradient(std::f32::consts::FRAC_PI_2, [(0.0, Color::TRANSPARENT), (0.5, Color::WHITE.alpha(0.04)), (1.0, Color::TRANSPARENT)])
                .center().child(text("Loading image…").font(caption()).color(k.c.text_faint)),
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
            row().key(format!("react-{}-{emo}", m.msg.id)).gap(5.0).px(9.0).h(26.0).items_center().radius(13.0)
                .fill(if mine { accent().alpha(0.2) } else { card() })
                .border(1.0, if mine { accent().alpha(0.75) } else { stroke() })
                .hover(if mine { accent().alpha(0.3) } else { card().mix(Color::WHITE, 0.06) })
                .press_scale(0.94)
                .cursor(rune::Cursor::Pointer)
                .help(who.join(", "))
                .on_press(move |s: &mut App| s.react(&key, &id, &e))
                .child(text(emoji_for(emo)).font(ts(13.0, 400.0)))
                .child(text(users.len().to_string()).font(tabular(ts(12.0, 650.0))).color(if mine { hex("#c5c8ff") } else { k.c.text_base })),
        );
    }
    Some(r)
}

fn action_bar(s: &App, t: &TenantState, key: &str, m: &Shown, channel_root: bool, k: &Kit) -> El {
    let mut bar = elevation(row(), 6.0, 20.0).key("msg-actions").gap(2.0).items_center().padding(4.0).radius(10.0)
        .fill(hex("#272938")).border_gradient(1.0, Color::WHITE.alpha(0.12), Color::WHITE.alpha(0.04));
    for (emo, glyph) in REACTIONS {
        let (key, id) = (key.to_string(), m.msg.id.clone());
        bar = bar.child(
            el().key(format!("add-{emo}")).size(32.0, 30.0).radius(7.0).center().hover(Color::WHITE.alpha(0.08)).press_scale(0.88).cursor(rune::Cursor::Pointer)
                .help(emo)
                .on_press(move |s: &mut App| s.react(&key, &id, emo))
                .child(text(glyph).font(ts(16.0, 400.0))),
        );
    }
    bar = bar.child(el().w(1.0).h(18.0).mx(4.0).fill(stroke()));
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
    let mut c = col().key("replies").ml(56.0).mt(4.0).mb(8.0).pl(14.0).py(4.0).gap(2.0).border_sides([0.0, 0.0, 0.0, 2.0], accent().alpha(0.6));
    let empty = |c: El| c.child(text("No replies yet. Be the first.").font(ts(12.5, 450.0)).color(k.c.text_faint).py(6.0));
    match t.threads.get(&key) {
        Some(th) if !th.list.is_empty() => {
            for m in th.list.iter().filter(|m| m.msg.id != root && (m.msg.is_chat_content() || m.msg.deleted)) {
                c = c.child(message(s, t, &key, m, false, false, k));
            }
            if th.list.iter().all(|m| m.msg.id == root) {
                c = empty(c);
            }
        }
        Some(th) if th.loading => c = c.child(text("Loading replies…").font(ts(12.5, 450.0)).color(k.c.text_faint).py(6.0)),
        _ => c = empty(c),
    }
    c
}

/// Who wrote a quoted message. The embedded name can be the placeholder "Display Name", so prefer
/// the profile for the author MRI, then the quoted message itself if it is loaded.
fn quote_author(t: &TenantState, key: &str, q: &crate::api::html::Quote) -> Option<String> {
    if let Some(mri) = &q.author_mri {
        if *mri == t.me {
            return Some("You".into());
        }
        if let Some(n) = t.names.get(mri) {
            return Some(n.clone());
        }
    }
    let conv = key.split(";messageid=").next().unwrap_or(key);
    let original = q.message_id.as_ref().and_then(|id| {
        t.threads.iter().filter(|(k, _)| k.starts_with(conv)).find_map(|(_, th)| th.list.iter().find(|m| &m.msg.id == id))
    });
    if let Some(o) = original {
        if o.msg.from == t.me {
            return Some("You".into());
        }
        let n = if o.msg.from_name.is_empty() { t.names.get(&o.msg.from).cloned().unwrap_or_default() } else { o.msg.from_name.clone() };
        if !n.is_empty() {
            return Some(n);
        }
    }
    Some(q.author.clone()).filter(|a| !a.is_empty() && !a.eq_ignore_ascii_case("display name"))
}

fn message(s: &App, t: &TenantState, key: &str, m: &Shown, grouped: bool, channel_root: bool, k: &Kit) -> El {
    let mine = m.msg.from == t.me;
    // Channel posts read as a feed, chats as a conversation: own messages on the right.
    let right = mine && !channel_root && !key.contains(";messageid=");
    let name = if mine {
        "You".to_string()
    } else if !m.msg.from_name.is_empty() {
        m.msg.from_name.clone()
    } else {
        t.names.get(&m.msg.from).cloned().unwrap_or_else(|| "Unknown".into())
    };
    let selected = s.selected_msg.as_deref() == Some(m.msg.id.as_str()) && !m.msg.id.starts_with("pending-");
    let (bubble_fill, bubble_hover) = if right {
        (hex("#33356a"), hex("#3b3e7a"))
    } else {
        (card(), card().mix(Color::WHITE, 0.04))
    };
    // The corner toward the sender stays tight, like Teams.
    let corners = match (right, grouped) {
        (true, false) => [14.0, 4.0, 14.0, 14.0],
        (false, false) => [4.0, 14.0, 14.0, 14.0],
        _ => [14.0, 14.0, 14.0, 14.0],
    };
    let mut bubble = col().gap(6.0).max_w(620.0).shrink().min_w(0.0).px(14.0).py(10.0).corners(corners)
        .fill(bubble_fill)
        .border(1.0, if selected { accent().alpha(0.8) } else if right { accent().alpha(0.22) } else { stroke() })
        .hover(bubble_hover);
    if m.msg.deleted {
        bubble = bubble.fill(Color::TRANSPARENT).border(1.0, stroke()).border_dash(4.0, 3.0)
            .child(text("This message has been deleted.").font(ts(13.5, 400.0).italic()).color(k.c.text_faint));
    } else {
        if let Some(q) = &m.rendered.quoted {
            let mut quote = col().gap(2.0).min_w(0.0).shrink();
            if let Some(author) = quote_author(t, key, q) {
                quote = quote.child(text(author).font(ts(12.0, 600.0)).color(k.c.text_base).truncate());
            }
            bubble = bubble.child(
                el().min_w(0.0).shrink().pl(10.0).pr(8.0).py(6.0).radius(6.0).fill(Color::BLACK.alpha(0.18)).border_sides([0.0, 0.0, 0.0, 3.0], accent().alpha(0.7))
                    .child(quote.child(text(q.text.clone()).font(ts(12.5, 400.0)).color(k.c.text_muted).max_lines(2).min_w(0.0).shrink())),
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
        let ext = name.rsplit('.').next().filter(|e| e.len() <= 4 && *e != name.as_str()).unwrap_or("file").to_uppercase();
        file_chips = file_chips.child(
            row().key(format!("file-{}", name)).gap(12.0).pl(8.0).pr(14.0).py(8.0).radius(10.0).items_center().w(320.0)
                .fill(card()).border(1.0, stroke()).hover(card().mix(Color::WHITE, 0.06)).press_scale(0.98).cursor(rune::Cursor::Pointer)
                .help("Open in the browser")
                .on_press_tx(move |s: &mut App, tx| s.open_link(&url, tx))
                .child(el().size(36.0, 40.0).radius(6.0).fill(brand_deep().alpha(0.55)).border(1.0, accent().alpha(0.3)).center().child(text(ext).font(ts(9.5, 750.0).tracking(0.4)).color(hex("#c5c8ff"))))
                .child(col().min_w(0.0).shrink().grow(1.0).gap(1.0).child(text(name.clone()).font(ts(13.5, 600.0)).color(k.c.text_base).truncate()).child(text(kb).font(tabular(caption())).color(k.c.text_faint)))
                .child(text("↗").font(ts(14.0, 500.0)).color(k.c.text_muted)),
        );
    }
    let mut meta = row().gap(8.0).items_center().px(4.0);
    if !grouped {
        if !right {
            meta = meta.child(text(name.clone()).font(ts(12.5, 650.0)).color(k.c.text_base));
        }
        meta = meta.child(text(when_exact(m.msg.time)).font(tabular(caption())).color(k.c.text_faint));
    }
    if m.msg.edited && !m.msg.deleted {
        meta = meta.child(text("Edited").font(caption()).color(k.c.text_faint));
    }
    if m.pending {
        meta = meta.child(text("Sending…").font(caption()).color(k.c.text_faint));
    }
    if m.failed {
        meta = meta.child(text("Not sent").font(ts(12.0, 650.0)).color(danger()));
    }
    // Only the message itself selects; reactions and actions sit beside it, not inside it, so
    // their clicks don't reach the selection handler.
    let id = m.msg.id.clone();
    let align = |e: El| if right { e.items_end() } else { e.items_start() };
    let body = align(col()).key("msg-body").gap(4.0).min_w(0.0).cursor(rune::Cursor::Pointer)
        .on_press(move |s: &mut App| s.select_message(&id))
        .child(meta)
        .child(bubble);
    let mut content = align(col()).gap(6.0).grow(1.0).min_w(0.0).child(body);
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
            text(if open { "Hide replies" } else { "Replies ›" }).key(format!("replies-{}", m.msg.id)).font(ts(12.5, 650.0)).color(accent())
                .px(4.0).cursor(rune::Cursor::Pointer).on_press(move |s: &mut App| s.toggle_replies(&id)),
        );
    }
    if selected {
        content = content.child(action_bar(s, t, key, m, channel_root, k));
    }
    let row_ = row().key(format!("msg-{}", if m.msg.id.is_empty() { &m.msg.client_message_id } else { &m.msg.id }))
        .gap(12.0).pt(if grouped { 2.0 } else { 10.0 }).pb(2.0)
        .opacity(if m.pending { 0.6 } else { 1.0 });
    if right {
        row_.pl(96.0).child(content)
    } else {
        let left: El = if grouped { el().w(36.0) } else { avatar(&m.msg.from, &name, 36.0, false).mt(20.0) };
        row_.pr(96.0).items_start().child(left).child(content)
    }
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
            sp.bg = Some(Color::BLACK.alpha(0.3));
            sp.bg_pad = 2.0;
            styled = true;
        }
        if r.mention {
            sp = sp.color(hex("#aab0ff")).weight(650.0);
            styled = true;
        }
        if let Some(l) = &r.link
            && !l.is_empty()
        {
            sp = sp.color(hex("#aab0ff"));
            sp.underline = Some(hex("#aab0ff").alpha(0.55));
            styled = true;
            links.push(Link::new(start..end, l.clone()));
        }
        if styled {
            spans.push(sp);
        }
    }
    if s.is_empty() {
        s = if m.rendered.has_image { "🖼 Image".into() } else { " ".into() };
    }
    let mut body = rich(s, spans).font(ts(14.0, 400.0).lh(21.0)).color(k.c.text_base).wrap().selectable(true);
    for l in links {
        body = body.link(l);
    }
    body.on_link(|_: &mut App, url: &str, tx| {
        let _ = tx.open_url(url);
    })
}
