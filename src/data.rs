//! Quota fetchers for Claude (OAuth usage API) and Codex (ChatGPT wham API).

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Window {
    pub used_pct: f64,
    /// Unix seconds.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stashes {
    pub available: Option<i64>,
    pub deadlines: Vec<Option<i64>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Provider {
    /// Original network fetch time; cache reads must not make old data look fresh.
    #[serde(default)]
    pub fetched_at: Option<i64>,
    pub plan: Option<String>,
    pub account: Option<String>,
    pub five_hour: Option<Window>,
    pub weekly: Option<Window>,
    /// Named quota pools for services whose limits differ from Claude/Codex.
    pub pools: Vec<(String, Window)>,
    /// Label/value rows shown under the bars (credits, resets available, ...).
    pub extras: Vec<(String, String)>,
    pub stashes: Option<Stashes>,
}

pub type Fetched = Result<Provider, String>;

const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn home(rel: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rel)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------- Claude ----------

fn claude_creds_path() -> PathBuf {
    home(".claude/.credentials.json")
}

fn read_json(path: &PathBuf) -> Result<Value, String> {
    let s = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))
}

/// Exchange the refresh token and persist the rotated pair, keeping other fields.
fn claude_refresh(agent: &ureq::Agent) -> Result<String, String> {
    let path = claude_creds_path();
    let mut creds = read_json(&path)?;
    let rt = creds["claudeAiOauth"]["refreshToken"]
        .as_str()
        .ok_or("no refresh token; run `claude` to log in")?
        .to_string();
    let mut resp = agent
        .post("https://api.anthropic.com/v1/oauth/token")
        .header("User-Agent", "claude-cli/1.0")
        .send_json(
            json!({"grant_type":"refresh_token","refresh_token":rt,"client_id":CLAUDE_CLIENT_ID}),
        )
        .map_err(|e| e.to_string())?;
    if resp.status() != 200 {
        return Err(format!("token refresh HTTP {}", resp.status()));
    }
    let body: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    let (at, nrt) = (
        body["access_token"].as_str(),
        body["refresh_token"].as_str(),
    );
    let (Some(at), Some(nrt)) = (at, nrt) else {
        return Err("token refresh: missing tokens".into());
    };
    // Re-read in case the CLI refreshed concurrently; don't clobber other fields.
    if let Ok(fresh) = read_json(&path) {
        creds = fresh;
    }
    creds["claudeAiOauth"]["accessToken"] = json!(at);
    creds["claudeAiOauth"]["refreshToken"] = json!(nrt);
    creds["claudeAiOauth"]["expiresAt"] =
        json!(now_ms() + body["expires_in"].as_i64().unwrap_or(28800) * 1000);
    fs::write(&path, serde_json::to_vec(&creds).unwrap()).map_err(|e| e.to_string())?;
    Ok(at.to_string())
}

fn claude_usage(
    agent: &ureq::Agent,
    token: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|e| e.to_string())
}

fn parse_iso(v: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|d| d.timestamp())
}

fn claude_window(v: &Value) -> Option<Window> {
    Some(Window {
        used_pct: v["utilization"].as_f64()?,
        resets_at: parse_iso(&v["resets_at"]),
    })
}

// No published OAuth usage polling limit. Twice Claude Code's reported five-minute
// cadence is a conservative baseline; this is separate from the public Admin API.
const CLAUDE_POLL_SECS: i64 = 600;

#[derive(Serialize, Deserialize)]
struct ClaudeCache {
    next_attempt_at: i64,
    rate_limits: u32,
    result: Fetched,
}

fn retry_after_secs(value: &str, now: i64) -> Option<i64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|s| *s >= 0)
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|date| date.timestamp().saturating_sub(now).max(0))
        })
}

fn write_claude_cache(path: &Path, cache: &ClaudeCache) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("Claude usage cache: {e}"))?;
    serde_json::to_writer(file, cache).map_err(|e| format!("Claude usage cache: {e}"))?;
    fs::rename(tmp, path).map_err(|e| format!("Claude usage cache: {e}"))
}

