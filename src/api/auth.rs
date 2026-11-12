//! Azure AD sign-in for the Teams desktop client id (authorization code + PKCE), token refresh,
//! and per-tenant token minting for multitenancy.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{http, now_secs};

/// Microsoft Teams (desktop) public client. First-party, preauthorized for the Teams resources and
/// part of the family-of-client-ids, so one refresh token covers chat, presence and the middle tier.
/// Override with `TEAMLITE_CLIENT_ID` / `TEAMLITE_REDIRECT_URI` if a tenant blocks it.
const DEFAULT_CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
const DEFAULT_REDIRECT_URI: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";

fn env_or(key: &str, default: &'static str) -> &'static str {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Box::leak(v.into_boxed_str()),
        _ => default,
    }
}

pub fn client_id() -> &'static str {
    static V: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    V.get_or_init(|| env_or("TEAMLITE_CLIENT_ID", DEFAULT_CLIENT_ID))
}

pub fn redirect_uri() -> &'static str {
    static V: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    V.get_or_init(|| env_or("TEAMLITE_REDIRECT_URI", DEFAULT_REDIRECT_URI))
}
pub const LOGIN: &str = "https://login.microsoftonline.com";
/// Audience Teams' authz service and middle tier accept.
pub const SKYPE_SCOPE: &str = "https://api.spaces.skype.com/.default";

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        let mut bytes = [0u8; 48];
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce { verifier, challenge }
    }
}

pub fn authorize_url_in(tenant: &str, pkce: &Pkce, login_hint: Option<&str>) -> String {
    let mut u = url::Url::parse(&format!("{LOGIN}/{tenant}/oauth2/v2.0/authorize")).unwrap();
    u.query_pairs_mut()
        .append_pair("client_id", client_id())
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri())
        .append_pair("scope", &format!("{SKYPE_SCOPE} offline_access openid profile"))
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256");
    if tenant == "organizations" {
        u.query_pairs_mut().append_pair("prompt", "select_account");
    }
    if let Some(h) = login_hint {
        u.query_pairs_mut().append_pair("login_hint", h);
    }
    u.to_string()
}

/// Pulls `code` out of the redirect URL the login window landed on.
pub fn code_from_redirect(redirect: &str) -> Result<String> {
    let u = url::Url::parse(redirect.trim()).context("not a URL")?;
    let mut code = None;
    let mut err = None;
    for (k, v) in u.query_pairs() {
        match &*k {
            "code" => code = Some(v.into_owned()),
            "error_description" => err = Some(v.into_owned()),
            _ => {}
        }
    }
    match (code, err) {
        (Some(c), _) => Ok(c),
        (None, Some(e)) => bail!("sign-in failed: {e}"),
        _ => bail!("no authorization code in the redirect"),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: u64,
}

impl Tokens {
    pub fn fresh(&self) -> bool {
        self.expires_at > now_secs() + 120
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Who signed in, from the id token (or access token) claims.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Identity {
    pub oid: String,
    pub tid: String,
    pub name: String,
    pub upn: String,
}

pub fn claims(jwt: &str) -> Result<serde_json::Value> {
    let payload = jwt.split('.').nth(1).ok_or_else(|| anyhow!("malformed token"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .context("token payload")?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn identity(jwt: &str) -> Result<Identity> {
    let c = claims(jwt)?;
    let s = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let upn = [s("preferred_username"), s("upn"), s("unique_name")]
        .into_iter()
        .find(|x| !x.is_empty())
        .unwrap_or_default();
    Ok(Identity { oid: s("oid"), tid: s("tid"), name: s("name"), upn })
}

fn token_request(tenant: &str, form: &[(&str, &str)]) -> Result<(Tokens, Option<String>)> {
    let resp = http()
        .post(format!("{LOGIN}/{tenant}/oauth2/v2.0/token"))
        .form(form)
        .send()
        .context("token endpoint unreachable")?;
    let body: TokenResponse = resp.json().context("token response")?;
    if let Some(e) = body.error {
        bail!("{e}: {}", body.error_description.unwrap_or_default().lines().next().unwrap_or(""));
    }
    let access = body.access_token.ok_or_else(|| anyhow!("no access token"))?;
    let tokens = Tokens {
        access_token: access,
        refresh_token: body.refresh_token.unwrap_or_default(),
        expires_at: now_secs() + body.expires_in.unwrap_or(3600),
    };
    Ok((tokens, body.id_token))
}

/// Redeems the authorization code. Returns the tokens and who signed in.
pub fn redeem_code_in(tenant: &str, code: &str, verifier: &str) -> Result<(Tokens, Identity)> {
    let scope = format!("{SKYPE_SCOPE} offline_access openid profile");
    let (tokens, id_token) = token_request(
        tenant,
        &[
            ("client_id", client_id()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri()),
            ("code_verifier", verifier),
            ("scope", &scope),
        ],
    )?;
    let who = identity(id_token.as_deref().unwrap_or(&tokens.access_token))?;
    Ok((tokens, who))
}

/// Mints a token for `scope` in `tenant` from a refresh token. Works across tenants the user is a
/// member or guest of: that's how Teams switches organisations without a new sign-in.
pub fn refresh(tenant: &str, refresh_token: &str, scope: &str) -> Result<Tokens> {
    let scope = format!("{scope} offline_access");
    let (mut tokens, _) = token_request(
        tenant,
        &[
            ("client_id", client_id()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", &scope),
        ],
    )?;
    if tokens.refresh_token.is_empty() {
        tokens.refresh_token = refresh_token.to_string();
    }
    Ok(tokens)
}
