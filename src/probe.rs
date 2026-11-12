//! `teamlite --probe [tenant-id]`: checks the saved session against the live services and prints
//! response *shapes* (keys, counts, sync metadata), never message content.

use serde_json::Value;

use crate::api::{auth, http, teams};
use crate::store;

fn shape(v: &Value) -> String {
    match v {
        Value::Object(m) => format!("{{{}}}", m.keys().cloned().collect::<Vec<_>>().join(",")),
        Value::Array(a) => format!("[{} items]", a.len()),
        Value::String(s) => format!("str({})", s.len()),
        other => other.to_string(),
    }
}

pub fn run(tenant: Option<&str>) -> anyhow::Result<()> {
    let mut acc = store::load_account().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let tid = tenant.map(str::to_string).unwrap_or_else(|| acc.identity.tid.clone());
    let rt = acc.refresh_for(&tid).ok_or_else(|| anyhow::anyhow!("no refresh token"))?.to_string();
    let t = auth::refresh(&tid, &rt, auth::SKYPE_SCOPE)?;
    acc.refresh.insert(tid.clone(), t.refresh_token.clone());
    store::save_account(&acc);
    let who = auth::identity(&t.access_token)?;
    let sk = teams::authz(&t.access_token)?;
    println!("tenant {tid} region {} chat {}", sk.region, sk.chat_service);
    let get = |u: &str| -> anyhow::Result<(u16, Value)> {
        let r = http().get(u).header("Authentication", format!("skypetoken={}", sk.skype_token)).send()?;
        let st = r.status().as_u16();
        Ok((st, r.json().unwrap_or(Value::Null)))
    };
    let (st, v) = get(&format!("{}/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=5&startTime=1", sk.chat_service))?;
    println!("conversations: HTTP {st} top={} meta={}", shape(&v), shape(&v["_metadata"]));
    for (k, val) in v["_metadata"].as_object().into_iter().flatten() {
        let s = val.as_str().map(|s| s.split('?').next().unwrap_or("").to_string() + &format!(" (len {})", s.len())).unwrap_or_else(|| val.to_string());
        println!("  _metadata.{k} = {s}");
    }
    if let Some(c) = v["conversations"].get(0) {
        println!("  conv keys {} props {} tp {}", shape(c), shape(&c["properties"]), shape(&c["threadProperties"]));
    }
    // Full sync, then deltas from the last page's syncState.
    let mut url = format!("{}/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1", sk.chat_service);
    let mut total = 0;
    let mut sync = String::new();
    let mut fav_props = std::collections::BTreeSet::new();
    for page in 0..10 {
        let (st, v) = get(&url)?;
        let n = v["conversations"].as_array().map(|a| a.len()).unwrap_or(0);
        total += n;
        for c in v["conversations"].as_array().into_iter().flatten() {
            for k in c["properties"].as_object().into_iter().flatten().map(|(k, _)| k.clone()) { fav_props.insert(k); }
        }
        if page == 0 { sync = v["_metadata"]["syncState"].as_str().unwrap_or("").to_string(); }
        println!("page {page}: HTTP {st} {n} convs");
        match v["_metadata"]["backwardLink"].as_str().filter(|s| !s.is_empty()) { Some(b) => url = b.to_string(), None => { break } }
    }
    println!("total {total}; conversation property keys seen: {:?}", fav_props);
    let (_, v) = get(&format!("{}/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1", sk.chat_service))?;
    for c in v["conversations"].as_array().into_iter().flatten() {
        let mi = &c["properties"]["meetingInfo"];
        if !mi.is_null() {
            let parsed = mi.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(mi.clone());
            println!("meetingInfo {} ; tp keys {}", shape(&parsed), shape(&c["threadProperties"]));
            for (k, x) in parsed.as_object().into_iter().flatten() { if k.to_lowercase().contains("url") || k.to_lowercase().contains("link") { println!("  {k}: {}…", x.as_str().unwrap_or("").chars().take(48).collect::<String>()); } }
            break;
        }
    }
    let (st, d) = get(&sync)?;
    println!("delta from first-page syncState: HTTP {st} {} items", d["conversations"].as_array().map(|a| a.len()).unwrap_or(0));
    let s2 = d["_metadata"]["syncState"].as_str().unwrap_or("").to_string();
    let (st, d2) = get(&s2)?;
    println!("delta again: HTTP {st} {} items", d2["conversations"].as_array().map(|a| a.len()).unwrap_or(0));
    let (st, v) = get(&format!("{}/v1/users/ME/properties", sk.chat_service))?;
    println!("user properties: HTTP {st} {}", shape(&v));
    let fav = &v["favorites"];
    println!("teamsOrder {}", shape(&v["teamsOrder"]));
    if let Some(s) = fav.as_str() { if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(s) { for (k, x) in m.iter().take(3) { println!("  fav {}… -> {}", &k[..k.len().min(12)], shape(x)); } } }
    println!("favorites: {} parsed={}", shape(fav), fav.as_str().map(|s| shape(&serde_json::from_str::<Value>(s).unwrap_or(Value::Null))).unwrap_or_default());
    if let Some(s) = fav.as_str() && let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
        if let Some(first) = a.first() { println!("  favorites[0] shape {}", shape(first)); for (k, x) in first.as_object().into_iter().flatten() { println!("    {k}: {}", x.as_str().map(|s| s.chars().take(14).collect::<String>() + "…").unwrap_or_else(|| x.to_string())); } }
    }
    println!("me oid {}", who.oid);
    Ok(())
}