// Both binaries include this module. A persistent cache plus an OS file lock
// coordinates concurrent processes, restarts, and manual refreshes.
fn cached_claude(path: &Path, now: i64, fetch: impl FnOnce() -> (Fetched, Option<i64>)) -> Fetched {
    let dir = path.parent().ok_or("Claude usage cache has no directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("Claude usage cache: {e}"))?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))
        .map_err(|e| format!("Claude usage lock: {e}"))?;
    lock.lock().map_err(|e| format!("Claude usage lock: {e}"))?;
    let previous: Option<ClaudeCache> = match fs::read(path) {
        Ok(bytes) => {
            Some(serde_json::from_slice(&bytes).map_err(|e| format!("Claude usage cache: {e}"))?)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Claude usage cache: {e}")),
    };
    if let Some(cache) = &previous {
        if now < cache.next_attempt_at {
            return cache.result.clone();
        }
    }
    // Reserve before touching the network, so a crash cannot cause a retry burst.
    let mut cache = ClaudeCache {
        next_attempt_at: now.saturating_add(CLAUDE_POLL_SECS),
        rate_limits: previous.as_ref().map_or(0, |c| c.rate_limits),
        result: Err("Claude usage refresh interrupted; waiting for next scheduled attempt".into()),
    };
    write_claude_cache(path, &cache)?;
    let (mut result, retry_after) = fetch();
    if let Ok(provider) = &mut result {
        provider.fetched_at = Some(now);
    }
    if let Some(seconds) = retry_after {
        cache.rate_limits = cache.rate_limits.saturating_add(1);
        let fallback = (CLAUDE_POLL_SECS * 2_i64.pow(cache.rate_limits.min(3))).min(3600);
        let delay = fallback.max(seconds.saturating_mul(2));
        cache.next_attempt_at = now.saturating_add(delay);
        if let Err(error) = &mut result {
            *error = format!(
                "{error}; retry no earlier than {}",
                chrono::DateTime::from_timestamp(cache.next_attempt_at, 0)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "server cooldown ends".into())
            );
        }
    } else if result.is_ok() {
        cache.rate_limits = 0;
    }
    cache.result = result.clone();
    write_claude_cache(path, &cache)?;
    result
}

pub fn fetch_claude() -> Fetched {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home(".cache"));
    let path = base.join("ai-quota-bar/claude-usage.json");
    cached_claude(&path, chrono::Utc::now().timestamp(), || {
        let mut retry_after = None;
        let result = fetch_claude_uncached(&mut retry_after);
        (result, retry_after)
    })
}

fn fetch_claude_uncached(retry_after: &mut Option<i64>) -> Fetched {
    let agent = agent();
    let creds = read_json(&claude_creds_path())?;
    let oauth = &creds["claudeAiOauth"];
    let mut token = oauth["accessToken"].as_str().unwrap_or("").to_string();
    if token.is_empty() || oauth["expiresAt"].as_i64().unwrap_or(0) < now_ms() + 60_000 {
        token = claude_refresh(&agent)?;
    }
    let mut resp = claude_usage(&agent, &token)?;
    if resp.status() == 401 {
        token = claude_refresh(&agent)?;
        resp = claude_usage(&agent, &token)?;
    }
    if resp.status() == 429 {
        *retry_after = Some(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_after_secs(v, chrono::Utc::now().timestamp()))
                .unwrap_or(0),
        );
    }
    if resp.status() != 200 {
        return Err(format!("usage HTTP {}", resp.status()));
    }
    let v: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;

    let mut p = Provider {
        plan: oauth["subscriptionType"].as_str().map(title_case),
        five_hour: claude_window(&v["five_hour"]),
        weekly: claude_window(&v["seven_day"]),
        ..Default::default()
    };
    for (key, label) in [
        ("seven_day_opus", "Opus 7d"),
        ("seven_day_sonnet", "Sonnet 7d"),
    ] {
        if let Some(w) = claude_window(&v[key]) {
            p.extras
                .push((label.into(), format!("{:.0}% used", w.used_pct)));
        }
    }
    let x = &v["extra_usage"];
    if x["is_enabled"].as_bool() == Some(true) {
        let cur = x["currency"].as_str().unwrap_or("");
        let div = 10f64.powi(x["decimal_places"].as_i64().unwrap_or(2) as i32);
        let used = x["used_credits"].as_f64().unwrap_or(0.0) / div;
        let val = match x["monthly_limit"].as_f64() {
            Some(l) => format!("{used:.2} / {:.2} {cur}", l / div),
            None => format!("{used:.2} {cur}"),
        };
        p.extras.push(("Extra usage".into(), val));
    } else {
        p.extras.push(("Extra usage".into(), "off".into()));
    }
    Ok(p)
}

// ---------- Codex ----------

pub fn fetch_codex() -> Fetched {
    let agent = agent();
    let base = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home(".codex"));
    let auth =
        read_json(&base.join("auth.json")).map_err(|e| format!("{e} (run `codex login`)"))?;
    let tokens = &auth["tokens"];
    let token = tokens["access_token"]
        .as_str()
        .ok_or("no access token; run `codex login`")?;
    if let Some(exp) = jwt_exp(token) {
        if exp <= chrono::Utc::now().timestamp() + 30 {
            return Err("token expired; run `codex` once to refresh".into());
        }
    }
    let get = |path: &str| -> Result<Value, String> {
        let mut r = agent
            .get(&format!("https://chatgpt.com/backend-api/wham/{path}"))
            .header("Authorization", &format!("Bearer {token}"))
            .header(
                "ChatGPT-Account-Id",
                tokens["account_id"].as_str().unwrap_or(""),
            )
            .header("Referer", "https://chatgpt.com/")
            .header("oai-language", "en-US")
            .call()
            .map_err(|e| e.to_string())?;
        if r.status() != 200 {
            return Err(format!("{path} HTTP {}", r.status()));
        }
        r.body_mut().read_json().map_err(|e| e.to_string())
    };
    let v = get("usage")?;
    let win = |w: &Value| -> Option<Window> {
        Some(Window {
            used_pct: w["used_percent"].as_f64()?,
            resets_at: w["reset_at"].as_i64().or_else(|| {
                w["reset_after_seconds"]
                    .as_i64()
                    .map(|s| chrono::Utc::now().timestamp() + s)
            }),
        })
    };
    let rl = &v["rate_limit"];
    let mut p = Provider {
        plan: v["plan_type"].as_str().map(title_case),
        account: v["email"].as_str().map(str::to_string),
        five_hour: win(&rl["primary_window"]),
        weekly: win(&rl["secondary_window"]),
        ..Default::default()
    };
    let c = &v["credits"];
    let balance = match &c["balance"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "0".into(),
    };
    p.extras.push(if c["unlimited"].as_bool() == Some(true) {
        ("Credits".into(), "unlimited".into())
    } else {
        ("Credits".into(), format!("{balance} left"))
    });
    let details = get("rate-limit-reset-credits").ok();
    let available = details
        .as_ref()
        .and_then(|rc| rc["available_count"].as_i64())
        .or_else(|| v["rate_limit_reset_credits"]["available_count"].as_i64());
    if available.is_some() || details.is_some() {
        p.stashes = Some(Stashes {
            available,
            deadlines: stash_deadlines(details.as_ref().unwrap_or(&v["rate_limit_reset_credits"])),
        });
    }
    Ok(p)
}

fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = b64url_decode(payload)?;
    serde_json::from_slice::<Value>(&bytes).ok()?["exp"].as_i64()
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut buf, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn stash_deadlines(v: &Value) -> Vec<Option<i64>> {
    let mut deadlines: Vec<_> = v["credits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|credit| credit["status"].as_str() == Some("available"))
        .map(|credit| parse_iso(&credit["expires_at"]))
        .collect();
    deadlines.sort_by_key(|deadline| (deadline.is_none(), *deadline));
    deadlines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("claude-poll-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("usage.json")
    }

    #[test]
    fn claude_cache_is_shared_and_manual_refresh_cannot_bypass_ten_minutes() {
        let path = cache_path("shared");
        let mut calls = 0;
        let mut fetch = || {
            calls += 1;
            (
                Ok(Provider {
                    plan: Some("Max".into()),
                    ..Default::default()
                }),
                None,
            )
        };
        for now in [100, 100, 220, 699, 700] {
            let result = cached_claude(&path, now, &mut fetch).unwrap();
            assert_eq!(result.plan.as_deref(), Some("Max"));
            assert_eq!(result.fetched_at, Some(if now < 700 { 100 } else { 700 }));
        }
        assert_eq!(calls, 2);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn claude_429_waits_double_retry_after_and_survives_a_new_caller() {
        let path = cache_path("retry-after");
        let result = cached_claude(&path, 100, || (Err("usage HTTP 429".into()), Some(900)));
        assert!(result.unwrap_err().contains("429"));
        assert!(cached_claude(&path, 1899, || panic!("must honor persisted cooldown")).is_err());
        assert!(cached_claude(&path, 1900, || (Ok(Provider::default()), None)).is_ok());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn claude_429_without_a_useful_header_backs_off_then_recovers() {
        let path = cache_path("backoff");
        for now in [100, 1300] {
            assert!(cached_claude(&path, now, || (Err("usage HTTP 429".into()), Some(0))).is_err());
        }
        assert!(cached_claude(&path, 3699, || panic!("second cooldown is 40 minutes")).is_err());
        assert!(cached_claude(&path, 3700, || (Ok(Provider::default()), None)).is_ok());
        assert!(cached_claude(&path, 4299, || panic!("success restores ten minutes")).is_ok());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn simultaneous_claude_callers_make_only_one_request() {
        use std::sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        };
        let path = cache_path("concurrent");
        let calls = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    barrier.wait();
                    assert!(
                        cached_claude(&path, 100, || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            (Ok(Provider::default()), None)
                        })
                        .is_ok()
                    );
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        assert_eq!(retry_after_secs("900", 0), Some(900));
        assert_eq!(
            retry_after_secs("Thu, 01 Jan 1970 00:15:00 GMT", 100),
            Some(800)
        );
        assert_eq!(
            retry_after_secs("Thu, 01 Jan 1970 00:00:00 GMT", 100),
            Some(0)
        );
        assert_eq!(retry_after_secs("invalid", 100), None);
    }

    #[test]
    fn every_available_stash_keeps_its_own_deadline() {
        let result = stash_deadlines(&json!({"credits":[
            {"status":"available", "expires_at":"2026-11-20T12:00:00Z"},
            {"status":"consumed", "expires_at":"2026-10-30T12:00:00Z"},
            {"status":"available", "expires_at":"2026-10-31T12:00:00Z"},
            {"status":"available", "expires_at":"2026-10-31T12:00:00Z"},
            {"status":"available"}
        ]}));
        assert_eq!(result.len(), 4);
        assert!(result[0] < result[2]);
        assert_eq!(result[0], result[1], "two stashes can share a deadline");
        assert_eq!(result[3], None, "unknown deadlines must not be discarded");
    }
}
