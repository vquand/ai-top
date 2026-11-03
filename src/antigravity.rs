//! Read the official CLI's quota-only report, including free account pools.
use crate::data::{Fetched, Provider, Window};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, symlink},
    path::PathBuf,
    process::{Command, Stdio},
};

pub fn fetch(jio_plan: bool) -> Fetched {
    let binary = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/agy");
    if !binary.is_file() {
        return Err("Install agy and sign in once to show Antigravity quota".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ai-quota-agy-{}-{stamp}", std::process::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| e.to_string())?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    // A passive refresh must never open an OAuth browser or update the CLI.
    for name in ["open", "xdg-open"] {
        symlink("/bin/false", dir.join(name)).map_err(|e| e.to_string())?;
    }
    let output = Command::new("/usr/bin/timeout")
        .args(["--kill-after=3s", "50s"])
        .arg(binary)
        .args([
            "-p",
            "/usage",
            "--output-format",
            "json",
            "--print-timeout",
            "45s",
        ])
        .current_dir(&dir)
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .env("AI_QUOTA_USAGE_PROBE", "1")
        .env("BROWSER", "/bin/false")
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Could not read Antigravity quota: {e}"))?;
    if !output.status.success() {
        return Err(
            "Antigravity quota unavailable; run agy in a terminal to check your login".into(),
        );
    }
    let value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Invalid Antigravity usage report")?;
    parse(&value, jio_plan)
}

fn parse(v: &Value, jio_plan: bool) -> Fetched {
    if v["status"] != "SUCCESS" || v["command"]["name"] != "usage" {
        return Err("Antigravity usage report was not successful".into());
    }
    let mut provider = Provider::default();
    if jio_plan {
        provider.plan = Some("Google AI Pro (Jio)".into());
        provider.extras.push(("Claude / GPT".into(), "Not included in Jio plan".into()));
    }
    for group in v["command"]["data"]["groups"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let name = group["name"].as_str().unwrap_or("Models");
        // /usage can report third-party quota even without model access. It
        // exposes no plan entitlement, so the Jio restriction is configured.
        let lower = name.to_ascii_lowercase();
        if jio_plan && (lower.contains("claude") || lower.contains("gpt")) {
            continue;
        }
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            if bucket["disabled"].as_bool() == Some(true) {
                continue;
            }
            let Some(left) = bucket["remaining_fraction"]
                .as_f64()
                .filter(|f| (0.0..=1.0).contains(f))
            else {
                continue;
            };
            let window = match bucket["window"].as_str() {
                Some("weekly") => "Weekly",
                Some("5h") | Some("five_hour") => "5 hour",
                _ => continue,
            };
            provider.pools.push((
                format!("{name} · {window}"),
                Window {
                    used_pct: (1.0 - left) * 100.0,
                    resets_at: bucket["reset_time"]
                        .as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|t| t.timestamp()),
                },
            ));
        }
    }
    if provider.pools.is_empty() {
        return Err("Antigravity did not return any quota pools for this account".into());
    }
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn free_account_weekly_pools_are_not_five_hour_limits() {
        let p = parse(&json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[
            {"name":"Gemini Models", "buckets":[{"window":"weekly", "remaining_fraction":0.75, "reset_time":"2026-11-05T15:32:29Z"}]},
            {"name":"Claude and GPT models", "buckets":[{"window":"weekly", "remaining_fraction":0.0}]}
        ]}}}), false).unwrap();
        assert!(p.five_hour.is_none());
        assert!(p.weekly.is_none());
        assert_eq!(p.pools.len(), 2);
        assert_eq!(p.pools[0].0, "Gemini Models · Weekly");
        assert_eq!(p.pools[0].1.used_pct, 25.0);
        assert!(p.pools[0].1.resets_at.is_some());
        assert_eq!(p.pools[1].1.used_pct, 100.0);
    }

    #[test]
    fn missing_or_invalid_quota_is_not_reported_as_full() {
        for value in [
            json!({"status":"ERROR"}),
            json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[]}}}),
            json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[{"name":"Gemini", "buckets":[{"remaining_fraction":2.0}]}]}}}),
        ] {
            assert!(parse(&value, false).is_err());
        }
    }

    #[test]
    fn jio_plan_excludes_third_party_pools_even_when_quota_remains() {
        let report = json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[
            {"name":"Gemini Models", "buckets":[
                {"window":"weekly", "remaining_fraction":0.75},
                {"window":"5h", "remaining_fraction":0.5}
            ]},
            {"name":"Claude and GPT models", "buckets":[
                {"window":"weekly", "remaining_fraction":0.99},
                {"window":"5h", "remaining_fraction":0.98}
            ]}
        ]}}});
        let p = parse(&report, true).unwrap();
        assert_eq!(p.plan.as_deref(), Some("Google AI Pro (Jio)"));
        assert_eq!(p.pools.len(), 2);
        assert_eq!(p.pools[0].0, "Gemini Models · Weekly");
        assert_eq!(p.pools[0].1.used_pct, 25.0);
        assert_eq!(p.pools[1].0, "Gemini Models · 5 hour");
        assert_eq!(p.pools[1].1.used_pct, 50.0);
        assert_eq!(p.extras, vec![("Claude / GPT".into(), "Not included in Jio plan".into())]);
        assert_eq!(parse(&report, false).unwrap().pools.len(), 4,
            "other accounts must retain the CLI's third-party pools");
    }

    #[test]
    fn jio_plan_without_third_party_pools_still_explains_the_restriction() {
        let p = parse(&json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[
            {"name":"Gemini Models", "buckets":[{"window":"5h", "remaining_fraction":0.0}]}
        ]}}}), true).unwrap();
        assert_eq!(p.pools.len(), 1);
        assert_eq!(p.pools[0].1.used_pct, 100.0,
            "exhausted Gemini quota must still show as exhausted");
        assert_eq!(p.extras.len(), 1);
        assert!(parse(&json!({"status":"SUCCESS", "command":{"name":"usage", "data":{"groups":[
            {"name":"Claude and GPT models", "buckets":[{"window":"weekly", "remaining_fraction":1.0}]}
        ]}}}), true).is_err(), "third-party quota cannot substitute for missing Gemini quota");
    }
}