/// `teamlite --probe-msgs <tenant> <conv-id-prefix>`: message property keys, emotion shape,
/// reply-chain links and image hosts (no text content).
pub fn messages(tenant: &str, conv_prefix: &str) -> anyhow::Result<()> {
    let mut acc = store::load_account().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let rt = acc.refresh_for(tenant).ok_or_else(|| anyhow::anyhow!("no refresh token"))?.to_string();
    let t = auth::refresh(tenant, &rt, auth::SKYPE_SCOPE)?;
    acc.refresh.insert(tenant.to_string(), t.refresh_token.clone());
    store::save_account(&acc);
    let sk = teams::authz(&t.access_token)?;
    let get = |u: &str| -> anyhow::Result<Value> {
        Ok(http().get(u).header("Authentication", format!("skypetoken={}", sk.skype_token)).send()?.json().unwrap_or(Value::Null))
    };
    let v = get(&format!("{}/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1", sk.chat_service))?;
    let convs = v["conversations"].as_array().cloned().unwrap_or_default();
    if conv_prefix == "*" {
        // Find a message with reactions anywhere in the 30 most recent chats.
        for c in convs.iter().take(30) {
            let id = c["id"].as_str().unwrap_or("");
            let m = get(&format!("{}/v1/users/ME/conversations/{}/messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=50&startTime=1", sk.chat_service, teams::urlenc(id)))?;
            for msg in m["messages"].as_array().into_iter().flatten() {
                if std::env::var_os("PROBE_FILES").is_some() {
                    if let Some(f) = msg["properties"]["files"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).and_then(|v| v.get(0).cloned()) {
                        println!("files[0] keys {}", shape(&f));
                        for k in ["@type", "version", "type", "state", "permissionScope", "fileType"] { println!("  {k} = {}", f[k]); }
                        println!("  fileInfo {} chiclet {} preview {}", shape(&f["fileInfo"]), f["fileChicletState"], shape(&f["filePreview"]));
                        println!("  id looks like guid: {}", f["id"].as_str().map(|x| x.len() == 36).unwrap_or(false));
                        println!("  content: {}", msg["content"].as_str().map(|c| if c.is_empty() { "empty" } else { "non-empty" }).unwrap_or("none"));
                        return Ok(());
                    }
                    continue;
                }
                let e = &msg["properties"]["emotions"];
                if !e.is_null() {
                    let parsed = e.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(e.clone());
                    println!("emotions raw type {} parsed {}", if e.is_string() { "string" } else { "json" }, shape(&parsed));
                    if let Some(f) = parsed.get(0) { println!("  first {} key {:?} users[0] {} user keys {}", shape(f), f["key"], shape(&f["users"]), shape(&f["users"][0])); println!("  user value example {:?}", f["users"][0]["value"]); }
                    return Ok(());
                }
            }
        }
        println!("no reactions found");
        return Ok(());
    }
    let conv = convs.iter().filter_map(|c| c["id"].as_str()).find(|id| id.starts_with(conv_prefix)).unwrap_or(conv_prefix).to_string();
    println!("conv {conv}");
    let m = get(&format!("{}/v1/users/ME/conversations/{}/messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=50&startTime=1", sk.chat_service, teams::urlenc(&conv)))?;
    let mut keys = std::collections::BTreeSet::new();
    let mut props = std::collections::BTreeSet::new();
    for msg in m["messages"].as_array().into_iter().flatten() {
        keys.extend(msg.as_object().into_iter().flatten().map(|(k, _)| k.clone()));
        props.extend(msg["properties"].as_object().into_iter().flatten().map(|(k, _)| k.clone()));
        let e = &msg["properties"]["emotions"];
        if !e.is_null() {
            let parsed = e.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(e.clone());
            if let Some(first) = parsed.get(0) {
                println!("  emotions: {} first {} users[0] {}", shape(&parsed), shape(first), shape(&first["users"][0]));
                println!("    key example {:?}", first["key"]);
            }
        }
        if let Some(f) = msg["properties"]["files"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).and_then(|v| v.get(0).cloned()) {
            println!("  files[0] keys {}", shape(&f));
            for k in ["@type", "version", "type", "state", "permissionScope"] { println!("    {k} = {}", f[k]); }
            println!("    fileInfo {} chiclet {}", shape(&f["fileInfo"]), f["fileChicletState"]);
            println!("    content snippet tags: {}", msg["content"].as_str().unwrap_or("").chars().filter(|c| *c == '<').count());
        }
        let link = msg["conversationLink"].as_str().unwrap_or("");
        if link.contains(";messageid=") {
            println!("  msg {} root {}", msg["id"], link.rsplit("messageid=").next().unwrap_or(""));
        }
        let content = msg["content"].as_str().unwrap_or("");
        for part in content.split("src=\"").skip(1) {
            let src = part.split('"').next().unwrap_or("");
            let host = src.split('/').take(3).collect::<Vec<_>>().join("/");
            let path: String = src.split('/').skip(3).take(2).collect::<Vec<_>>().join("/");
            println!("  img host {host} path /{path}/… msg {} at {} from-me-name {}", msg["id"], msg["originalarrivaltime"], msg["imdisplayname"].as_str().map(|n| !n.is_empty()).unwrap_or(false));
        }
        if msg["messagetype"].as_str().is_some_and(|t| t.starts_with("RichText/Media")) {
            println!("  media message type {}", msg["messagetype"]);
        }
    }
    println!("  message keys {keys:?}");
    println!("  property keys {props:?}");
    Ok(())
}

