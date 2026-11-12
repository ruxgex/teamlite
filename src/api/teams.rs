//! Teams' own services, as the web client (teams.microsoft.com/v2) calls them:
//! authsvc (AAD token → skype token + regional hosts), the chat service (conversations, messages,
//! long-poll notifications) and the middle tier (tenants, profiles).

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::{RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{http, now_secs};

pub const AUTHZ: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkypeAuth {
    pub skype_token: String,
    pub expires_at: u64,
    pub region: String,
    /// e.g. https://emea.ng.msg.teams.microsoft.com
    pub chat_service: String,
    /// e.g. https://teams.microsoft.com/api/mt/emea
    pub middle_tier: String,
    /// Media service for images, e.g. https://eu-api.asm.skype.com
    #[serde(default)]
    pub ams: String,
}

impl SkypeAuth {
    pub fn fresh(&self) -> bool {
        self.expires_at > now_secs() + 300
    }
}

fn check(resp: Response, what: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    bail!("{what}: HTTP {status} {body}")
}

/// Exchanges an AAD token (audience api.spaces.skype.com) for a skype token and region hosts.
pub fn authz(aad_token: &str) -> Result<SkypeAuth> {
    let resp = http()
        .post(AUTHZ)
        .bearer_auth(aad_token)
        .header("ms-teams-authz-type", "TokenRefresh")
        .header("Content-Length", "0")
        .send()
        .context("authz unreachable")?;
    let v: Value = check(resp, "authz")?.json()?;
    let tokens = &v["tokens"];
    let skype_token = tokens["skypeToken"].as_str().ok_or_else(|| anyhow!("authz: no skype token"))?;
    let expires_in = tokens["expiresIn"].as_u64().unwrap_or(3600);
    let g = &v["regionGtms"];
    let s = |k: &str| g[k].as_str().unwrap_or_default().trim_end_matches('/').to_string();
    let region = v["region"].as_str().unwrap_or("emea").to_string();
    let chat_service = Some(s("chatService"))
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| format!("https://{region}.ng.msg.teams.microsoft.com"));
    let middle_tier = Some(s("middleTier"))
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| format!("https://teams.microsoft.com/api/mt/{region}"));
    let ams = Some(s("ams")).filter(|x| x.starts_with("https://")).unwrap_or_else(|| {
        match region.as_str() {
            "amer" | "us" | "ca" | "br" => "https://us-api.asm.skype.com",
            "apac" | "au" | "jp" | "in" => "https://ap-api.asm.skype.com",
            _ => "https://eu-api.asm.skype.com",
        }
        .to_string()
    });
    Ok(SkypeAuth {
        ams,
        skype_token: skype_token.to_string(),
        expires_at: now_secs() + expires_in,
        region,
        chat_service,
        middle_tier,
    })
}

// ---------------------------------------------------------------- middle tier

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    /// The user's object id in that tenant (a guest has its own).
    pub user_id: String,
    pub is_home: bool,
}

/// Every organisation the account belongs to, home and guest (`/beta/users/tenantsv2`).
pub fn tenants(middle_tier: &str, aad_token: &str, home_tid: &str) -> Result<Vec<Tenant>> {
    let resp = http()
        .get(format!("{middle_tier}/beta/users/tenantsv2"))
        .bearer_auth(aad_token)
        .send()
        .context("tenants unreachable")?;
    let v: Value = check(resp, "tenants")?.json()?;
    let list = v.as_array().cloned().or_else(|| v["value"].as_array().cloned()).unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|t| {
            let id = t["tenantId"].as_str()?.to_string();
            let redeemed = t["isInvitationRedeemed"].as_bool().unwrap_or(true);
            let is_home = id == home_tid;
            if !redeemed && !is_home {
                return None;
            }
            Some(Tenant {
                name: t["tenantName"].as_str().unwrap_or(&id).to_string(),
                user_id: t["userId"].as_str().unwrap_or_default().to_string(),
                is_home,
                id,
            })
        })
        .collect())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub mri: String,
    pub display_name: String,
    pub email: String,
}

