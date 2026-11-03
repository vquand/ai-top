//! Explicit offline fixture, never substituted for unavailable live data.
use crate::{
    model::{Agent, Kind, Status},
    quota::{QuotaState, QuotaWindow},
    ui::App,
};
pub fn app(now: i64) -> App {
    let mut app = App::default();
    app.warnings = vec!["DEMO · illustrative data, not live sessions".into()];
    for (kind, id, parent, project, summary, status, seconds) in [
        (
            Kind::Claude,
            "c81a",
            None,
            "willLinuxSetup",
            "Updating AI quota monitor",
            Status::Running,
            522,
        ),
        (
            Kind::Claude,
            "c92b",
            Some("c81a"),
            "willLinuxSetup",
            "Checking provider quota parsing",
            Status::Running,
            477,
        ),
        (
            Kind::Claude,
            "c93c",
            Some("c81a"),
            "willLinuxSetup",
            "Reviewed configuration handling",
            Status::Finished,
            206,
        ),
        (
            Kind::Claude,
            "c14d",
            None,
            "willLinuxSetup",
            "Waiting for user input",
            Status::Paused,
            723,
        ),
        (
            Kind::Claude,
            "c44e",
            None,
            "underSiege",
            "Updating town layout",
            Status::Running,
            198,
        ),
        (
            Kind::Claude,
            "c45f",
            Some("c44e"),
            "underSiege",
            "Waiting for parent agent",
            Status::Paused,
            70,
        ),
        (
            Kind::Claude,
            "c46a",
            Some("c44e"),
            "underSiege",
            "Checked layout constraints",
            Status::Finished,
            48,
        ),
        (
            Kind::Codex,
            "x21a",
            None,
            "chassets",
            "Checking game assets",
            Status::Running,
            384,
        ),
        (
            Kind::Codex,
            "x22b",
            Some("x21a"),
            "chassets",
            "Validating model geometry",
            Status::Running,
            252,
        ),
        (
            Kind::Codex,
            "x23c",
            Some("x21a"),
            "chassets",
            "Checked texture dimensions",
            Status::Finished,
            96,
        ),
    ] {
        let end = (status != Status::Running).then_some(now - 60);
        app.agents.push(Agent {
            id: id.into(),
            kind,
            parent: parent.map(str::to_string),
            project: format!("~/Documents/code/{project}"),
            name: format!(
                "{}-{id}",
                if parent.is_some() {
                    "sub-agent"
                } else {
                    "agent"
                }
            ),
            summary: summary.into(),
            status,
            started_at: Some(end.unwrap_or(now) - seconds),
            ended_at: end,
            updated_at: now - seconds,
        });
    }
    app.quotas = Kind::ALL
        .into_iter()
        .enumerate()
        .map(|(i, kind)| QuotaState {
            kind,
            plan: Some(["Max", "Pro", "Google AI Pro (Jio)"][i].into()),
            windows: if i == 2 {
                vec![QuotaWindow {
                    label: "Gemini · 5 hour".into(),
                    used: Some(51.0),
                    resets_at: Some(now + 7500),
                }]
            } else {
                vec![
                    QuotaWindow {
                        label: "5 hour".into(),
                        used: Some([78.0, 32.0][i]),
                        resets_at: Some(now + 5040),
                    },
                    QuotaWindow {
                        label: "Weekly".into(),
                        used: Some([46.0, 18.0][i]),
                        resets_at: Some(now + 280000),
                    },
                ]
            },
            extras: if i == 2 {
                vec![("Claude / GPT".into(), "Not included in Jio plan".into())]
            } else {
                vec![]
            },
            error: None,
            updated_at: Some(now - 28),
            refreshing: false,
        })
        .collect();
    app
}
