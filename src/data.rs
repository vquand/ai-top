//! Quota fetchers for Claude (OAuth usage API) and Codex (ChatGPT wham API).

use chrono::DateTime;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};

#[derive(Clone, Debug)]
pub struct Window {
    pub used_pct: f64,
    /// Unix seconds.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct Stashes {
    pub available: Option<i64>,
    pub deadlines: Vec<Option<i64>>,
}

#[derive(Clone, Debug, Default)]
pub struct Provider {
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
        .send_json(json!({"grant_type":"refresh_token","refresh_token":rt,"client_id":CLAUDE_CLIENT_ID}))
        .map_err(|e| e.to_string())?;
    if resp.status() != 200 {
        return Err(format!("token refresh HTTP {}", resp.status()));
    }
    let body: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    let (at, nrt) = (body["access_token"].as_str(), body["refresh_token"].as_str());
    let (Some(at), Some(nrt)) = (at, nrt) else {
        return Err("token refresh: missing tokens".into());
    };
    // Re-read in case the CLI refreshed concurrently; don't clobber other fields.
    if let Ok(fresh) = read_json(&path) {
        creds = fresh;
    }
    creds["claudeAiOauth"]["accessToken"] = json!(at);
    creds["claudeAiOauth"]["refreshToken"] = json!(nrt);
    creds["claudeAiOauth"]["expiresAt"] = json!(now_ms() + body["expires_in"].as_i64().unwrap_or(28800) * 1000);
    fs::write(&path, serde_json::to_vec(&creds).unwrap()).map_err(|e| e.to_string())?;
    Ok(at.to_string())
}

fn claude_usage(agent: &ureq::Agent, token: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|e| e.to_string())
}

fn parse_iso(v: &Value) -> Option<i64> {
    DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|d| d.timestamp())
}

fn claude_window(v: &Value) -> Option<Window> {
    Some(Window { used_pct: v["utilization"].as_f64()?, resets_at: parse_iso(&v["resets_at"]) })
}

pub fn fetch_claude() -> Fetched {
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
    for (key, label) in [("seven_day_opus", "Opus 7d"), ("seven_day_sonnet", "Sonnet 7d")] {
        if let Some(w) = claude_window(&v[key]) {
            p.extras.push((label.into(), format!("{:.0}% used", w.used_pct)));
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
    let base = std::env::var("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|_| home(".codex"));
    let auth = read_json(&base.join("auth.json")).map_err(|e| format!("{e} (run `codex login`)"))?;
    let tokens = &auth["tokens"];
    let token = tokens["access_token"].as_str().ok_or("no access token; run `codex login`")?;
    if let Some(exp) = jwt_exp(token) {
        if exp <= chrono::Utc::now().timestamp() + 30 {
            return Err("token expired; run `codex` once to refresh".into());
        }
    }
    let get = |path: &str| -> Result<Value, String> {
        let mut r = agent
            .get(&format!("https://chatgpt.com/backend-api/wham/{path}"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("ChatGPT-Account-Id", tokens["account_id"].as_str().unwrap_or(""))
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
                w["reset_after_seconds"].as_i64().map(|s| chrono::Utc::now().timestamp() + s)
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
    let available = details.as_ref().and_then(|rc| rc["available_count"].as_i64())
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
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn stash_deadlines(v: &Value) -> Vec<Option<i64>> {
    let mut deadlines: Vec<_> = v["credits"].as_array().into_iter().flatten()
        .filter(|credit| credit["status"].as_str() == Some("available"))
        .map(|credit| parse_iso(&credit["expires_at"]))
        .collect();
    deadlines.sort_by_key(|deadline| (deadline.is_none(), *deadline));
    deadlines
}

#[cfg(test)]
mod tests {
    use super::*;

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
