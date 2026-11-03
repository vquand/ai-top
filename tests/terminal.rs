use ai_top::{
    model::{Agent, Kind, Status},
    ui::App,
};
use ratatui::{Terminal, backend::TestBackend};
fn app() -> App {
    let mut app = App::default();
    app.agents = vec![
        Agent {
            id: "main".into(),
            kind: Kind::Claude,
            parent: None,
            project: "/work/project".into(),
            name: "agent-main".into(),
            summary: "Updating AI quota monitor".into(),
            status: Status::Running,
            started_at: Some(100),
            ended_at: None,
            updated_at: 100,
        },
        Agent {
            id: "child".into(),
            kind: Kind::Codex,
            parent: Some("main".into()),
            project: "/work/project".into(),
            name: "sub-agent-child".into(),
            summary: "Checking quota parser".into(),
            status: Status::Finished,
            started_at: Some(100),
            ended_at: Some(306),
            updated_at: 306,
        },
    ];
    app
}
fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(|f| app.draw(f, 400)).unwrap();
    t.backend()
        .buffer()
        .content
        .chunks(width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect())
        .collect()
}
#[test]
fn wide_rows_share_a_line_but_narrow_rows_put_summary_below_name() {
    let mut a = app();
    let wide = screen(&mut a, 180, 35);
    assert!(
        wide.iter()
            .any(|s| s.contains("agent-main") && s.contains("Updating AI quota monitor"))
    );
    let narrow = screen(&mut a, 100, 35);
    assert!(
        !narrow
            .iter()
            .any(|s| s.contains("agent-main") && s.contains("Updating AI quota monitor"))
    );
    assert!(
        narrow
            .iter()
            .any(|s| s.contains("Updating AI quota monitor"))
    );
}
#[test]
fn all_provider_default_filter_toggle_and_project_collapse() {
    let mut a = app();
    assert_eq!(a.selected, None);
    assert!(
        screen(&mut a, 180, 35)
            .join("\n")
            .contains("sub-agent-child")
    );
    a.toggle_provider(Kind::Claude);
    assert!(
        !screen(&mut a, 180, 35)
            .join("\n")
            .contains("sub-agent-child")
    );
    a.toggle_provider(Kind::Claude);
    assert_eq!(a.selected, None);
    a.toggle_project();
    assert!(!screen(&mut a, 180, 35).join("\n").contains("agent-main"));
}
#[test]
fn tiny_terminals_and_control_characters_are_safe() {
    let mut a = app();
    a.agents[0].name = "agent\x1b[2J\nmain".into();
    for (w, h) in [(1, 1), (35, 8), (60, 12), (80, 24)] {
        let out = screen(&mut a, w, h).join("\n");
        assert!(!out.contains('\x1b'));
    }
}

#[test]
fn keyboard_selection_scrolls_projects_and_provider_cards_into_view() {
    let mut a = app();
    let template = a.agents[0].clone();
    a.agents = (0..10)
        .map(|n| {
            let mut agent = template.clone();
            agent.id = format!("agent-{n}");
            agent.project = format!("/work/project-{n:02}");
            agent
        })
        .collect();
    a.focus = ai_top::ui::Focus::Projects;
    screen(&mut a, 100, 20);
    for _ in 0..9 {
        a.move_selection(1);
        screen(&mut a, 100, 20);
    }
    assert!(screen(&mut a, 100, 20).join("\n").contains("project-09"));
    a.focus = ai_top::ui::Focus::Providers;
    for _ in 0..3 {
        a.move_selection(1);
        screen(&mut a, 100, 20);
    }
    assert!(screen(&mut a, 100, 20).join("\n").contains("Antigravity"));
}

#[test]
fn moving_to_a_project_beyond_a_large_tree_keeps_its_heading_visible() {
    let mut a = app();
    let mut template = a.agents[0].clone();
    a.agents = (0..45)
        .map(|n| {
            let mut agent = template.clone();
            agent.id = format!("item-{n}");
            agent.project = "/work/first".into();
            agent
        })
        .collect();
    template.id = "last".into();
    template.project = "/work/last".into();
    a.agents.push(template);
    a.focus = ai_top::ui::Focus::Projects;
    screen(&mut a, 100, 20);
    a.move_selection(1);
    screen(&mut a, 100, 20);
    assert!(a.project_hits.iter().any(|(path, _)| path == "/work/last"));
    let mut a = ai_top::demo::app(400);
    screen(&mut a, 100, 15);
    for _ in 0..3 {
        a.move_selection(1);
        screen(&mut a, 100, 15);
    }
    assert!(
        a.provider_hits
            .iter()
            .any(|(kind, _)| *kind == Some(Kind::Antigravity))
    );
}
