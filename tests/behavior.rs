use ai_top::model::{Agent, Kind, Status, projects};
fn agent(id: &str, parent: Option<&str>, status: Status, kind: Kind) -> Agent {
    Agent {
        id: id.into(),
        parent: parent.map(str::to_string),
        kind,
        project: "/work/app".into(),
        name: id.into(),
        summary: "Working".into(),
        status,
        started_at: Some(100),
        ended_at: None,
        updated_at: 110,
    }
}
#[test]
fn all_providers_merge_projects_and_keep_finished_siblings_under_their_parent() {
    let agents = vec![
        agent("parent", None, Status::Paused, Kind::Claude),
        agent("done", Some("parent"), Status::Finished, Kind::Claude),
        agent("running", Some("parent"), Status::Running, Kind::Claude),
        agent("codex", None, Status::Running, Kind::Codex),
    ];
    let grouped = projects(&agents, None);
    assert_eq!(grouped.len(), 1);
    let ids: Vec<_> = grouped[0]
        .rows
        .iter()
        .map(|r| r.agent.id.as_str())
        .collect();
    assert_eq!(ids, vec!["codex", "parent", "running", "done"]);
    assert_eq!(grouped[0].rows[2].depth, 1);
    assert_eq!(projects(&agents, Some(Kind::Codex))[0].rows.len(), 1);
}
#[test]
fn inactive_projects_are_hidden_and_cycles_cannot_drop_agents_or_loop() {
    let mut agents = vec![
        agent("a", Some("b"), Status::Running, Kind::Codex),
        agent("b", Some("a"), Status::Paused, Kind::Codex),
    ];
    let mut old = agent("old", None, Status::Finished, Kind::Claude);
    old.project = "/old".into();
    agents.push(old);
    let grouped = projects(&agents, None);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].rows.len(), 2);
}

#[test]
fn finished_limit_is_per_project_and_preserves_other_statuses_and_nested_work() {
    let mut agents = vec![];
    for path in ["/work/a", "/work/b"] {
        let mut root = agent(&format!("root-{path}"), None, Status::Running, Kind::Claude);
        root.project = path.into();
        for n in 0..8 {
            let mut child = agent(
                &format!("done-{path}-{n}"),
                Some(&root.id),
                Status::Finished,
                Kind::Codex,
            );
            child.project = path.into();
            child.ended_at = Some(n);
            if n == 0 {
                let mut nested = agent(
                    &format!("nested-{path}"),
                    Some(&child.id),
                    Status::Running,
                    Kind::Claude,
                );
                nested.project = path.into();
                agents.push(nested);
            }
            agents.push(child);
        }
        for status in [
            Status::Paused,
            Status::Failed,
            Status::Cancelled,
            Status::Exited,
            Status::Unknown,
        ] {
            let mut child = agent(
                &format!("{path}-{status:?}"),
                Some(&root.id),
                status,
                Kind::Claude,
            );
            child.project = path.into();
            agents.push(child);
        }
        agents.push(root);
    }
    for project in projects(&agents, None) {
        assert_eq!(project.hidden_finished(), 3);
        let visible = project.visible_rows(false);
        assert_eq!(
            visible
                .iter()
                .filter(|r| r.agent.status == Status::Finished)
                .count(),
            5
        );
        assert_eq!(visible.len(), 12);
        let nested = visible
            .iter()
            .find(|r| r.agent.id.starts_with("nested-"))
            .unwrap();
        assert_eq!(
            nested.depth, 1,
            "hidden parent must not hide or over-indent active descendants"
        );
        assert_eq!(project.visible_rows(true).len(), 15);
    }
}
