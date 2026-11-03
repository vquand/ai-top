use ai_top::{
    collector::{Collector, Transcript},
    model::{Kind, Status},
};
use serde_json::json;
use std::{fs, os::unix::fs::symlink, path::PathBuf};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ai-top-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, value: &str) {
        let p = self.0.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, value).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn codex_lifecycle_preserves_child_identity_and_never_keeps_prompt_text() {
    let mut t = Transcript::default();
    t.consume(&json!({"type":"session_meta","payload":{"id":"child","cwd":"/work","source":{"subagent":{"thread_spawn":{"parent_thread_id":"main"}}}}}),Kind::Codex);
    t.consume(
        &json!({"type":"session_meta","payload":{"id":"copied-parent"}}),
        Kind::Codex,
    );
    t.consume(
        &json!({"type":"event_msg","timestamp":100,"payload":{"type":"task_started"}}),
        Kind::Codex,
    );
    assert_eq!(t.id, "child");
    assert_eq!(t.parent.as_deref(), Some("main"));
    assert_eq!(t.status, Status::Running);
    t.consume(&json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"PRIVATE"}}),Kind::Codex);
    assert!(!t.summary.contains("PRIVATE"));
    t.consume(&json!({"type":"event_msg","timestamp":200,"payload":{"type":"task_complete","last_agent_message":"PRIVATE"}}),Kind::Codex);
    assert_eq!(t.status, Status::Finished);
    assert_eq!(t.ended_at, Some(200));
    t.consume(
        &json!({"type":"event_msg","timestamp":250,"payload":{"type":"task_started"}}),
        Kind::Codex,
    );
    assert_eq!(t.started_at, Some(250));
    assert_eq!(t.ended_at, None);
}
#[test]
fn claude_end_turn_and_background_completion_are_not_reported_working() {
    let mut t = Transcript::default();
    t.consume(
        &json!({"type":"user","timestamp":100,"message":{"content":"PRIVATE"}}),
        Kind::Claude,
    );
    t.consume(&json!({"type":"assistant","timestamp":130,"message":{"stop_reason":"end_turn","content":[{"type":"text","text":"PRIVATE"}]}}),Kind::Claude);
    assert_eq!(t.status, Status::Finished);
    t.consume(&json!({"type":"user","timestamp":140,"toolUseResult":{"agentId":"child","status":"completed"},"message":{"content":[{"type":"tool_result"}]}}),Kind::Claude);
    assert_eq!(t.finished.get("child"), Some(&(Status::Finished, 140)));
    assert!(!format!("{t:?}").contains("PRIVATE"));
}
#[test]
fn partial_lines_wait_for_newline_and_crashed_process_cannot_stay_running() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let proc = f.0.join("proc");
    f.write(
        "home/.codex/sessions/main.jsonl",
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"main\",\"cwd\":\"/work\"}}\n",
            "{\"type\":\"event_msg\",\"timestamp\":100,\"payload\":{\"type\":\"task_started\"}}\n",
            "{\"type\":\"event_msg\",\"timestamp\":120,\"payload\":{\"type\":\"task_complete\"}}"
        ),
    );
    f.write("proc/stat", "btime 1\n");
    f.write("proc/42/comm", "codex\n");
    let mut stat = vec!["0"; 22];
    stat[0] = "S";
    stat[1] = "1";
    stat[19] = "100";
    f.write("proc/42/stat", &format!("42 (codex) {}", stat.join(" ")));
    f.write(
        "proc/42/environ",
        &format!("CODEX_HOME={}\0", home.join(".codex").display()),
    );
    fs::create_dir_all(proc.join("42/fd")).unwrap();
    symlink(
        home.join(".codex/sessions/main.jsonl"),
        proc.join("42/fd/4"),
    )
    .unwrap();
    symlink("/work", proc.join("42/cwd")).unwrap();
    let mut collector = Collector::new(home, proc.clone());
    let first = collector.snapshot(110);
    assert_eq!(
        first.agents.iter().find(|a| a.id == "main").unwrap().status,
        Status::Running
    );
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(f.0.join("home/.codex/sessions/main.jsonl"))
        .unwrap();
    use std::io::Write;
    writeln!(file).unwrap();
    let next = collector.snapshot(125);
    assert_eq!(
        next.agents.iter().find(|a| a.id == "main").unwrap().status,
        Status::Paused
    );
    fs::remove_dir_all(proc.join("42")).unwrap();
    let last = collector.snapshot(130);
    assert!(last.agents.iter().all(|a| a.status != Status::Running));
}