/// `teamlite --probe-presence <tenant>`: can we read presence, and in what shape.
pub fn presence(tenant: &str) -> anyhow::Result<()> {
    let acc = store::load_account().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let rt = acc.refresh_for(tenant).ok_or_else(|| anyhow::anyhow!("no refresh token"))?.to_string();
    let t = auth::refresh(tenant, &rt, "https://presence.teams.microsoft.com/.default")?;
    let who = auth::identity(&t.access_token)?;
    let body = serde_json::json!([{ "mri": format!("8:orgid:{}", who.oid) }]);
    let r = http().post("https://presence.teams.microsoft.com/v1/presence/getpresence/").bearer_auth(&t.access_token).json(&body).send()?;
    let st = r.status();
    let v: Value = r.json().unwrap_or(Value::Null);
    println!("getpresence HTTP {st} {}", shape(&v));
    if let Some(p) = v.get(0) {
        println!("  item {} presence {}", shape(p), shape(&p["presence"]));
        println!("  availability {:?} activity {:?}", p["presence"]["availability"], p["presence"]["activity"]);
    }
    Ok(())
}

/// `teamlite --probe-delete <tenant> <conv> <message-id>`: tries delete variants on one of the
/// user's own messages and shows what the server keeps.
pub fn delete(tenant: &str, conv: &str, id: &str) -> anyhow::Result<()> {
    let mut acc = store::load_account().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let rt = acc.refresh_for(tenant).ok_or_else(|| anyhow::anyhow!("no refresh token"))?.to_string();
    let t = auth::refresh(tenant, &rt, auth::SKYPE_SCOPE)?;
    acc.refresh.insert(tenant.to_string(), t.refresh_token.clone());
    store::save_account(&acc);
    let sk = teams::authz(&t.access_token)?;
    let base = format!("{}/v1/users/ME/conversations/{}/messages/{}", sk.chat_service, teams::urlenc(conv), id);
    let show = |label: &str| -> anyhow::Result<()> {
        let v: Value = http().get(format!("{base}?view=msnp24Equivalent|supportsMessageProperties")).header("Authentication", format!("skypetoken={}", sk.skype_token)).send()?.json().unwrap_or(Value::Null);
        println!("{label}: content len {} props {}", v["content"].as_str().map(|s| s.len()).unwrap_or(0), shape(&v["properties"]));
        Ok(())
    };
    show("before")?;
    for (label, url) in [("softDelete", format!("{base}?behavior=softDelete")), ("plain", base.clone())] {
        let r = http().delete(&url).header("Authentication", format!("skypetoken={}", sk.skype_token)).send()?;
        println!("DELETE {label}: HTTP {} {}", r.status(), r.text().unwrap_or_default().chars().take(160).collect::<String>());
        show("after")?;
    }
    Ok(())
}

