//! What survives a restart: the account's refresh token, its tenants, and a cache of each tenant's
//! chat list and recent messages so the window opens with content before the network answers.
//! Files are written 0600 under $XDG_CONFIG_HOME/teamlite and $XDG_CACHE_HOME/teamlite.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::auth::Identity;
use crate::api::teams::{Conversation, Message, Tenant};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    pub identity: Identity,
    /// Refresh tokens by tenant id; the home tenant's is the one minted at sign-in.
    pub refresh: HashMap<String, String>,
    pub tenants: Vec<Tenant>,
    pub active_tenant: String,
    /// Tenants the user hid from the switcher.
    #[serde(default)]
    pub muted_tenants: Vec<String>,
}

impl Account {
    pub fn refresh_for(&self, tenant: &str) -> Option<&str> {
        self.refresh
            .get(tenant)
            .or_else(|| self.refresh.get(&self.identity.tid))
            .map(String::as_str)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TenantCache {
    pub conversations: Vec<Conversation>,
    pub messages: HashMap<String, Vec<Message>>,
    pub names: HashMap<String, String>,
}

fn dir(base: Option<PathBuf>) -> PathBuf {
    let d = base.unwrap_or_else(|| PathBuf::from("/tmp")).join("teamlite");
    let _ = fs::create_dir_all(&d);
    d
}

fn config_dir() -> PathBuf {
    std::env::var_os("TEAMLITE_HOME").map(PathBuf::from).map(|p| { let _ = fs::create_dir_all(&p); p }).unwrap_or_else(|| dir(dirs::config_dir()))
}

fn cache_dir() -> PathBuf {
    std::env::var_os("TEAMLITE_HOME").map(|p| PathBuf::from(p).join("cache")).map(|p| { let _ = fs::create_dir_all(&p); p }).unwrap_or_else(|| dir(dirs::cache_dir()))
}

fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    // Headless UI runs (snapshots, scripts) use made-up data: never persist it.
    if rune::tooling::headless_run().is_some() {
        return Ok(());
    }
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(tmp, path)
}

pub fn load_account() -> Option<Account> {
    let bytes = fs::read(config_dir().join("account.json")).ok()?;
    let a: Account = serde_json::from_slice(&bytes).ok()?;
    // No refresh token means no session (an interrupted sign-in, or test data).
    if a.refresh.values().all(|t| t.is_empty()) { None } else { Some(a) }
}

pub fn save_account(a: &Account) {
    if let Ok(bytes) = serde_json::to_vec_pretty(a) {
        let _ = write_private(&config_dir().join("account.json"), &bytes);
    }
}

pub fn forget_account() {
    let _ = fs::remove_file(config_dir().join("account.json"));
    let _ = fs::remove_dir_all(cache_dir());
}

fn cache_path(tenant: &str) -> PathBuf {
    let safe: String = tenant.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    cache_dir().join(format!("{safe}.json"))
}

pub fn load_cache(tenant: &str) -> TenantCache {
    fs::read(cache_path(tenant)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_cache(tenant: &str, c: &TenantCache) {
    if let Ok(bytes) = serde_json::to_vec(c) {
        let _ = write_private(&cache_path(tenant), &bytes);
    }
}