fn process(f: &Fixture, pid: u32, name: &str, parent: u32) {
    f.write("proc/stat", "btime 1\n");
    f.write(&format!("proc/{pid}/comm"), name);
    let mut stat = vec!["0".to_string(); 22];
    stat[0] = "S".into();
    stat[1] = parent.to_string();
    stat[19] = "100".into();
    f.write(
        &format!("proc/{pid}/stat"),
        &format!("{pid} ({name}) {}", stat.join(" ")),
    );
    f.write(&format!("proc/{pid}/environ"), "");
    fs::create_dir_all(f.0.join(format!("proc/{pid}/fd"))).unwrap();
    symlink("/work", f.0.join(format!("proc/{pid}/cwd"))).unwrap();
}
#[test]
fn claude_project_owns_children_and_completed_children_survive_while_parent_works() {
    let f = Fixture::new();
    process(&f, 42, "claude", 1);
    f.write(
        "home/.claude/sessions/current.json",
        r#"{"pid":42,"sessionId":"main","cwd":"/work"}"#,
    );
    f.write("home/.claude/projects/-work/main.jsonl",concat!("{\"type\":\"user\",\"timestamp\":100,\"cwd\":\"/work\",\"message\":{\"content\":\"PRIVATE\"}}\n","{\"type\":\"user\",\"timestamp\":110,\"toolUseResult\":{\"agentId\":\"child\",\"status\":\"completed\"},\"message\":{\"content\":[{\"type\":\"tool_result\"}]}}\n"));
    f.write(
        "home/.claude/projects/-work/main/subagents/agent-child.jsonl",
        "{\"type\":\"user\",\"timestamp\":101,\"message\":{\"content\":\"PRIVATE\"}}\n",
    );
    let mut c = Collector::new(f.0.join("home"), f.0.join("proc"));
    let s = c.snapshot(120);
    assert_eq!(s.agents.len(), 2);
    let child = s.agents.iter().find(|a| a.id == "child").unwrap();
    assert_eq!(child.parent.as_deref(), Some("main"));
    assert_eq!(child.status, Status::Finished);
    assert_eq!(child.ended_at, Some(110));
    assert_eq!(child.project, "/work");
    assert!(!serde_json::to_string(&s).unwrap().contains("PRIVATE"));
    // A detached CLI launched with origin metadata attaches to its parent AI.
    process(&f, 43, "codex", 42);
    let base = f.0.join("home/.codex");
    f.write(
        "proc/43/environ",
        &format!("CODEX_HOME={}\0", base.display()),
    );
    f.write("home/.codex/sessions/cli.jsonl","{\"type\":\"session_meta\",\"payload\":{\"id\":\"cli\",\"cwd\":\"/work\"}}\n{\"type\":\"event_msg\",\"timestamp\":100,\"payload\":{\"type\":\"task_started\"}}\n");
    symlink(base.join("sessions/cli.jsonl"), f.0.join("proc/43/fd/4")).unwrap();
    let s = c.snapshot(125);
    let cli = s.agents.iter().find(|a| a.id == "cli").unwrap();
    assert_eq!(cli.parent.as_deref(), Some("main"));
    assert_eq!(cli.status, Status::Running);
}
#[test]
fn antigravity_presence_gates_running_and_keeps_finished_children() {
    let f = Fixture::new();
    process(&f, 42, "agy", 1);
    let base = f.0.join("home/.gemini/antigravity-cli");
    fs::create_dir_all(&base).unwrap();
    let conn = rusqlite::Connection::open(base.join("conversation_summaries.db")).unwrap();
    conn.execute_batch("CREATE TABLE conversation_summaries (conversation_id TEXT,parent_conversation_id TEXT,status TEXT,killed INTEGER,last_user_input_time TEXT,last_modified_time TEXT,workspace_uris TEXT,agent_name TEXT); INSERT INTO conversation_summaries VALUES ('main','','CASCADE_RUN_STATUS_RUNNING',0,'2026-11-03T01:00:00Z','2026-11-03T01:01:00Z','[\"file:///work/project%20one\"]',''); INSERT INTO conversation_summaries VALUES ('child','main','CASCADE_RUN_STATUS_COMPLETED',0,'2026-11-03T01:00:00Z','2026-11-03T01:01:00Z','[]','checker');").unwrap();
    fs::create_dir_all(base.join("presence")).unwrap();
    f.write("home/.gemini/antigravity-cli/presence/main.lock", "");
    symlink(base.join("presence/main.lock"), f.0.join("proc/42/fd/4")).unwrap();
    let mut c = Collector::new(f.0.join("home"), f.0.join("proc"));
    let s = c.snapshot(2_000_000_000);
    assert_eq!(s.agents.len(), 2);
    assert_eq!(
        s.agents.iter().find(|a| a.id == "child").unwrap().status,
        Status::Finished
    );
    assert!(s.agents.iter().all(|a| a.project == "/work/project one"));
    fs::remove_dir_all(f.0.join("proc/42")).unwrap();
    let s = c.snapshot(2_000_000_002);
    assert!(s.agents.iter().all(|a| a.status != Status::Running));
}
