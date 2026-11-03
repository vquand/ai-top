use crate::{
    data::Fetched,
    model::{Kind, elapsed},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub label: String,
    pub used: Option<f64>,
    pub resets_at: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaState {
    pub kind: Kind,
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub extras: Vec<(String, String)>,
    pub error: Option<String>,
    pub updated_at: Option<i64>,
    pub refreshing: bool,
}
impl QuotaState {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            plan: None,
            windows: vec![],
            extras: vec![],
            error: None,
            updated_at: None,
            refreshing: true,
        }
    }
    pub fn update(&mut self, result: Fetched, now: i64) {
        self.refreshing = false;
        match result {
            Ok(p) => {
                self.error = None;
                self.updated_at = Some(now);
                self.plan = p.plan;
                self.extras = p.extras;
                self.windows = if p.pools.is_empty() {
                    vec![
                        ("5 hour".to_string(), p.five_hour),
                        ("Weekly".to_string(), p.weekly),
                    ]
                    .into_iter()
                    .map(|(label, w)| QuotaWindow {
                        label,
                        used: w.as_ref().map(|w| w.used_pct),
                        resets_at: w.and_then(|w| w.resets_at),
                    })
                    .collect()
                } else {
                    p.pools
                        .into_iter()
                        .map(|(label, w)| QuotaWindow {
                            label,
                            used: Some(w.used_pct),
                            resets_at: w.resets_at,
                        })
                        .collect()
                };
                if let Some(stashes) = p.stashes {
                    if let Some(n) = stashes.available {
                        self.extras.push(("Reset stashes".into(), n.to_string()));
                    }
                    for at in stashes.deadlines {
                        self.extras.push((
                            "Stash expires".into(),
                            at.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                                .map(|t| {
                                    t.with_timezone(&chrono::Local)
                                        .format("%b %d %H:%M")
                                        .to_string()
                                })
                                .unwrap_or_else(|| "unknown".into()),
                        ));
                    }
                }
            }
            Err(e) => self.error = Some(e),
        }
    }
    pub fn freshness(&self, now: i64) -> String {
        let age = self
            .updated_at
            .map(|t| format!("{} ago", elapsed(now - t)))
            .unwrap_or_else(|| "no data".into());
        format!(
            "{}{}",
            if self.refreshing {
                "refreshing · "
            } else if self.error.is_some() {
                "unavailable · "
            } else {
                "updated "
            },
            age
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Provider, Window};
    #[test]
    fn failed_refresh_retains_last_quota_with_error_and_original_age() {
        let mut q = QuotaState::new(Kind::Codex);
        q.update(
            Ok(Provider {
                five_hour: Some(Window {
                    used_pct: 42.0,
                    resets_at: Some(300),
                }),
                ..Default::default()
            }),
            100,
        );
        q.update(Err("offline".into()), 150);
        assert_eq!(q.windows[0].used, Some(42.0));
        assert_eq!(q.updated_at, Some(100));
        assert!(q.freshness(160).contains("unavailable"));
    }
}