/// Display names for user MRIs (`8:orgid:<oid>`), batched.
pub fn short_profiles(middle_tier: &str, aad_token: &str, mris: &[String]) -> Result<Vec<Profile>> {
    let mut out = Vec::new();
    for chunk in mris.chunks(50) {
        let resp = http()
            .post(format!(
                "{middle_tier}/beta/users/fetchShortProfile?isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true"
            ))
            .bearer_auth(aad_token)
            .json(&chunk)
            .send()
            .context("profiles unreachable")?;
        let v: Value = check(resp, "profiles")?.json()?;
        for p in v["value"].as_array().into_iter().flatten() {
            let mri = p["mri"].as_str().unwrap_or_default().to_string();
            if mri.is_empty() {
                continue;
            }
            out.push(Profile {
                display_name: p["displayName"].as_str().unwrap_or(&mri).to_string(),
                email: p["email"].as_str().or(p["userPrincipalName"].as_str()).unwrap_or_default().to_string(),
                mri,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- chat service

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub client_message_id: String,
    /// `8:orgid:<oid>` (from the `from` contact URL).
    pub from: String,
    pub from_name: String,
    pub html: String,
    pub message_type: String,
    /// Unix milliseconds.
    pub time: i64,
    pub edited: bool,
    pub deleted: bool,
    /// Reactions: emotion key ("like", "heart", "laugh"…) → who reacted (MRIs).
    #[serde(default)]
    pub reactions: Vec<(String, Vec<String>)>,
    /// Channel posts: the reply chain's root message id (equal to `id` for a root post).
    #[serde(default)]
    pub root: String,
    /// Attached files: (name, url, size).
    #[serde(default)]
    pub files: Vec<(String, String, u64)>,
}

impl Message {
    pub fn is_chat_content(&self) -> bool {
        matches!(self.message_type.as_str(), "RichText/Html" | "Text" | "RichText" | "RichText/Media_Card" | "RichText/UriObject")
    }
}

fn mri_from_url(u: &str) -> String {
    u.rsplit("/contacts/").next().unwrap_or(u).to_string()
}

fn parse_time(s: &str) -> i64 {
    // 2026-11-12T14:03:12.345Z
    let s = s.trim();
    if s.len() < 19 {
        return 0;
    }
    let num = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let (y, mo, d, h, mi, se) = (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19));
    let ms = s.get(20..23).and_then(|x| x.trim_end_matches('Z').parse::<i64>().ok()).unwrap_or(0);
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    ((days * 86400 + h * 3600 + mi * 60 + se) * 1000) + ms
}

pub fn parse_message(m: &Value, conv: &str) -> Message {
    let s = |k: &str| m[k].as_str().unwrap_or_default().to_string();
    let time = m["originalarrivaltime"]
        .as_str()
        .or(m["composetime"].as_str())
        .map(parse_time)
        .unwrap_or(0);
    let conversation_id = m["conversationid"]
        .as_str()
        .map(str::to_string)
        .or_else(|| m["conversationLink"].as_str().map(|l| l.rsplit('/').next().unwrap_or(l).to_string()))
        .unwrap_or_else(|| conv.to_string());
    let props = &m["properties"];
    let emotions = match &props["emotions"] {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        v => v.clone(),
    };
    let reactions = emotions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let key = e["key"].as_str()?.to_string();
            let users: Vec<String> = e["users"].as_array().into_iter().flatten().filter_map(|u| u["mri"].as_str().map(str::to_string)).collect();
            (!users.is_empty()).then_some((key, users))
        })
        .collect();
    let root = m["conversationLink"].as_str().and_then(|l| l.split(";messageid=").nth(1)).unwrap_or_default().to_string();
    Message {
        files: super::files::parse_files(props),
        reactions,
        root,
        id: s("id"),
        conversation_id,
        client_message_id: s("clientmessageid"),
        from: mri_from_url(&s("from")),
        from_name: s("imdisplayname"),
        html: s("content"),
        message_type: s("messagetype"),
        time,
        edited: props["edittime"].as_str().is_some_and(|x| !x.is_empty()) || props["edittime"].is_number(),
        deleted: props["deletetime"].as_str().is_some_and(|x| !x.is_empty()) || props["deletetime"].is_number(),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum ConvKind {
    #[default]
    OneOnOne,
    Group,
    Meeting,
    Channel,
    SelfChat,
    Other,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub kind: ConvKind,
    /// Topic for groups and meetings, channel name for channels (empty for 1:1 until resolved).
    pub topic: String,
    /// Team name for channels.
    pub team: String,
    pub last: Option<Message>,
    /// Unix ms of the last message we've read (consumption horizon).
    pub read_horizon: i64,
    pub pinned: bool,
    /// Pinned through the conversation's own properties (as opposed to the user favourites list).
    #[serde(default)]
    pub starred: bool,
    pub hidden: bool,
    /// Other member MRI for 1:1s.
    pub peer: String,
    pub members: Vec<String>,
}

impl Conversation {
    pub fn last_time(&self) -> i64 {
        self.last.as_ref().map(|m| m.time).unwrap_or(0)
    }
    pub fn unread(&self, me: &str) -> bool {
        self.last.as_ref().is_some_and(|m| m.time > self.read_horizon && m.from != me && m.is_chat_content())
    }
}

fn parse_conversation(c: &Value, me: &str) -> Option<Conversation> {
    let id = c["id"].as_str()?.to_string();
    if id.starts_with("48:") && id != "48:notes" {
        return None; // system streams: notifications, calllogs, mentions, saved…
    }
    let tp = &c["threadProperties"];
    let props = &c["properties"];
    let thread_type = tp["threadType"].as_str().unwrap_or_default();
    let product = tp["productThreadType"].as_str().unwrap_or_default();
    let kind = match (thread_type, product) {
        (_, "OneToOneChat") => ConvKind::OneOnOne,
        ("chat", _) if id.ends_with("@unq.gbl.spaces") => ConvKind::OneOnOne,
        ("meeting", _) | (_, "Meeting") => ConvKind::Meeting,
        ("chat", _) => ConvKind::Group,
        ("space" | "topic", _) => ConvKind::Channel,
        ("streamofnotes", _) | (_, "StreamOfNotes") => ConvKind::SelfChat,
        _ if id.starts_with("48:notes") => ConvKind::SelfChat,
        _ => ConvKind::Other,
    };
    let mut topic = tp["topic"].as_str().unwrap_or_default().to_string();
    let team = tp["spaceThreadTopic"].as_str().unwrap_or_default().to_string();
    if kind == ConvKind::Channel && topic.is_empty() {
        topic = if tp["threadType"] == "space" { "General".into() } else { tp["topicThreadTopic"].as_str().unwrap_or("Channel").into() };
    }
    let me_oid = me.rsplit(':').next().unwrap_or(me);
    let peer = if kind == ConvKind::OneOnOne {
        // 19:<a>_<b>@unq.gbl.spaces
        id.trim_start_matches("19:")
            .trim_end_matches("@unq.gbl.spaces")
            .split('_')
            .find(|x| *x != me_oid)
            .map(|x| format!("8:orgid:{x}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let horizon = props["consumptionhorizon"]
        .as_str()
        .and_then(|h| h.split(';').next())
        .and_then(|x| x.parse::<i64>().ok())
        .unwrap_or(0);
    let truthy = |v: &Value| v.as_bool().unwrap_or(false) || v.as_str() == Some("true");
    let last = c.get("lastMessage").filter(|m| m.is_object() && m["id"].is_string()).map(|m| parse_message(m, &id));
    Some(Conversation {
        kind,
        topic,
        team,
        last,
        read_horizon: horizon,
        // `favorite` on a channel means "favourite channel"; pinned chats are `ispinned` or in
        // the user's favourites list (merged in by the worker).
        pinned: truthy(&props["ispinned"]),
        starred: truthy(&props["ispinned"]),
        hidden: truthy(&props["isemptyconversation"]) && c["lastMessage"]["id"].is_null(),
        peer,
        members: Vec::new(),
        id,
    })
}

pub struct ChatService {
    pub auth: SkypeAuth,
    pub me: String,
}

pub struct Page<T> {
    pub items: Vec<T>,
    pub backward: Option<String>,
}

impl ChatService {
    pub fn new(auth: SkypeAuth, me: String) -> Self {
        ChatService { auth, me }
    }

    fn authed(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("Authentication", format!("skypetoken={}", self.auth.skype_token))
            .header("behavioroverride", "redirectAs404")
            .header("x-ms-client-type", "web")
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.auth.chat_service, path.trim_start_matches('/'))
    }

    /// Full chat list. Returns the list and the sync URL for [`ChatService::changes`].
    pub fn conversations(&self) -> Result<(Vec<Conversation>, String)> {
        let mut out = Vec::new();
        let mut next = Some(self.url("v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1"));
        let mut pages = 0;
        let mut sync = String::new();
        while let Some(u) = next.take() {
            let resp = self.authed(http().get(&u)).send().context("conversations unreachable")?;
            let v: Value = check(resp, "conversations")?.json()?;
            for c in v["conversations"].as_array().into_iter().flatten() {
                if let Some(conv) = parse_conversation(c, &self.me) {
                    out.push(conv);
                }
            }
            if pages == 0 {
                sync = v["_metadata"]["syncState"].as_str().unwrap_or_default().to_string();
            }
            pages += 1;
            next = v["_metadata"]["backwardLink"].as_str().filter(|s| !s.is_empty() && pages < 8).map(str::to_string);
        }
        Ok((out, sync))
    }

    /// Conversations changed since `sync` (empty when nothing happened) and the next sync URL.
    pub fn changes(&self, sync: &str) -> Result<(Vec<Conversation>, String)> {
        let resp = self.authed(http().get(sync)).send().context("sync unreachable")?;
        let status = resp.status().as_u16();
        if status == 429 {
            let wait = resp.headers().get("retry-after").and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<u64>().ok()).unwrap_or(30);
            bail!("throttled: retry after {wait}");
        }
        let v: Value = check(resp, "sync")?.json()?;
        let list = v["conversations"].as_array().into_iter().flatten().filter_map(|c| parse_conversation(c, &self.me)).collect();
        let next = v["_metadata"]["syncState"].as_str().filter(|s| !s.is_empty()).unwrap_or(sync).to_string();
        Ok((list, next))
    }

    /// Conversation ids the user marked as favourite (pinned) in Teams.
    pub fn favorites(&self) -> Result<Vec<String>> {
        let resp = self.authed(http().get(self.url("v1/users/ME/properties"))).send()?;
        let v: Value = check(resp, "properties")?.json()?;
        let fav = v["favorites"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
        Ok(match fav {
            Value::Object(m) => m.keys().cloned().collect(),
            Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string).or_else(|| x["conversationId"].as_str().map(str::to_string))).collect(),
            _ => Vec::new(),
        })
    }

    /// Uploads an image to the media service, readable by `conv`'s members. Returns its object id.
    pub fn upload_image(&self, conv: &str, bytes: &[u8], filename: &str) -> Result<String> {
        let auth = format!("skype_token {}", self.auth.skype_token);
        let body = json!({ "type": "pish/image", "permissions": { conv: ["read"] }, "filename": filename, "sharingMode": "Inline" });
        let resp = http()
            .post(format!("{}/v1/objects/", self.auth.ams))
            .header("Authorization", &auth)
            .header("x-ams-post-sharing-mode", "Inline")
            .json(&body)
            .send()
            .context("media service unreachable")?;
        let v: Value = check(resp, "image upload")?.json()?;
        let id = v["id"].as_str().ok_or_else(|| anyhow!("media service returned no id"))?.to_string();
        let resp = http()
            .put(format!("{}/v1/objects/{id}/content/imgpsh", self.auth.ams))
            .header("Authorization", &auth)
            .header("Content-Type", "application/octet-stream")
            .body(bytes.to_vec())
            .send()?;
        check(resp, "image content")?;
        Ok(id)
    }

    /// Posts a message carrying an uploaded file (OneDrive), with optional text.
    pub fn send_file(&self, conv: &str, files_json: &str, text_html: &str, display_name: &str, client_id: &str) -> Result<()> {
        let body = json!({
            "content": text_html,
            "messagetype": "RichText/Html",
            "contenttype": "text",
            "clientmessageid": client_id,
            "imdisplayname": display_name,
            "properties": { "importance": "", "subject": null, "files": files_json },
        });
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages", urlenc(conv)));
        check(self.authed(http().post(u)).json(&body).send()?, "send file")?;
        Ok(())
    }

    /// Posts a message showing an uploaded image (and optional text above it).
    pub fn send_image(&self, conv: &str, id: &str, size: (u32, u32), text_html: &str, display_name: &str, client_id: &str) -> Result<()> {
        let (w, h) = size;
        let scale = (250.0 / w.max(1) as f32).min(1.0);
        let (dw, dh) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
        let src = format!("{}/v1/objects/{id}/views/imgo", self.auth.ams);
        let img = format!(r#"<p><img itemscope="" itemtype="http://schema.skype.com/AMSImage" src="{src}" width="{dw}" height="{dh}" id="x_{id}" itemid="{id}" href="{src}" target-src="{src}"></p>"#);
        let body = json!({
            "content": format!("{text_html}{img}"),
            "messagetype": "RichText/Html",
            "contenttype": "text",
            "amsreferences": [id],
            "clientmessageid": client_id,
            "imdisplayname": display_name,
            "properties": { "importance": "", "subject": null },
        });
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages", urlenc(conv)));
        check(self.authed(http().post(u)).json(&body).send()?, "send image")?;
        Ok(())
    }

    /// "… is typing" for the other members (Teams clears it after a few seconds).
    pub fn typing(&self, conv: &str) -> Result<()> {
        let body = json!({ "content": "", "messagetype": "Control/Typing", "contenttype": "Application/Message" });
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages", urlenc(conv)));
        check(self.authed(http().post(u)).json(&body).send()?, "typing")?;
        Ok(())
    }

    pub fn edit(&self, conv: &str, id: &str, html: &str, display_name: &str) -> Result<()> {
        let body = json!({ "content": html, "messagetype": "RichText/Html", "contenttype": "text", "imdisplayname": display_name });
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages/{}", urlenc(conv), urlenc(id)));
        check(self.authed(http().put(u)).json(&body).send()?, "edit")?;
        Ok(())
    }

    pub fn delete(&self, conv: &str, id: &str) -> Result<()> {
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages/{}?behavior=softDelete", urlenc(conv), urlenc(id)));
        check(self.authed(http().delete(u)).send()?, "delete")?;
        Ok(())
    }

    /// Adds or removes the user's reaction (`like`, `heart`, `laugh`, `surprised`, `sad`, `angry`).
    pub fn react(&self, conv: &str, id: &str, key: &str, add: bool) -> Result<()> {
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages/{}/properties?name=emotions", urlenc(conv), urlenc(id)));
        let rb = if add {
            self.authed(http().put(u)).json(&json!({ "emotions": { "key": key, "value": super::now_ms() } }))
        } else {
            self.authed(http().delete(u)).json(&json!({ "emotions": { "key": key } }))
        };
        check(rb.send()?, "reaction")?;
        Ok(())
    }

    /// Image bytes: AMS objects need the skype token, other hosts are fetched as they are.
    pub fn image(&self, url: &str) -> Result<Vec<u8>> {
        let mut rb = http().get(url);
        let ams_host = url.contains(".asm.skype.com/") || url.contains("/api/asm/") || url.contains(".asyncgw.teams.microsoft.com/") || (!self.auth.ams.is_empty() && url.starts_with(&self.auth.ams));
        if ams_host {
            rb = rb.header("Authorization", format!("skype_token {}", self.auth.skype_token));
        }
        let resp = check(rb.send()?, "image")?;
        let bytes = resp.bytes()?;
        if bytes.len() > 25 * 1024 * 1024 {
            bail!("image too large");
        }
        Ok(bytes.to_vec())
    }

    pub fn thread_members(&self, id: &str) -> Result<Vec<String>> {
        let u = self.url(&format!("v1/threads/{}?view=msnp24Equivalent", urlenc(id)));
        let resp = self.authed(http().get(u)).send()?;
        let v: Value = check(resp, "thread")?.json()?;
        Ok(v["members"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["id"].as_str().map(str::to_string))
            .filter(|m| m.starts_with("8:"))
            .collect())
    }

    /// Newest page of messages, oldest first. `backward` continues further into history.
    pub fn messages(&self, conv: &str, backward: Option<&str>) -> Result<Page<Message>> {
        let u = match backward {
            Some(b) => b.to_string(),
            None => self.url(&format!(
                "v1/users/ME/conversations/{}/messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=50&startTime=1",
                urlenc(conv)
            )),
        };
        let resp = self.authed(http().get(u)).send().context("messages unreachable")?;
        let v: Value = check(resp, "messages")?.json()?;
        let mut items: Vec<Message> = v["messages"].as_array().into_iter().flatten().map(|m| parse_message(m, conv)).collect();
        items.sort_by_key(|m| m.time);
        let backward = v["_metadata"]["backwardLink"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        Ok(Page { items, backward })
    }

    pub fn send(&self, conv: &str, html: &str, display_name: &str, client_id: &str) -> Result<i64> {
        let body = json!({
            "content": html,
            "messagetype": "RichText/Html",
            "contenttype": "text",
            "clientmessageid": client_id,
            "imdisplayname": display_name,
            "properties": { "importance": "", "subject": null },
        });
        let u = self.url(&format!("v1/users/ME/conversations/{}/messages", urlenc(conv)));
        let resp = self.authed(http().post(u)).json(&body).send().context("send unreachable")?;
        let v: Value = check(resp, "send")?.json().unwrap_or(Value::Null);
        Ok(v["OriginalArrivalTime"].as_i64().unwrap_or(0))
    }

    pub fn mark_read(&self, conv: &str, msg: &Message) -> Result<()> {
        let horizon = format!("{};{};{}", msg.time, now_secs() * 1000, msg.id);
        let u = self.url(&format!("v1/users/ME/conversations/{}/properties?name=consumptionhorizon", urlenc(conv)));
        let resp = self.authed(http().put(u)).json(&json!({ "consumptionhorizon": horizon })).send()?;
        check(resp, "mark read")?;
        Ok(())
    }

}

/// Availability for user MRIs ("Available", "Busy", "DoNotDisturb", "Away", "Offline"…).
pub fn presence(aad_presence_token: &str, mris: &[String]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for chunk in mris.chunks(100) {
        let body: Vec<Value> = chunk.iter().map(|m| json!({ "mri": m })).collect();
        let resp = http()
            .post("https://presence.teams.microsoft.com/v1/presence/getpresence/")
            .bearer_auth(aad_presence_token)
            .json(&body)
            .send()?;
        let v: Value = check(resp, "presence")?.json()?;
        for p in v.as_array().into_iter().flatten() {
            if let (Some(m), Some(a)) = (p["mri"].as_str(), p["presence"]["availability"].as_str()) {
                out.push((m.to_string(), a.to_string()));
            }
        }
    }
    Ok(out)
}

pub fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse() {
        assert_eq!(parse_time("1970-01-01T00:00:01.500Z"), 1500);
        assert_eq!(parse_time("2026-11-12T00:00:00.000Z"), 1794441600000);
    }

    #[test]
    fn one_on_one_peer() {
        let c = json!({"id":"19:aaa_bbb@unq.gbl.spaces","threadProperties":{"threadType":"chat","productThreadType":"OneToOneChat"},"properties":{}});
        let conv = parse_conversation(&c, "8:orgid:aaa").unwrap();
        assert_eq!(conv.kind, ConvKind::OneOnOne);
        assert_eq!(conv.peer, "8:orgid:bbb");
    }

    #[test]
    fn reactions_and_reply_root() {
        let m = json!({"id":"5","conversationLink":"https://x/v1/users/ME/conversations/19:c@thread.tacv2;messageid=3",
            "properties":{"emotions":[{"key":"like","users":[{"mri":"8:orgid:a","time":1,"value":"1"}]}]}});
        let p = parse_message(&m, "19:c@thread.tacv2");
        assert_eq!(p.root, "3");
        assert_eq!(p.reactions, vec![("like".to_string(), vec!["8:orgid:a".to_string()])]);
        let m2 = json!({"id":"6","properties":{"emotions":"[{\"key\":\"heart\",\"users\":[{\"mri\":\"8:orgid:b\"}]}]"}});
        assert_eq!(parse_message(&m2, "c").reactions[0].0, "heart");
    }

    #[test]
    fn mri_from_contact_url() {
        assert_eq!(mri_from_url("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/8:orgid:abc"), "8:orgid:abc");
    }
}