/// `teamlite --probe-scopes <tenant>`: which delegated scopes the Teams client gets for Graph
/// and SharePoint (claims only, no tokens printed).
pub fn scopes(tenant: &str) -> anyhow::Result<()> {
    let acc = store::load_account().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let rt = acc.refresh_for(tenant).ok_or_else(|| anyhow::anyhow!("no refresh token"))?.to_string();
    for res in ["https://graph.microsoft.com/.default"] {
        match auth::refresh(tenant, &rt, res) {
            Ok(t) => {
                let c = auth::claims(&t.access_token)?;
                let scp = c["scp"].as_str().unwrap_or("");
                let has = |s: &str| scp.split(' ').any(|x| x == s);
                println!("{res}: Files.ReadWrite.All {} Files.ReadWrite {} Sites.ReadWrite.All {} Chat.ReadWrite {}", has("Files.ReadWrite.All"), has("Files.ReadWrite"), has("Sites.ReadWrite.All"), has("Chat.ReadWrite"));
            }
            Err(e) => println!("{res}: {e}"),
        }
    }
    if let Ok(g) = auth::refresh(tenant, &rt, "https://graph.microsoft.com/.default") {
        for path in ["me", "me/drive"] {
            let r = http().get(format!("https://graph.microsoft.com/v1.0/{path}?$select=id")).bearer_auth(&g.access_token).send()?;
            println!("graph GET /{path}: HTTP {}", r.status());
        }
    }
    let g = auth::refresh(tenant, &rt, "https://graph.microsoft.com/.default")?;
    let v: Value = http().get("https://graph.microsoft.com/v1.0/me?$select=mySite").bearer_auth(&g.access_token).send()?.json()?;
    let mut site = v["mySite"].as_str().unwrap_or("").trim_end_matches('/').to_string();
    if site.is_empty() {
        let o: Value = http().get("https://graph.microsoft.com/v1.0/organization?$select=verifiedDomains").bearer_auth(&g.access_token).send()?.json()?;
        let initial = o["value"][0]["verifiedDomains"].as_array().into_iter().flatten().find(|d| d["isInitial"].as_bool() == Some(true)).and_then(|d| d["name"].as_str()).unwrap_or("").to_string();
        let me: Value = http().get("https://graph.microsoft.com/v1.0/me?$select=userPrincipalName").bearer_auth(&g.access_token).send()?.json()?;
        let upn = me["userPrincipalName"].as_str().unwrap_or("");
        let prefix = initial.split('.').next().unwrap_or("");
        println!("initial domain found {} upn found {}", !initial.is_empty(), !upn.is_empty());
        site = format!("https://{prefix}-my.sharepoint.com/personal/{}", upn.replace(['.', '@'], "_").to_lowercase());
    }
    let host = site.split('/').take(3).collect::<Vec<_>>().join("/");
    println!("personalFileSite host {host} (path segments {})", site.split('/').count().saturating_sub(3));
    if !host.is_empty() {
        match auth::refresh(tenant, &rt, &format!("{host}/.default")) {
            Ok(t) => {
                let r = http().get(format!("{site}/_api/web/title")).bearer_auth(&t.access_token).header("Accept", "application/json;odata=nometadata").send()?;
                println!("sharepoint GET web: HTTP {}", r.status());
                for h in ["x-ms-diagnostics", "www-authenticate", "sprequestguid"] {
                    if let Some(v) = r.headers().get(h) { println!("  {h}: {}", v.to_str().unwrap_or("").chars().take(220).collect::<String>()); }
                }
                let c = auth::claims(&t.access_token)?;
                println!("sharepoint scp has AllSites.Write {} MyFiles.Write {}", c["scp"].as_str().unwrap_or("").contains("AllSites.Write"), c["scp"].as_str().unwrap_or("").contains("MyFiles.Write"));
            }
            Err(e) => println!("sharepoint token: {e}"),
        }
    }
    Ok(())
}

