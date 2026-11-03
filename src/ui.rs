use crate::{
    model::{Agent, Kind, Status, clean, elapsed, projects},
    quota::QuotaState,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use std::collections::HashSet;
const TEXT: Color = Color::Rgb(217, 225, 232);
const MUTED: Color = Color::Rgb(154, 169, 184);
const BORDER: Color = Color::Rgb(56, 67, 79);
const BG: Color = Color::Rgb(16, 20, 25);
pub fn color(k: Kind) -> Color {
    match k {
        Kind::Claude => Color::Rgb(239, 170, 128),
        Kind::Codex => Color::Rgb(128, 207, 172),
        Kind::Antigravity => Color::Rgb(142, 186, 255),
    }
}
fn style_status(s: Status) -> Style {
    Style::default().fg(match s {
        Status::Running => Color::Rgb(128, 207, 172),
        Status::Paused => Color::Rgb(233, 204, 126),
        Status::Failed => Color::LightRed,
        _ => MUTED,
    })
}
fn text(s: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(c))
}
fn width(s: &str) -> usize {
    Line::from(s).width()
}
pub fn clip(s: &str, limit: usize) -> String {
    let safe = clean(s);
    if width(&safe) <= limit {
        return safe;
    }
    if limit == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in safe.chars() {
        let n = Span::raw(c.to_string()).width();
        if used + n >= limit {
            break;
        }
        out.push(c);
        used += n;
    }
    out.push('…');
    out
}
fn pad(s: &str, limit: usize) -> String {
    let s = clip(s, limit);
    format!("{s}{}", " ".repeat(limit.saturating_sub(width(&s))))
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Providers,
    Projects,
}
pub struct App {
    pub agents: Vec<Agent>,
    pub quotas: Vec<QuotaState>,
    pub warnings: Vec<String>,
    pub selected: Option<Kind>,
    pub focus: Focus,
    pub remaining: bool,
    pub help: bool,
    pub scroll: u16,
    pub sidebar_scroll: u16,
    pub project_cursor: usize,
    collapsed: HashSet<String>,
    expanded_history: HashSet<String>,
    history_selected: bool,
    pub history_hits: Vec<(String, Rect)>,
    history_offsets: Vec<(String, u16)>,
    pub provider_hits: Vec<(Option<Kind>, Rect)>,
    pub project_hits: Vec<(String, Rect)>,
    pub sidebar_area: Rect,
    pub tree_area: Rect,
    tree_len: u16,
    project_offsets: Vec<(String, u16)>,
    reveal_provider: bool,
    sidebar_len: u16,
}
impl Default for App {
    fn default() -> Self {
        Self {
            agents: vec![],
            quotas: Kind::ALL.into_iter().map(QuotaState::new).collect(),
            warnings: vec![],
            selected: None,
            focus: Focus::Providers,
            remaining: false,
            help: false,
            scroll: 0,
            sidebar_scroll: 0,
            project_cursor: 0,
            collapsed: HashSet::new(),
            expanded_history: HashSet::new(),
            history_selected: false,
            history_hits: vec![],
            history_offsets: vec![],
            provider_hits: vec![],
            project_hits: vec![],
            sidebar_area: Rect::default(),
            tree_area: Rect::default(),
            tree_len: 0,
            project_offsets: vec![],
            reveal_provider: false,
            sidebar_len: 0,
        }
    }
}
impl App {
    pub fn toggle_provider(&mut self, k: Kind) {
        self.select(if self.selected == Some(k) {
            None
        } else {
            Some(k)
        })
    }
    pub fn select(&mut self, k: Option<Kind>) {
        self.selected = k;
        self.scroll = 0;
        self.project_cursor = 0;
        self.history_selected = false;
        self.reveal_provider = true;
    }
    pub fn move_selection(&mut self, delta: i32) {
        match self.focus {
            Focus::Providers => {
                let i = self
                    .selected
                    .map(|k| Kind::ALL.iter().position(|p| *p == k).unwrap() + 1)
                    .unwrap_or(0) as i32;
                let next = (i + delta).rem_euclid(4) as usize;
                self.select(if next == 0 {
                    None
                } else {
                    Some(Kind::ALL[next - 1])
                });
                self.sidebar_scroll = 0;
            }
            Focus::Projects => {
                let groups = projects(&self.agents, self.selected);
                let targets: Vec<_> = groups
                    .iter()
                    .enumerate()
                    .flat_map(|(index, p)| {
                        let mut targets = vec![(index, false)];
                        if p.hidden_finished() > 0 && !self.collapsed.contains(&p.path) {
                            targets.push((index, true));
                        }
                        targets
                    })
                    .collect();
                if !targets.is_empty() {
                    let current = targets
                        .iter()
                        .position(|t| *t == (self.project_cursor, self.history_selected))
                        .unwrap_or(0);
                    let next = (current as i32 + delta).clamp(0, targets.len() as i32 - 1) as usize;
                    (self.project_cursor, self.history_selected) = targets[next];
                    let target = &groups[self.project_cursor].path;
                    let offsets = if self.history_selected {
                        &self.history_offsets
                    } else {
                        &self.project_offsets
                    };
                    if let Some((_, offset)) = offsets.iter().find(|(p, _)| p == target) {
                        if *offset < self.scroll {
                            self.scroll = *offset;
                        } else if *offset >= self.scroll.saturating_add(self.tree_area.height) {
                            self.scroll = offset.saturating_sub(self.tree_area.height / 3);
                        }
                    }
                }
            }
        }
    }
    pub fn toggle_project(&mut self) {
        let groups = projects(&self.agents, self.selected);
        if let Some(p) = groups.get(self.project_cursor) {
            if self.history_selected && p.hidden_finished() > 0 {
                if !self.expanded_history.remove(&p.path) {
                    self.expanded_history.insert(p.path.clone());
                }
            } else if !self.collapsed.remove(&p.path) {
                self.collapsed.insert(p.path.clone());
            }
        }
    }

