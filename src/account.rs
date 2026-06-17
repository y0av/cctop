//! Account identity (local) and live plan-usage gauges (OAuth).
//!
//! Endpoints confirmed from the Claude Code bundle:
//!   GET  https://api.anthropic.com/api/oauth/usage   (Bearer + oauth beta header)
//!   POST https://platform.claude.com/v1/oauth/token  (refresh_token grant)
//! The usage response carries `five_hour` / `seven_day` / `seven_day_opus` /
//! `seven_day_sonnet`, each `{utilization, resets_at}`.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{UsageSource, UsageWindows, Window};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "cctop/0.1 (claude-usage-monitor)";

/// Local account identity, read from ~/.claude.json + credentials.
pub struct Account {
    pub display_name: String,
    pub email: String,
    pub org: String,
    pub subscription: String,
    pub rate_limit_tier: String,
}

pub fn read_account(home: &Path) -> Account {
    let mut acc = Account {
        display_name: String::new(),
        email: String::new(),
        org: String::new(),
        subscription: String::new(),
        rate_limit_tier: String::new(),
    };
    if let Ok(txt) = std::fs::read_to_string(home.join(".claude.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if let Some(o) = v.get("oauthAccount") {
                acc.display_name =
                    o.get("displayName").and_then(Value::as_str).unwrap_or("").to_string();
                acc.email = o.get("emailAddress").and_then(Value::as_str).unwrap_or("").to_string();
                acc.org =
                    o.get("organizationName").and_then(Value::as_str).unwrap_or("").to_string();
            }
        }
    }
    if let Ok(c) = read_creds(&home.join(".claude/.credentials.json")) {
        acc.subscription = c.subscription_type;
        acc.rate_limit_tier = c.rate_limit_tier;
    }
    acc
}

struct Creds {
    access_token: String,
    refresh_token: String,
    expires_at_ms: i64,
    subscription_type: String,
    rate_limit_tier: String,
}

fn read_creds(path: &Path) -> Result<Creds, String> {
    let txt = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    let o = v.get("claudeAiOauth").ok_or("no claudeAiOauth")?;
    Ok(Creds {
        access_token: o.get("accessToken").and_then(Value::as_str).unwrap_or("").to_string(),
        refresh_token: o.get("refreshToken").and_then(Value::as_str).unwrap_or("").to_string(),
        expires_at_ms: o.get("expiresAt").and_then(Value::as_i64).unwrap_or(0),
        subscription_type: o
            .get("subscriptionType")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        rate_limit_tier: o.get("rateLimitTier").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

/// Ensure the access token is fresh (refresh + write back if near expiry),
/// then fetch the live usage windows. Returns a `Live` `UsageWindows`.
pub fn ensure_and_fetch(creds_path: &Path, now_ms: i64) -> Result<UsageWindows, String> {
    let mut creds = read_creds(creds_path)?;

    // Refresh proactively if within 5 minutes of expiry.
    if creds.expires_at_ms.saturating_sub(now_ms) < 5 * 60 * 1000 && !creds.refresh_token.is_empty()
    {
        if let Ok(new) = refresh_token(&creds.refresh_token, now_ms) {
            write_back(creds_path, &new)?;
            creds.access_token = new.access_token;
            creds.expires_at_ms = new.expires_at_ms;
            creds.refresh_token = new.refresh_token;
        }
    }

    match fetch_usage(&creds.access_token) {
        Ok(u) => Ok(u),
        Err(e) => {
            // One retry path: maybe the token just expired — try a refresh.
            if !creds.refresh_token.is_empty() {
                if let Ok(new) = refresh_token(&creds.refresh_token, now_ms) {
                    write_back(creds_path, &new)?;
                    return fetch_usage(&new.access_token);
                }
            }
            Err(e)
        }
    }
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

fn fetch_usage(access_token: &str) -> Result<UsageWindows, String> {
    if access_token.is_empty() {
        return Err("no access token".into());
    }
    let resp = http_client()?
        .get(USAGE_URL)
        .bearer_auth(access_token)
        .header("anthropic-beta", OAUTH_BETA)
        .header("anthropic-version", "2023-06-01")
        .header("User-Agent", USER_AGENT)
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("usage HTTP {}", status.as_u16()));
    }
    let v: Value = resp.json().map_err(|e| e.to_string())?;
    Ok(UsageWindows {
        five_hour: parse_window(v.get("five_hour")),
        seven_day: parse_window(v.get("seven_day")),
        seven_day_opus: parse_window(v.get("seven_day_opus")),
        seven_day_sonnet: parse_window(v.get("seven_day_sonnet")),
        source: UsageSource::Live,
        note: None,
    })
}

fn parse_window(v: Option<&Value>) -> Option<Window> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let utilization = v.get("utilization").and_then(Value::as_f64);
    let resets_at = v
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    // A window with neither field is not worth showing.
    utilization?;
    Some(Window { utilization, tokens: None, resets_at })
}

struct NewCreds {
    access_token: String,
    refresh_token: String,
    expires_at_ms: i64,
}

fn refresh_token(refresh_token: &str, now_ms: i64) -> Result<NewCreds, String> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });
    let resp = http_client()?
        .post(TOKEN_URL)
        .header("anthropic-beta", OAUTH_BETA)
        .header("User-Agent", USER_AGENT)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("refresh HTTP {}", resp.status().as_u16()));
    }
    let v: Value = resp.json().map_err(|e| e.to_string())?;
    let access = v.get("access_token").and_then(Value::as_str).unwrap_or("").to_string();
    if access.is_empty() {
        return Err("refresh: no access_token".into());
    }
    let new_refresh = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or(refresh_token)
        .to_string();
    let expires_in = v.get("expires_in").and_then(Value::as_i64).unwrap_or(3600);
    Ok(NewCreds {
        access_token: access,
        refresh_token: new_refresh,
        expires_at_ms: now_ms + expires_in * 1000,
    })
}

/// Merge new tokens into the existing credentials JSON and write atomically,
/// preserving 0600 perms and every other field (scopes, tier, device token).
fn write_back(path: &Path, new: &NewCreds) -> Result<(), String> {
    let txt = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut v: Value = serde_json::from_str(&txt).map_err(|e| e.to_string())?;
    if let Some(o) = v.get_mut("claudeAiOauth").and_then(Value::as_object_mut) {
        o.insert("accessToken".into(), Value::String(new.access_token.clone()));
        o.insert("refreshToken".into(), Value::String(new.refresh_token.clone()));
        o.insert("expiresAt".into(), Value::Number(new.expires_at_ms.into()));
    }
    let serialized = serde_json::to_string(&v).map_err(|e| e.to_string())?;

    let dir: PathBuf = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let tmp = dir.join(".credentials.json.cctop.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        // Keep the credentials file owner-only on Unix; Windows inherits NTFS ACLs.
        #[cfg(unix)]
        f.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        f.write_all(serialized.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}
