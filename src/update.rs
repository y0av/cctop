//! Startup check for a newer release.
//!
//! Asks the GitHub releases API for the latest tag at most once a day (the
//! answer is cached in `~/.config/cctop/update`), and reports it only when it
//! parses as a higher version than this build. Nothing is downloaded and
//! nothing is installed — the header just grows a `↑ v0.4.2` marker, and
//! updating stays a deliberate re-run of the installer.
//!
//! Off under `--no-net`, `--demo`, `--no-update-check`, and whenever
//! `CCTOP_NO_UPDATE_CHECK` is set to anything but `0`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const LATEST_URL: &str = "https://api.github.com/repos/y0av/cctop/releases/latest";
const USER_AGENT: &str = concat!("cctop/", env!("CARGO_PKG_VERSION"), " (update-check)");
/// Seconds between network checks. A failed check backs off just as long, so a
/// machine that is simply offline never retries more than once a day either.
const CHECK_EVERY: i64 = 24 * 60 * 60;
const TIMEOUT: Duration = Duration::from_secs(5);

/// True when the user has opted out via the environment.
pub fn disabled_by_env() -> bool {
    match std::env::var("CCTOP_NO_UPDATE_CHECK") {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// The latest release tag when it is newer than this build, else `None`.
///
/// With `cache_only` the network is never touched — used by `--once`, which is
/// meant to be script-fast, so it reports whatever a previous TUI run cached.
pub fn check(cache_only: bool) -> Option<String> {
    let path = cache_path();
    let now = chrono::Local::now().timestamp();

    if let Some(p) = &path {
        if let Some((tag, checked)) = read_cache(p) {
            if cache_only || now - checked < CHECK_EVERY {
                return newer(&tag);
            }
        }
    }
    if cache_only {
        return None;
    }

    // An empty tag caches the failure, so an offline machine backs off a day
    // instead of paying the timeout on every launch.
    let tag = fetch().unwrap_or_default();
    if let Some(p) = &path {
        write_cache(p, &tag, now);
    }
    newer(&tag)
}

fn fetch() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .ok()?;
    let resp = client.get(LATEST_URL).header("Accept", "application/vnd.github+json").send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().ok()?;
    let tag = v.get("tag_name").and_then(Value::as_str)?;
    Some(tag.to_string())
}

/// `tag` if it parses as a strictly higher version than this build.
fn newer(tag: &str) -> Option<String> {
    let latest = parse(tag)?;
    let current = parse(env!("CARGO_PKG_VERSION"))?;
    (latest > current).then(|| tag.to_string())
}

/// `v1.2.3`, `1.2.3`, `1.2.3-rc1` → (1, 2, 3). Pre-release suffixes are
/// ignored: this only decides whether to show a marker.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor, patch))
}

fn cache_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("cctop").join("update"))
}

fn read_cache(p: &Path) -> Option<(String, i64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    let tag = v.get("tag").and_then(Value::as_str).unwrap_or("").to_string();
    let checked = v.get("checked").and_then(Value::as_i64)?;
    Some((tag, checked))
}

fn write_cache(p: &Path, tag: &str, now: i64) {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, format!(r#"{{"tag":"{tag}","checked":{now}}}"#));
}