    pub fn page(&mut self, down: bool) {
        let (scroll, amount) = match self.focus {
            Focus::Providers => (
                &mut self.sidebar_scroll,
                self.sidebar_area.height.saturating_sub(2),
            ),
            Focus::Projects => (&mut self.scroll, self.tree_area.height.saturating_sub(2)),
        };
        *scroll = if down {
            scroll.saturating_add(amount)
        } else {
            scroll.saturating_sub(amount)
        };
    }
    pub fn click(&mut self, x: u16, y: u16) {
        if let Some((k, _)) = self
            .provider_hits
            .iter()
            .find(|(_, r)| r.contains((x, y).into()))
        {
            let k = *k;
            self.focus = Focus::Providers;
            if let Some(k) = k {
                self.toggle_provider(k)
            } else {
                self.select(None)
            }
            return;
        }
        if let Some((path, _)) = self
            .history_hits
            .iter()
            .find(|(_, r)| r.contains((x, y).into()))
        {
            let path = path.clone();
            self.focus = Focus::Projects;
            self.project_cursor = projects(&self.agents, self.selected)
                .iter()
                .position(|p| p.path == path)
                .unwrap_or(0);
            self.history_selected = true;
            if !self.expanded_history.remove(&path) {
                self.expanded_history.insert(path);
            }
            return;
        }
        if let Some((path, _)) = self
            .project_hits
            .iter()
            .find(|(_, r)| r.contains((x, y).into()))
        {
            let path = path.clone();
            self.focus = Focus::Projects;
            self.history_selected = false;
            self.project_cursor = projects(&self.agents, self.selected)
                .iter()
                .position(|p| p.path == path)
                .unwrap_or(0);
            if !self.collapsed.remove(&path) {
                self.collapsed.insert(path);
            }
        }
    }
    pub fn draw(&mut self, f: &mut Frame, now: i64) {
        let area = f.area();
        f.render_widget(Block::new().style(Style::default().fg(TEXT).bg(BG)), area);
        if area.width < 60 || area.height < 12 {
            f.render_widget(
                Paragraph::new("ai-top\nResize terminal to at least 60 × 12.\nq quit")
                    .style(Style::default().fg(TEXT)),
                area,
            );
            return;
        }
        let vertical = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(if self.warnings.is_empty() { 1 } else { 2 }),
        ])
        .split(area);
        let running = self
            .agents
            .iter()
            .filter(|a| a.status == Status::Running)
            .count();
        let paused = self
            .agents
            .iter()
            .filter(|a| a.status == Status::Paused)
            .count();
        f.render_widget(
            Paragraph::new(Line::from(vec![
                text(" ai-top", TEXT).add_modifier(Modifier::BOLD),
                text(
                    format!(" / local AI monitor   ● {running} working · {paused} paused"),
                    MUTED,
                ),
            ])),
            vertical[0],
        );
        let sidebar_width = if area.width >= 110 { 34 } else { 27 };
        let columns = Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(20)])
            .split(vertical[1]);
        self.sidebar(f, columns[0], now);
        self.tree(f, columns[1], now);
        let mut footer = vec![Line::from(text(
            " Tab focus  ↑↓ select  Enter toggle  A/Esc all  PgUp/Dn scroll  U used/left  R refresh  ? help  Q quit",
            MUTED,
        ))];
        if !self.warnings.is_empty() {
            footer.push(Line::from(text(
                format!(" ! {}", clean(&self.warnings.join(" · "))),
                Color::Yellow,
            )));
        }
        f.render_widget(Paragraph::new(footer), vertical[2]);
        if self.help {
            let r = Rect::new(
                area.x + area.width / 10,
                area.y + area.height / 6,
                area.width * 8 / 10,
                area.height * 2 / 3,
            );
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new("A or Esc: all providers. 1/2/3: provider.\nTab / Left / Right: switch panel focus.\nUp / Down or J / K: provider / project selection.\nEnter / Space: clear provider filter / collapse project.\nPageUp / PageDown: scroll focused panel. Home / End: top / bottom.\nMouse: provider filter or project collapse; wheel: scroll.\nU: quota used / remaining. R: refresh quotas. Q / Ctrl-C: quit.\n\nRunning agents first among siblings; hierarchy stays intact.\nFive newest finished sub-agents per project; select … and Enter to show more.\nFinished and paused sessions remain in active projects.\nProvider badges: ✳ Claude  ◎ Codex  ◆ Antigravity.\nNo prompts or conversation bodies are displayed.\n\nPress ? or Esc to close help.").block(Block::bordered().title(" ai-top help ").border_style(Style::default().fg(MUTED))).style(Style::default().fg(TEXT).bg(BG)),r);
        }
    }
    fn sidebar(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let block = Block::bordered()
            .title(if self.remaining {
                " Providers / remaining "
            } else {
                " Providers / used "
            })
            .border_style(Style::default().fg(if self.focus == Focus::Providers {
                MUTED
            } else {
                BORDER
            }));
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.sidebar_area = inner;
        let w = inner.width as usize;
        let mut lines = vec![];
        let mut ranges = vec![];
        lines.push(Line::from(text(
            format!(
                "{} All providers",
                if self.selected.is_none() { "▶" } else { " " }
            ),
            TEXT,
        )));
        ranges.push((None, 0, 1));
        lines.push(Line::default());
        for q in &self.quotas {
            let start = lines.len();
            let k = q.kind;
            let active = self
                .agents
                .iter()
                .filter(|a| a.kind == k && a.status == Status::Running)
                .count();
            lines.push(Line::from(text(
                format!(
                    "{} {} {}  {}",
                    if self.selected == Some(k) { "▶" } else { " " },
                    k.icon(),
                    k.name(),
                    if active > 0 {
                        format!("●{active}")
                    } else {
                        "idle".into()
                    }
                ),
                color(k),
            )));
            if let Some(plan) = &q.plan {
                lines.push(Line::from(text(
                    format!("  {}", clip(plan, w.saturating_sub(2))),
                    MUTED,
                )));
            }
            if q.windows.is_empty() {
                lines.push(Line::from(text(
                    if q.refreshing {
                        "  Loading quota…"
                    } else {
                        "  Quota unavailable"
                    },
                    MUTED,
                )));
            }
            for window in &q.windows {
                lines.push(Line::from(text(
                    format!("  {}", clip(&window.label, w.saturating_sub(2))),
                    MUTED,
                )));
                let n = window
                    .used
                    .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
                    .map(|n| if self.remaining { 100.0 - n } else { n });
                let bars = w.saturating_sub(10);
                let used = n
                    .map(|n| (n / 100.0 * bars as f64).round() as usize)
                    .unwrap_or(0)
                    .min(bars);
                lines.push(Line::from(vec![
                    text(format!("  {}", "━".repeat(used)), color(k)),
                    text("─".repeat(bars - used), BORDER),
                    text(
                        n.map(|n| format!(" {n:3.0}%"))
                            .unwrap_or_else(|| "  -- ".into()),
                        TEXT,
                    ),
                ]));
                if let Some(reset) = window.resets_at {
                    lines.push(Line::from(text(
                        format!("  reset in {}", elapsed(reset - now)),
                        MUTED,
                    )));
                }
            }
            for (label, value) in &q.extras {
                lines.push(Line::from(text(
                    format!(
                        "  {}",
                        clip(&format!("{label}: {value}"), w.saturating_sub(2))
                    ),
                    MUTED,
                )));
            }
            lines.push(Line::from(text(
                format!("  {}", clip(&q.freshness(now), w.saturating_sub(2))),
                MUTED,
            )));
            if let Some(error) = &q.error {
                for part in wrap(&format!("! {error}"), w.saturating_sub(2)) {
                    lines.push(Line::from(text(format!("  {part}"), Color::Yellow)));
                }
            }
            ranges.push((Some(k), start, lines.len()));
            lines.push(Line::from(text("─".repeat(w), BORDER)));
        }
        self.sidebar_len = lines.len().min(u16::MAX as usize) as u16;
        if self.reveal_provider {
            if let Some((_, start, end)) = ranges.iter().find(|(kind, _, _)| *kind == self.selected)
            {
                let start = *start as u16;
                let end = *end as u16;
                if start < self.sidebar_scroll {
                    self.sidebar_scroll = start;
                } else if end > self.sidebar_scroll.saturating_add(inner.height) {
                    self.sidebar_scroll = if end - start <= inner.height {
                        end.saturating_sub(inner.height)
                    } else {
                        start
                    };
                }
            }
            self.reveal_provider = false;
        }
        self.sidebar_scroll = self
            .sidebar_scroll
            .min(self.sidebar_len.saturating_sub(inner.height));
        self.provider_hits = ranges
            .into_iter()
            .filter_map(|(k, start, end)| {
                hit(inner, self.sidebar_scroll, start, end).map(|r| (k, r))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).scroll((self.sidebar_scroll, 0)),
            inner,
        );
    }
    fn tree(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let title = self.selected.map(|k| k.name()).unwrap_or("All providers");
        let block = Block::bordered()
            .title(format!(" Agents / {title} "))
            .border_style(Style::default().fg(if self.focus == Focus::Projects {
                MUTED
            } else {
                BORDER
            }));
        let inner = block.inner(area);
        f.render_widget(block, area);
        self.tree_area = inner;
        let w = inner.width as usize;
        let compact = w >= 105;
        let groups = projects(&self.agents, self.selected);
        self.project_cursor = self.project_cursor.min(groups.len().saturating_sub(1));
        let mut lines = vec![
            Line::from(text(
                format!(
                    " {} active projects · {} rows · running first",
                    groups.len(),
                    if compact { "compact" } else { "two-line" }
                ),
                MUTED,
            )),
            Line::default(),
        ];
        let mut ranges = vec![];
        let mut history_ranges = vec![];
        if groups.is_empty() {
            lines.push(Line::from(text(
                " No projects with verified running agents.",
                MUTED,
            )));
            lines.push(Line::from(text(
                " Quotas update independently. R refresh · ? help",
                MUTED,
            )));
        }
        for (index, project) in groups.iter().enumerate() {
            let row = lines.len();
            let closed = self.collapsed.contains(&project.path);
            let name = std::path::Path::new(&project.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&project.path);
            lines.push(Line::from(
                text(
                    format!(
                        " {} {} {}",
                        if self.focus == Focus::Projects
                            && index == self.project_cursor
                            && !self.history_selected
                        {
                            "▶"
                        } else {
                            " "
                        },
                        if closed { "▸" } else { "▾" },
                        clip(name, w.saturating_sub(6))
                    ),
                    TEXT,
                )
                .add_modifier(Modifier::BOLD),
            ));
            ranges.push((project.path.clone(), row, row + 1));
            lines.push(Line::from(text(
                format!("     {}", clip(&project.path, w.saturating_sub(5))),
                MUTED,
            )));
            if !closed {
                let expanded = self.expanded_history.contains(&project.path);
                for row in project.visible_rows(expanded) {
                    let a = row.agent;
                    let indent = "│  ".repeat(row.depth.min(8));
                    let branch = if row.depth > 0 { "├─ " } else { "" };
                    let prefix = format!(" {indent}{branch}{} ", a.kind.icon());
                    let prefix_width = width(&prefix);
                    let avail = w.saturating_sub(prefix_width);
                    let status = clean(&a.timing(now));
                    let name = clean(&a.name);
                    if compact {
                        let timing_width = width(&status).min(avail / 2);
                        let name_width = 22.min(avail.saturating_sub(timing_width + 4));
                        let work_width = avail.saturating_sub(name_width + timing_width + 4);
                        lines.push(Line::from(vec![
                            text(prefix, color(a.kind)),
                            text(pad(&name, name_width), TEXT),
                            text("  ", TEXT),
                            text(pad(&a.summary, work_width), MUTED),
                            text("  ", TEXT),
                            Span::styled(clip(&status, timing_width), style_status(a.status)),
                        ]));
                    } else {
                        let timing_width = width(&status).min(avail.saturating_sub(14));
                        let name_width = avail.saturating_sub(timing_width + 1);
                        lines.push(Line::from(vec![
                            text(prefix.clone(), color(a.kind)),
                            text(pad(&name, name_width), TEXT),
                            text(" ", TEXT),
                            Span::styled(clip(&status, timing_width), style_status(a.status)),
                        ]));
                        lines.push(Line::from(text(
                            format!("{}{}", " ".repeat(prefix_width), clip(&a.summary, avail)),
                            MUTED,
                        )));
                    }
                }
                if project.hidden_finished() > 0 {
                    let start = lines.len();
                    let label = if expanded {
                        "Show fewer finished sub-agents".to_string()
                    } else {
                        format!("… {} more finished sub-agents", project.hidden_finished())
                    };
                    let selected = self.focus == Focus::Projects
                        && index == self.project_cursor
                        && self.history_selected;
                    lines.push(Line::from(text(
                        clip(
                            &format!(
                                " {} {label} (Enter / click)",
                                if selected { "▶" } else { " " }
                            ),
                            w,
                        ),
                        if selected { TEXT } else { MUTED },
                    )));
                    history_ranges.push((project.path.clone(), start, start + 1));
                }
            }
            lines.push(Line::default());
        }
        self.tree_len = lines.len().min(u16::MAX as usize) as u16;
        self.scroll = self.scroll.min(self.tree_len.saturating_sub(inner.height));
        self.project_offsets = ranges
            .iter()
            .map(|(p, start, _)| (p.clone(), (*start).min(u16::MAX as usize) as u16))
            .collect();
        self.history_offsets = history_ranges
            .iter()
            .map(|(p, start, _)| (p.clone(), (*start).min(u16::MAX as usize) as u16))
            .collect();
        self.history_hits = history_ranges
            .into_iter()
            .filter_map(|(p, start, end)| hit(inner, self.scroll, start, end).map(|r| (p, r)))
            .collect();
        self.project_hits = ranges
            .into_iter()
            .filter_map(|(p, start, end)| hit(inner, self.scroll, start, end).map(|r| (p, r)))
            .collect();
        f.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), inner);
    }
}
fn hit(area: Rect, scroll: u16, start: usize, end: usize) -> Option<Rect> {
    let start = start.max(scroll as usize);
    let end = end.min(scroll as usize + area.height as usize);
    (end > start).then(|| {
        Rect::new(
            area.x,
            area.y + (start - scroll as usize) as u16,
            area.width,
            (end - start) as u16,
        )
    })
}
fn wrap(s: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![];
    }
    let mut result = vec![];
    let mut line = String::new();
    for c in clean(s).chars() {
        if width(&line) + Span::raw(c.to_string()).width() > limit {
            result.push(line);
            line = String::new()
        }
        line.push(c);
    }
    if !line.is_empty() {
        result.push(line)
    }
    result
}