/// `teamlite --probe-ams <tenant>`: newest image in notes-to-self, fetched with each auth form.
pub fn ams(tenant: &str) -> anyhow::Result<()> {
    let acc = store::load_account().ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    let rt = acc.refresh_for(tenant).ok_or_else(|| anyhow::anyhow!("no refresh token"))?.to_string();
    let sk = teams::authz(&auth::refresh(tenant, &rt, auth::SKYPE_SCOPE)?.access_token)?;
    println!("regionGtms ams = {}", sk.ams);
    let v: Value = http().get(format!("{}/v1/users/ME/conversations/48%3Anotes/messages?view=msnp24Equivalent&pageSize=20&startTime=1", sk.chat_service)).header("Authentication", format!("skypetoken={}", sk.skype_token)).send()?.json()?;
    let src = v["messages"].as_array().into_iter().flatten().filter_map(|m| m["content"].as_str()).find_map(|c| c.split(r#"src=""#).nth(1).map(|s| s.split('"').next().unwrap_or("").to_string()));
    let Some(src) = src else { println!("no image"); return Ok(()) };
    println!("src host {}", src.split('/').take(3).collect::<Vec<_>>().join("/"));
    let tries: [(&str, Box<dyn Fn(reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder>); 3] = [
        ("Authorization: skype_token", Box::new(|r| r.header("Authorization", format!("skype_token {}", sk.skype_token)))),
        ("Cookie skypetoken_asm", Box::new(|r| r.header("Cookie", format!("skypetoken_asm={}", sk.skype_token)))),
        ("Authentication: skypetoken=", Box::new(|r| r.header("Authentication", format!("skypetoken={}", sk.skype_token)))),
    ];
    for (label, f) in tries {
        let r = f(http().get(&src)).send()?;
        println!("{label}: HTTP {} ({} bytes)", r.status(), r.bytes().map(|b| b.len()).unwrap_or(0));
    }
    Ok(())
}
