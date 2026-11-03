use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
    Antigravity,
}
impl Kind {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Antigravity];
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Antigravity => "Antigravity",
        }
    }
    // Single-cell provider badges work without image protocols or patched fonts.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Claude => "✳",
            Self::Codex => "◎",
            Self::Antigravity => "◆",
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Paused,
    Finished,
    Failed,
    Cancelled,
    Exited,
    Unknown,
}
impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Working",
            Self::Paused => "Paused",
            Self::Finished => "Finished",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Exited => "Exited",
            Self::Unknown => "Unknown",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Running => "●",
            Self::Paused => "Ⅱ",
            Self::Finished => "✓",
            Self::Failed => "!",
            Self::Cancelled => "×",
            Self::Exited => "○",
            Self::Unknown => "?",
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Agent {
    pub id: String,
    pub parent: Option<String>,
    pub kind: Kind,
    pub project: String,
    pub name: String,
    pub summary: String,
    pub status: Status,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub updated_at: i64,
}
impl Agent {
    pub fn timing(&self, now: i64) -> String {
        let end = self
            .ended_at
            .or_else(|| matches!(self.status, Status::Running).then_some(now));
        let duration = self
            .started_at
            .zip(end)
            .map(|(s, e)| elapsed(e.saturating_sub(s)));
        let mut value = format!("{} {}", self.status.glyph(), self.status.label());
        if let Some(d) = duration {
            value.push_str(&format!(
                " {}{d}",
                if matches!(
                    self.status,
                    Status::Finished | Status::Failed | Status::Cancelled | Status::Exited
                ) {
                    "after "
                } else if self.status == Status::Paused {
                    "· worked "
                } else {
                    ""
                }
            ));
        }
        if let Some(at) = self
            .ended_at
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        {
            value.push_str(&format!(
                " at {}",
                at.with_timezone(&chrono::Local).format("%H:%M")
            ));
        }
        value
    }
}
pub fn elapsed(seconds: i64) -> String {
    let s = seconds.max(0);
    if s >= 86400 {
        format!("{}d{}h", s / 86400, s % 86400 / 3600)
    } else if s >= 3600 {
        format!("{}h{}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}
/// Untrusted local metadata must never inject terminal control sequences.
pub fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
        })
        .take(180)
        .collect()
}
#[derive(Clone, Copy, Debug)]
pub struct TreeRow<'a> {
    pub agent: &'a Agent,
    pub depth: usize,
}
#[derive(Debug)]
pub struct Project<'a> {
    pub path: String,
    pub rows: Vec<TreeRow<'a>>,
}
impl Project<'_> {
    pub fn hidden_finished(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.agent.parent.is_some() && r.agent.status == Status::Finished)
            .count()
            .saturating_sub(5)
    }

    pub fn visible_rows(&self, expanded: bool) -> Vec<TreeRow<'_>> {
        if expanded || self.hidden_finished() == 0 {
            return self.rows.clone();
        }
        let mut finished: Vec<_> = self
            .rows
            .iter()
            .filter(|r| r.agent.parent.is_some() && r.agent.status == Status::Finished)
            .collect();
        finished.sort_by_key(|r| {
            (
                std::cmp::Reverse(r.agent.ended_at.unwrap_or(r.agent.updated_at)),
                r.agent.kind,
                r.agent.id.as_str(),
            )
        });
        let hidden: HashSet<_> = finished
            .into_iter()
            .skip(5)
            .map(|r| (r.agent.kind, r.agent.id.as_str()))
            .collect();
        // Keep descendants of hidden parents visible at the nearest visible ancestor.
        let mut depths = vec![0];
        let mut rows = vec![];
        for row in &self.rows {
            depths.resize(row.depth + 2, 0);
            let depth = depths[row.depth];
            let visible = !hidden.contains(&(row.agent.kind, row.agent.id.as_str()));
            depths[row.depth + 1] = depth + usize::from(visible);
            if visible {
                rows.push(TreeRow {
                    agent: row.agent,
                    depth,
                });
            }
        }
        rows
    }
}
pub fn projects(agents: &[Agent], filter: Option<Kind>) -> Vec<Project<'_>> {
    let active: HashSet<_> = agents
        .iter()
        .filter(|a| a.status == Status::Running)
        .map(|a| a.project.as_str())
        .collect();
    let mut groups: BTreeMap<&str, Vec<&Agent>> = BTreeMap::new();
    for a in agents {
        if active.contains(a.project.as_str()) && filter.is_none_or(|k| a.kind == k) {
            groups.entry(&a.project).or_default().push(a);
        }
    }
    groups
        .into_iter()
        .map(|(path, mut entries)| {
            entries.sort_by(|a, b| {
                a.status
                    .cmp(&b.status)
                    .then_with(|| b.updated_at.cmp(&a.updated_at))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let mut visited = HashSet::new();
            let mut rows = vec![];
            fn visit<'a>(
                a: &'a Agent,
                depth: usize,
                entries: &[&'a Agent],
                seen: &mut HashSet<(Kind, &'a str)>,
                rows: &mut Vec<TreeRow<'a>>,
            ) {
                if !seen.insert((a.kind, &a.id)) {
                    return;
                }
                rows.push(TreeRow { agent: a, depth });
                for child in entries
                    .iter()
                    .filter(|c| c.parent.as_deref() == Some(a.id.as_str()))
                {
                    visit(child, depth + 1, entries, seen, rows);
                }
            }
            for a in &entries {
                if a.parent
                    .as_ref()
                    .is_none_or(|parent| !entries.iter().any(|p| p.id == *parent))
                {
                    visit(a, 0, &entries, &mut visited, &mut rows);
                }
            }
            for a in &entries {
                visit(a, 0, &entries, &mut visited, &mut rows);
            }
            Project {
                path: path.into(),
                rows,
            }
        })
        .collect()
}
