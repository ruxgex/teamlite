//! Sending files the way Teams does: upload to the sender's OneDrive ("Microsoft Teams Chat
//! Files"), give the chat's members read access, then post a message whose `files` property
//! points at it.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::http;

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const FOLDER: &str = "Microsoft Teams Chat Files";
const SIMPLE_MAX: usize = 4 * 1024 * 1024;
const CHUNK: usize = 5 * 320 * 1024; // multiple of 320 KiB, as Graph requires

pub struct Uploaded {
    pub item_id: String,
    pub name: String,
    pub size: u64,
    pub web_url: String,
    pub site_url: String,
    pub unique_id: String,
    pub server_relative_url: String,
}

fn ok(resp: reqwest::blocking::Response, what: &str) -> Result<Value> {
    let st = resp.status();
    let body = resp.text().unwrap_or_default();
    if !st.is_success() {
        bail!("{what}: HTTP {st} {}", body.chars().take(200).collect::<String>());
    }
    Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn enc_path(name: &str) -> String {
    format!("{}/{}", super::teams::urlenc(FOLDER), super::teams::urlenc(name))
}

pub fn upload(graph_token: &str, name: &str, bytes: &[u8]) -> Result<Uploaded> {
    let item: Value = if bytes.len() <= SIMPLE_MAX {
        let resp = http()
            .put(format!("{GRAPH}/me/drive/root:/{}:/content?@microsoft.graph.conflictBehavior=rename", enc_path(name)))
            .bearer_auth(graph_token)
            .header("Content-Type", "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .context("OneDrive unreachable")?;
        ok(resp, "upload")?
    } else {
        let resp = http()
            .post(format!("{GRAPH}/me/drive/root:/{}:/createUploadSession", enc_path(name)))
            .bearer_auth(graph_token)
            .json(&json!({ "item": { "@microsoft.graph.conflictBehavior": "rename" } }))
            .send()?;
        let session = ok(resp, "upload session")?;
        let url = session["uploadUrl"].as_str().ok_or_else(|| anyhow!("no upload URL"))?.to_string();
        let total = bytes.len();
        let mut last = Value::Null;
        for start in (0..total).step_by(CHUNK) {
            let end = (start + CHUNK).min(total);
            let resp = http()
                .put(&url)
                .header("Content-Range", format!("bytes {start}-{}/{total}", end - 1))
                .body(bytes[start..end].to_vec())
                .timeout(std::time::Duration::from_secs(300))
                .send()?;
            last = ok(resp, "upload chunk")?;
        }
        last
    };
    let id = item["id"].as_str().ok_or_else(|| anyhow!("upload returned no item"))?.to_string();
    let resp = http()
        .get(format!("{GRAPH}/me/drive/items/{id}?$select=id,name,size,webUrl,sharepointIds,parentReference"))
        .bearer_auth(graph_token)
        .send()?;
    let meta = ok(resp, "file details")?;
    let sp = &meta["sharepointIds"];
    let web_url = meta["webUrl"].as_str().unwrap_or_default().to_string();
    let site_url = sp["siteUrl"].as_str().unwrap_or_default().trim_end_matches('/').to_string() + "/";
    let server_relative_url = web_url.split(".sharepoint.com").nth(1).map(|p| url_decode(p)).unwrap_or_default();
    Ok(Uploaded {
        item_id: id,
        name: meta["name"].as_str().unwrap_or(name).to_string(),
        size: meta["size"].as_u64().unwrap_or(bytes.len() as u64),
        web_url,
        site_url,
        unique_id: sp["listItemUniqueId"].as_str().unwrap_or_default().to_uppercase(),
        server_relative_url,
    })
}

fn url_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or_default()
}

/// Moves a chat file to the OneDrive recycle bin (test cleanup).
pub fn remove(graph_token: &str, name: &str) -> Result<()> {
    let resp = http().delete(format!("{GRAPH}/me/drive/root:/{}", enc_path(name))).bearer_auth(graph_token).send()?;
    if !resp.status().is_success() && resp.status().as_u16() != 404 {
        bail!("remove: HTTP {}", resp.status());
    }
    Ok(())
}

/// Read access for the chat's members (by email); no invitation mail is sent.
pub fn share(graph_token: &str, item_id: &str, emails: &[String]) -> Result<()> {
    if emails.is_empty() {
        return Ok(());
    }
    let recipients: Vec<Value> = emails.iter().map(|e| json!({ "email": e })).collect();
    let resp = http()
        .post(format!("{GRAPH}/me/drive/items/{item_id}/invite"))
        .bearer_auth(graph_token)
        .json(&json!({ "recipients": recipients, "roles": ["read"], "requireSignIn": true, "sendInvitation": false }))
        .send()?;
    ok(resp, "share")?;
    Ok(())
}

/// The message `files` property for an uploaded file.
pub fn files_property(u: &Uploaded) -> String {
    let ext = u.name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    json!([{
        "@type": "http://schema.skype.com/File",
        "itemid": u.unique_id,
        "fileName": u.name,
        "originalFileName": u.name,
        "fileType": ext,
        "size": u.size,
        "fileInfo": { "fileUrl": u.web_url, "siteUrl": u.site_url, "serverRelativeUrl": u.server_relative_url },
        "fileChicletState": { "serviceName": "p2p", "state": "active" },
    }])
    .to_string()
}

/// Files attached to a received message: (name, url, size).
pub fn parse_files(props: &Value) -> Vec<(String, String, u64)> {
    let v = match &props["files"] {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        v => v.clone(),
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let name = f["fileName"].as_str().or(f["title"].as_str())?.to_string();
            let url = f["fileInfo"]["fileUrl"].as_str().or(f["objectUrl"].as_str()).unwrap_or_default().to_string();
            Some((name, url, f["size"].as_u64().unwrap_or(0)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_round_trip() {
        let u = Uploaded { item_id: "x".into(), name: "report.pdf".into(), size: 12, web_url: "https://c-my.sharepoint.com/personal/a/Documents/report.pdf".into(), site_url: "https://c-my.sharepoint.com/personal/a/".into(), unique_id: "ABC".into(), server_relative_url: "/personal/a/Documents/report.pdf".into() };
        let props = json!({ "files": files_property(&u) });
        assert_eq!(parse_files(&props), vec![("report.pdf".to_string(), u.web_url.clone(), 12)]);
    }
}
