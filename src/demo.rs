//! Headless scenes with made-up data, for checking the UI without a network:
//! `teamlite --snap out.png --scene demo`.

use crate::api::auth::Identity;
use crate::api::now_ms;
use crate::api::teams::{ConvKind, Conversation, Message, Tenant};
use crate::app::{App, Phase, Shown, TenantState, Thread};
use crate::store::{Account, TenantCache};
use crate::worker::Conn;

pub const SCENES: &[(&str, fn(&mut App))] = &[("signin", |_| {}), ("demo", demo), ("offline", offline), ("search", search), ("mfa", |s| { demo(s); s.active = 2; }), ("channel", |s| { demo(s); s.tenants[0].selected = Some("19:gen@thread.tacv2".into()); s.tenants[0].reply_root = Some("100".into()); })];

const ME: &str = "8:orgid:me";

fn msg(id: &str, conv: &str, from: &str, name: &str, html: &str, mins_ago: i64) -> Message {
    Message {
        id: id.into(),
        conversation_id: conv.into(),
        client_message_id: format!("c{id}"),
        from: from.into(),
        from_name: name.into(),
        html: html.into(),
        message_type: "RichText/Html".into(),
        time: now_ms() - mins_ago * 60_000,
        ..Default::default()
    }
}

fn conv(id: &str, kind: ConvKind, topic: &str, peer: &str, last: Message, unread: bool) -> Conversation {
    Conversation {
        id: id.into(),
        kind,
        topic: topic.into(),
        team: if topic == "General" { "Platform".into() } else { String::new() },
        read_horizon: if unread { 0 } else { last.time },
        last: Some(last),
        peer: peer.into(),
        ..Default::default()
    }
}

fn demo(s: &mut App) {
    let home = Tenant { id: "t-home".into(), name: "Contoso".into(), user_id: "me".into(), is_home: true };
    let guest = Tenant { id: "t-guest".into(), name: "Fabrikam (Guest)".into(), user_id: "me2".into(), is_home: false };
    let third = Tenant { id: "t-third".into(), name: "Northwind".into(), user_id: "me3".into(), is_home: false };
    let a = "19:me_ann@unq.gbl.spaces";
    let thread = vec![
        msg("1", a, "8:orgid:ann", "Ann Rossi", "<p>Morning! Did you see the deploy report?</p>", 300),
        msg("2", a, ME, "Me", "<p>Yes, looks good. The <b>EU cluster</b> took a bit longer.</p>", 296),
        msg("3", a, "8:orgid:ann", "Ann Rossi", "<p>Ok. Can you check <a href=\"https://example.com/runs/42\">run 42</a> when you have a minute?</p>", 40),
        msg("4", a, "8:orgid:ann", "Ann Rossi", "<p>It fails on <code>cargo test -p api</code> 🤔</p><ul><li>timeout in auth</li><li>flaky poll test</li></ul>", 39),
        msg("5", a, ME, "Me", "<p>On it.</p>", 2),
    ];
    let mut thread = thread;
    thread[1].files = vec![("deploy-report.pdf".into(), "https://example.com/r.pdf".into(), 248_000)];
    thread[2].reactions = vec![("like".into(), vec!["8:orgid:ann".into(), ME.into()]), ("laugh".into(), vec!["8:orgid:bob".into()])];
    thread[3].html.push_str(r#"<img itemtype="http://schema.skype.com/AMSImage" src="https://eu-api.asm.skype.com/v1/objects/0-demo/views/imgo">"#);
    let convs = vec![
        conv(a, ConvKind::OneOnOne, "", "8:orgid:ann", thread.last().unwrap().clone(), false),
        conv("19:grp@thread.v2", ConvKind::Group, "Release crew", "", msg("g", "19:grp@thread.v2", "8:orgid:luca", "Luca Bianchi", "<p>Shipping at 5, objections?</p>", 12), true),
        conv("19:gen@thread.tacv2", ConvKind::Channel, "General", "", msg("c", "19:gen@thread.tacv2", "8:orgid:sara", "Sara Conti", "<p>Welcome to the team, Marco!</p>", 60 * 26), false),
        conv("19:me_bob@unq.gbl.spaces", ConvKind::OneOnOne, "", "8:orgid:bob", msg("b", "19:me_bob@unq.gbl.spaces", "8:orgid:bob", "Bob Smith", "<p>Thanks!</p>", 60 * 24 * 3), false),
        conv("19:meet@thread.v2", ConvKind::Meeting, "Weekly sync", "", msg("m", "19:meet@thread.v2", "8:orgid:ann", "Ann Rossi", "<p>Notes are in the wiki.</p>", 60 * 24 * 9), false),
    ];
    let mut pinned = convs;
    pinned[0].pinned = true;
    let names = [("8:orgid:ann", "Ann Rossi"), ("8:orgid:bob", "Bob Smith"), ("8:orgid:luca", "Luca Bianchi")]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let mut t = TenantState::from_cache(home.clone(), TenantCache { conversations: pinned, messages: Default::default(), names });
    t.me = ME.into();
    t.conn = Conn::Live;
    t.threads.insert(a.into(), Thread { version: 0, list: thread.into_iter().map(Shown::new).collect(), backward: Some("x".into()), loading: false, loaded: true });
    t.selected = Some(a.into());
    t.presence.insert("8:orgid:ann".into(), "Available".into());
    t.emails.insert("8:orgid:ann".into(), "ann@contoso.com".into());
    t.presence.insert("8:orgid:bob".into(), "Busy".into());
    let ch = "19:gen@thread.tacv2";
    let mut root = msg("100", ch, "8:orgid:sara", "Sara Conti", "<p>Welcome to the team, Marco!</p>", 60 * 26);
    root.root = "100".into();
    let mut reply = msg("101", ch, ME, "Me", "<p>Welcome aboard 🎉</p>", 60 * 25);
    reply.root = "100".into();
    t.threads.insert(ch.into(), Thread { version: 0, list: vec![Shown::new(root.clone())], backward: None, loading: false, loaded: true });
    t.threads.insert(crate::app::chain_key(ch, "100"), Thread { version: 0, list: vec![Shown::new(root), Shown::new(reply)], backward: None, loading: false, loaded: true });
    t.typing.insert("19:grp@thread.v2".into(), ("Luca Bianchi".into(), 1e9));
    let mut g = TenantState::from_cache(guest.clone(), TenantCache {
        conversations: vec![conv("19:x@thread.v2", ConvKind::Group, "Vendor onboarding", "", msg("x", "19:x@thread.v2", "8:orgid:z", "Zoe", "<p>Docs attached</p>", 5), true)],
        ..Default::default()
    });
    g.conn = Conn::Live;
    let mut n = TenantState::from_cache(third.clone(), TenantCache::default());
    n.conn = Conn::Retrying("invalid_grant: AADSTS50076: multi-factor authentication required".into());
    n.needs_sign_in = Some("AADSTS50076: you must use multi-factor authentication".into());
    s.tenants = vec![t, g, n];
    s.active = 0;
    s.account = Some(Account {
        identity: Identity { oid: "me".into(), tid: "t-home".into(), name: "Ruggero C".into(), upn: "r@contoso.com".into() },
        tenants: vec![home, guest, third],
        active_tenant: "t-home".into(),
        ..Default::default()
    });
    s.phase = Phase::Ready;
}

fn offline(s: &mut App) {
    demo(s);
    s.tenants[0].conn = Conn::Retrying("authz: HTTP 503".into());
}

fn search(s: &mut App) {
    demo(s);
    s.search.replace_text("ro");
}
