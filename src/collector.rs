//! Linux process ownership plus incremental, read-only provider lifecycle metadata.
use crate::model::{Agent, Kind, Status, clean};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn timestamp(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        v.as_str().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|t| t.timestamp())
        })
    })
}
#[derive(Debug)]
pub struct Transcript {
    pub id: String,
    pub parent: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub status: Status,
    pub summary: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub updated_at: i64,
    pub finished: HashMap<String, (Status, i64)>,
    offset: u64,
    identity: (u64, u64),
    complete: bool,
}
impl Default for Transcript {
    fn default() -> Self {
        Self {
            id: String::new(),
            parent: None,
            cwd: None,
            name: None,
            status: Status::Unknown,
            summary: "No lifecycle data".into(),
            started_at: None,
            ended_at: None,
            updated_at: 0,
            finished: HashMap::new(),
            offset: 0,
            identity: (0, 0),
            complete: false,
        }
    }
}
impl Transcript {
    fn start(&mut self, at: Option<i64>) {
        self.status = Status::Running;
        self.started_at = at;
        self.ended_at = None;
        self.summary = "Working".into();
    }
    fn end(&mut self, status: Status, at: Option<i64>) {
        self.status = status;
        self.ended_at = at;
        self.summary = if status == Status::Finished {
            "Work completed"
        } else {
            "Work interrupted"
        }
        .into();
    }
    pub fn consume(&mut self, v: &Value, kind: Kind) {
        let at = timestamp(&v["timestamp"]);
        let t = v["type"].as_str().unwrap_or("");
        if let Some(at) = at {
            self.updated_at = self.updated_at.max(at);
        }
        if kind == Kind::Codex {
            let p = &v["payload"];
            if t == "session_meta" && self.id.is_empty() {
                self.id = p["id"].as_str().unwrap_or("").into();
                self.cwd = p["cwd"].as_str().map(str::to_string);
                self.parent = p["parent_thread_id"]
                    .as_str()
                    .or_else(|| {
                        p.pointer("/source/subagent/thread_spawn/parent_thread_id")
                            .and_then(Value::as_str)
                    })
                    .map(str::to_string);
                self.name = p["agent_nickname"]
                    .as_str()
                    .or_else(|| p["agent_role"].as_str())
                    .map(clean);
            } else if t == "event_msg" {
                match p["type"].as_str().unwrap_or("") {
                    "task_started" => self.start(at),
                    "task_complete" => self.end(Status::Finished, at),
                    "turn_aborted" => self.end(Status::Cancelled, at),
                    "agent_shutdown" => self.end(Status::Exited, at),
                    _ => {}
                }
            } else if t == "response_item" && self.status == Status::Running {
                match p["type"].as_str().unwrap_or("") {
                    "function_call" | "custom_tool_call" => {
                        if let Some(name) = p["name"].as_str() {
                            self.summary = format!("Running {}", clean(name));
                        }
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        self.summary = "Working · tool returned".into()
                    }
                    "reasoning" => self.summary = "Thinking".into(),
                    _ => {}
                }
            }
            return;
        }
        if self.cwd.is_none() {
            self.cwd = v["cwd"].as_str().map(str::to_string);
        }
        if self.id.is_empty() {
            self.id = v["sessionId"].as_str().unwrap_or("").into();
        }
        let m = &v["message"];
        let content = &m["content"];
        let text_input = content.is_string()
            || content
                .as_array()
                .is_some_and(|a| a.iter().any(|b| b["type"] == "text"));
        if t == "user" && v["toolEndsTurn"] != true && text_input {
            self.start(at);
        }
        if t == "api-request" {
            if self.status != Status::Running {
                self.start(at);
            }
            self.summary = "Waiting for model".into();
        }
        if t == "assistant" {
            if let Some(blocks) = content.as_array() {
                for b in blocks {
                    if b["type"] == "tool_use"
                        && let Some(name) = b["name"].as_str()
                    {
                        self.summary = format!("Running {}", clean(name));
                    }
                }
            }
            if matches!(
                m["stop_reason"].as_str(),
                Some("end_turn" | "stop_sequence")
            ) {
                self.end(Status::Finished, at);
            }
        }
        if v["toolEndsTurn"] == true {
            self.end(Status::Finished, at);
        }
        if let (Some(id), Some(status)) = (
            v["toolUseResult"]["agentId"].as_str(),
            v["toolUseResult"]["status"].as_str(),
        ) && let Some(status) = completion(status)
        {
            self.finished
                .insert(id.into(), (status, at.unwrap_or(self.updated_at)));
        }
        // Only lifecycle tags are extracted; message text is never kept in state.
        if t == "user" {
            let texts: Vec<&str> = if let Some(s) = content.as_str() {
                vec![s]
            } else {
                content
                    .as_array()
                    .map(|a| a.iter().filter_map(|b| b["text"].as_str()).collect())
                    .unwrap_or_default()
            };
            for text in texts {
                for notice in text.split("<task-notification>").skip(1) {
                    let notice = notice.split("</task-notification>").next().unwrap_or("");
                    if let (Some(id), Some(status)) = (
                        tag(notice, "task-id"),
                        tag(notice, "status").and_then(completion),
                    ) {
                        self.finished
                            .insert(id.into(), (status, at.unwrap_or(self.updated_at)));
                    }
                }
            }
        }
    }
    fn read(&mut self, path: &Path, kind: Kind) -> std::io::Result<()> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        let identity = (metadata.dev(), metadata.ino());
        if identity != self.identity || metadata.len() < self.offset {
            *self = Self::default();
            self.identity = identity;
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        let mut consumed = 0;
        const LINE_LIMIT: u64 = 4 * 1024 * 1024;
        const POLL_LIMIT: u64 = 16 * 1024 * 1024;
        while consumed < POLL_LIMIT {
            let mut line = vec![];
            let size = reader
                .by_ref()
                .take(LINE_LIMIT)
                .read_until(b'\n', &mut line)? as u64;
            if size == 0 {
                self.complete = true;
                return Ok(());
            }
            if !line.ends_with(b"\n") {
                if size < LINE_LIMIT {
                    self.complete = self.offset + size >= metadata.len();
                    return Ok(());
                }
                // Ignore oversized records without allocating an unbounded line.
                loop {
                    line.clear();
                    let n = reader
                        .by_ref()
                        .take(LINE_LIMIT)
                        .read_until(b'\n', &mut line)? as u64;
                    consumed += n;
                    self.offset += n;
                    if n == 0 || line.ends_with(b"\n") {
                        break;
                    }
                }
            } else if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                self.consume(&v, kind);
            }
            self.offset += size;
            consumed += size;
        }
        self.complete = self.offset >= metadata.len();
        Ok(())
    }
    fn agent(
        &self,
        kind: Kind,
        fallback: &str,
        project: &str,
        live: bool,
        main: bool,
        now: i64,
    ) -> Agent {
        let mut status = self.status;
        let mut summary = self.summary.clone();
        if !self.complete {
            status = Status::Unknown;
            summary = "Indexing lifecycle metadata".into();
        }
        if status == Status::Running && !live {
            status = Status::Exited;
            summary = "Process exited · completion unconfirmed".into();
        }
        if main && live && status == Status::Finished {
            status = Status::Paused;
            summary = "Waiting for input".into();
        }
        let ended_at = if status == Status::Exited && self.status == Status::Running {
            None
        } else {
            self.ended_at
        };
        Agent {
            id: if self.id.is_empty() {
                fallback.into()
            } else {
                self.id.clone()
            },
            parent: self.parent.clone(),
            kind,
            project: self.cwd.clone().unwrap_or_else(|| project.into()),
            name: self.name.clone().unwrap_or_else(|| {
                format!(
                    "{}-{}",
                    if main { "agent" } else { "sub-agent" },
                    (if self.id.is_empty() {
                        fallback
                    } else {
                        &self.id
                    })
                    .chars()
                    .take(8)
                    .collect::<String>()
                )
            }),
            summary,
            status,
            started_at: self.started_at.filter(|t| *t <= now),
            ended_at,
            updated_at: self.updated_at,
        }
    }
}
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    s.split_once(&format!("<{name}>"))?
        .1
        .split_once(&format!("</{name}>"))
        .map(|(v, _)| v)
}
fn completion(s: &str) -> Option<Status> {
    match s {
        "completed" | "done" => Some(Status::Finished),
        "failed" | "error" => Some(Status::Failed),
        "killed" | "cancelled" => Some(Status::Cancelled),
        _ => None,
    }
}
#[derive(Debug)]
struct Process {
    kind: Option<Kind>,
    parent: u32,
    cwd: PathBuf,
    files: Vec<PathBuf>,
    env: HashMap<String, String>,
    started: i64,
}
fn entries(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect()
}
fn modified(path: &Path) -> i64 {
    fs::metadata(path).map(|m| m.mtime()).unwrap_or(0)
}
fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 200
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub agents: Vec<Agent>,
    pub warnings: Vec<String>,
}
pub struct Collector {
    home: PathBuf,
    proc: PathBuf,
    cache: HashMap<PathBuf, Transcript>,
    seen: HashSet<PathBuf>,
    pub history_secs: i64,
    owners: HashMap<(Kind, String), u32>,
}
impl Collector {
    pub fn new(home: PathBuf, proc: PathBuf) -> Self {
        Self {
            home,
            proc,
            cache: HashMap::new(),
            seen: HashSet::new(),
            history_secs: 86400,
            owners: HashMap::new(),
        }
    }
    fn processes(&self) -> HashMap<u32, Process> {
        let mut processes = HashMap::new();
        let boot = fs::read_to_string(self.proc.join("stat"))
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("btime ").and_then(|s| s.parse::<i64>().ok()))
            .unwrap_or(0);
        // SAFETY: sysconf and getuid have no pointer arguments.
        let (ticks, uid) = unsafe { (libc::sysconf(libc::_SC_CLK_TCK).max(1), libc::getuid()) };
        for dir in entries(&self.proc) {
            let Some(pid) = dir
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if fs::metadata(&dir).map(|m| m.uid() != uid).unwrap_or(true) {
                continue;
            }
            let stat = fs::read_to_string(dir.join("stat")).unwrap_or_default();
            let Some((_, tail)) = stat.rsplit_once(") ") else {
                continue;
            };
            let fields: Vec<_> = tail.split_whitespace().collect();
            if fields.first() == Some(&"Z") {
                continue;
            }
            let name = fs::read_to_string(dir.join("comm")).unwrap_or_default();
            let kind = match name.trim() {
                "claude" => Some(Kind::Claude),
                "codex" => Some(Kind::Codex),
                "agy" => Some(Kind::Antigravity),
                _ => None,
            };
            let mut process = Process {
                kind,
                parent: fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
                cwd: PathBuf::new(),
                files: vec![],
                env: HashMap::new(),
                started: boot
                    + fields
                        .get(19)
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0)
                        / ticks,
            };
            if kind.is_some() {
                for item in fs::read(dir.join("environ"))
                    .unwrap_or_default()
                    .split(|b| *b == 0)
                {
                    if let Some((key, value)) = std::str::from_utf8(item)
                        .ok()
                        .and_then(|s| s.split_once('='))
                        && matches!(
                            key,
                            "CODEX_HOME"
                                | "CLAUDE_CONFIG_DIR"
                                | "CODEX_THREAD_ID"
                                | "CLAUDECODE"
                                | "AI_QUOTA_USAGE_PROBE"
                        )
                    {
                        process.env.insert(key.into(), value.into());
                    }
                }
                if process
                    .env
                    .get("AI_QUOTA_USAGE_PROBE")
                    .is_some_and(|v| v == "1")
                {
                    continue;
                }
                process.cwd = fs::read_link(dir.join("cwd")).unwrap_or_default();
                process.files = entries(&dir.join("fd"))
                    .iter()
                    .filter_map(|p| fs::read_link(p).ok())
                    .collect();
            }
            processes.insert(pid, process);
        }
        processes
    }
    fn read(&mut self, path: &Path, kind: Kind) -> Option<&Transcript> {
        self.seen.insert(path.to_owned());
        let state = self.cache.entry(path.to_owned()).or_default();
        state.read(path, kind).ok()?;
        Some(state)
    }
    pub fn snapshot(&mut self, now: i64) -> Snapshot {
        self.seen.clear();
        self.owners.clear();
        let processes = self.processes();
        let mut out = Snapshot::default();
        if !self.proc.join("stat").is_file() {
            out.warnings
                .push("Linux process metadata unavailable".into());
        }
        self.codex(&processes, now, &mut out);
        self.claude(&processes, now, &mut out);
        self.antigravity(&processes, now, &mut out);
        // Attach CLI calls made by another provider to their local parent session.
        for a in &mut out.agents {
            if a.parent.is_some() {
                continue;
            }
            let Some(pid) = self.owners.get(&(a.kind, a.id.clone())).copied() else {
                continue;
            };
            let mut current = pid;
            let mut visited = HashSet::from([pid]);
            while let Some(process) = processes.get(&current) {
                current = process.parent;
                if !visited.insert(current) {
                    break;
                }
                if let Some(((kind, id), _)) =
                    self.owners.iter().find(|(_, owner)| **owner == current)
                {
                    if *kind != a.kind || *id != a.id {
                        a.parent = Some(id.clone());
                    }
                    break;
                }
            }
            if a.parent.is_none()
                && let Some(id) = processes
                    .get(&pid)
                    .and_then(|p| p.env.get("CODEX_THREAD_ID"))
                    .filter(|id| **id != a.id)
            {
                a.parent = Some(id.clone());
            }
        }
        self.cache.retain(|p, _| self.seen.contains(p));
        let mut unique = HashMap::new();
        for a in out.agents {
            unique.insert((a.kind, a.id.clone()), a);
        }
        out.agents = unique.into_values().collect();
        out.agents
            .sort_by(|a, b| (a.kind, &a.id).cmp(&(b.kind, &b.id)));
        out
    }
    fn codex(&mut self, processes: &HashMap<u32, Process>, now: i64, out: &mut Snapshot) {
        let mut live = HashMap::new();
        let mut bases = HashSet::from([std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(".codex"))]);
        for p in processes.values().filter(|p| p.kind == Some(Kind::Codex)) {
            let base = p
                .env
                .get("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join(".codex"));
            bases.insert(base.clone());
            for path in &p.files {
                if path.starts_with(base.join("sessions"))
                    && path.extension().is_some_and(|e| e == "jsonl")
                {
                    live.insert(path.clone(), p.cwd.to_string_lossy().into_owned());
                }
            }
        }
        let mut active_projects: HashSet<String> = processes
            .values()
            .filter(|p| p.kind.is_some() && !p.cwd.as_os_str().is_empty())
            .map(|p| p.cwd.to_string_lossy().into_owned())
            .collect();
        let mut candidates = live.clone();
        for (path, cwd) in &live {
            if let Some(t) = self.read(path, Kind::Codex) {
                active_projects.insert(t.cwd.clone().unwrap_or_else(|| cwd.clone()));
            }
        }
        for base in bases {
            for db in entries(&base).iter().filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("state_") && s.ends_with(".sqlite"))
            }) {
                let Ok(conn) = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                else {
                    out.warnings.push("Codex session index unavailable".into());
                    continue;
                };
                let _ = conn.busy_timeout(Duration::from_millis(100));
                let Ok(mut stmt)=conn.prepare("SELECT rollout_path, cwd FROM threads WHERE updated_at >= ?1 ORDER BY updated_at DESC LIMIT 500")else{continue};
                if let Ok(rows) = stmt.query_map([now - self.history_secs], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) {
                    for (path, cwd) in rows.flatten() {
                        if active_projects.contains(&cwd) {
                            let path = PathBuf::from(path);
                            if path.starts_with(base.join("sessions")) {
                                candidates.insert(path, cwd);
                            }
                        }
                    }
                }
            }
        }
        let mut path_owners = HashMap::new();
        for (pid, p) in processes
            .iter()
            .filter(|(_, p)| p.kind == Some(Kind::Codex))
        {
            for path in &p.files {
                path_owners.insert(path.clone(), *pid);
            }
        }
        let mut parsed = vec![];
        for (path, cwd) in candidates {
            let fallback = path.file_stem().unwrap_or_default().to_string_lossy();
            if let Some(t) = self.read(&path, Kind::Codex) {
                let main = t.parent.is_none();
                let agent = t.agent(
                    Kind::Codex,
                    &fallback,
                    &cwd,
                    live.contains_key(&path),
                    main,
                    now,
                );
                if let Some(pid) = path_owners.get(&path) {
                    self.owners.insert((Kind::Codex, agent.id.clone()), *pid);
                }
                parsed.push((agent, live.contains_key(&path)));
            }
        }
        // Native children can be written by their live parent's process rather than
        // owning a separate Linux process. Never revive an unrelated orphan.
        let owned: HashSet<_> = parsed
            .iter()
            .filter(|(_, live)| *live)
            .map(|(a, _)| a.id.clone())
            .collect();
        for (mut a, is_live) in parsed {
            if !is_live
                && a.status == Status::Exited
                && a.parent.as_ref().is_some_and(|id| owned.contains(id))
            {
                a.status = Status::Unknown;
                a.summary = "Child ownership unverified".into();
            }
            out.agents.push(a);
        }
    }
    fn claude(&mut self, processes: &HashMap<u32, Process>, now: i64, out: &mut Snapshot) {
        let mut bases = HashSet::from([self.home.join(".claude")]);
        for p in processes.values().filter(|p| p.kind == Some(Kind::Claude)) {
            if let Some(base) = p.env.get("CLAUDE_CONFIG_DIR") {
                bases.insert(PathBuf::from(base));
            }
        }
        for base in bases {
            let mut registries: HashMap<u32, (i64, Value)> = HashMap::new();
            let mut unstable = false;
            for path in entries(&base.join("sessions")) {
                if path.extension().is_none_or(|s| s != "json") {
                    continue;
                }
                let before = fs::metadata(&path).ok();
                let value = fs::read(&path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                let after = fs::metadata(&path).ok();
                let stable = before.as_ref().zip(after.as_ref()).is_some_and(|(a, b)| {
                    a.ino() == b.ino()
                        && a.mtime_nsec() == b.mtime_nsec()
                        && a.mtime() == b.mtime()
                        && a.len() == b.len()
                });
                if !stable || value.is_none() {
                    unstable = true;
                    continue;
                }
                let value = value.unwrap();
                let Some(pid) = value["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) else {
                    continue;
                };
                let modified = before.unwrap().mtime();
                if processes
                    .get(&pid)
                    .is_some_and(|p| p.kind == Some(Kind::Claude) && modified >= p.started)
                    && registries.get(&pid).is_none_or(|(t, _)| modified >= *t)
                {
                    registries.insert(pid, (modified, value));
                }
            }
            if unstable {
                out.warnings
                    .push("Claude session registry changing; retrying next poll".into());
                continue;
            }
            let mut live = HashMap::new();
            for (pid, (_, r)) in registries {
                if let Some(id) = r["sessionId"].as_str().filter(|s| safe_id(s)) {
                    let cwd = r["cwd"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| processes[&pid].cwd.to_string_lossy().into_owned());
                    self.owners.insert((Kind::Claude, id.to_owned()), pid);
                    live.insert(id.to_owned(), cwd);
                }
            }
            for project_dir in entries(&base.join("projects")) {
                let roots = entries(&project_dir);
                let live_project = processes.values().filter(|p| p.kind.is_some()).any(|p| {
                    let encoded: String = p
                        .cwd
                        .to_string_lossy()
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                        .collect();
                    !encoded.is_empty()
                        && project_dir
                            .file_name()
                            .is_some_and(|name| name == encoded.as_str())
                });
                let active_dir = live_project
                    || roots.iter().any(|p| {
                        p.file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|id| live.contains_key(id))
                    });
                if !active_dir {
                    continue;
                }
                let mut roots: Vec<_> = roots
                    .into_iter()
                    .filter(|p| {
                        p.extension().is_some_and(|s| s == "jsonl")
                            && (modified(p) >= now - self.history_secs
                                || p.file_stem()
                                    .and_then(|s| s.to_str())
                                    .is_some_and(|s| live.contains_key(s)))
                    })
                    .collect();
                roots.sort_by_key(|p| std::cmp::Reverse(modified(p)));
                roots.truncate(200);
                for path in roots {
                    let id = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let is_live = live.contains_key(&id);
                    let cwd = live.get(&id).cloned().unwrap_or_default();
                    let Some(main) = self.read(&path, Kind::Claude) else {
                        continue;
                    };
                    let mut a = main.agent(Kind::Claude, &id, &cwd, is_live, true, now);
                    a.id = id.clone();
                    let completed = main.finished.clone();
                    let project = a.project.clone();
                    out.agents.push(a);
                    for child_path in entries(&project_dir.join(&id).join("subagents")) {
                        if child_path.extension().is_none_or(|s| s != "jsonl") {
                            continue;
                        }
                        let child_id = child_path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .trim_start_matches("agent-")
                            .to_string();
                        if let Some(t) = self.read(&child_path, Kind::Claude) {
                            let mut child =
                                t.agent(Kind::Claude, &child_id, &project, is_live, false, now);
                            child.id = child_id.clone();
                            child.parent = Some(id.clone());
                            child.project = project.clone();
                            if let Some((status, at)) = completed
                                .get(&child_id)
                                .filter(|(_, at)| Some(*at) >= child.started_at)
                            {
                                child.status = *status;
                                child.ended_at = Some(*at);
                                child.summary = "Work completed".into();
                            }
                            out.agents.push(child);
                        }
                    }
                }
            }
        }
    }
    fn antigravity(&mut self, processes: &HashMap<u32, Process>, now: i64, out: &mut Snapshot) {
        let base = self.home.join(".gemini/antigravity-cli");
        let mut live = HashSet::new();
        for p in processes
            .values()
            .filter(|p| p.kind == Some(Kind::Antigravity))
        {
            for path in &p.files {
                if path.parent() == Some(base.join("presence").as_path())
                    && path.extension().is_some_and(|s| s == "lock")
                    && let Some(id) = path.file_stem().and_then(|s| s.to_str())
                {
                    live.insert(id.to_string());
                }
            }
        }
        let known_projects: HashSet<String> = processes
            .values()
            .filter(|p| p.kind.is_some() && !p.cwd.as_os_str().is_empty())
            .map(|p| p.cwd.to_string_lossy().into_owned())
            .collect();
        if live.is_empty()
            && (known_projects.is_empty() || !base.join("conversation_summaries.db").is_file())
        {
            return;
        }
        let result = (|| -> rusqlite::Result<Vec<Agent>> {
            let conn = Connection::open_with_flags(
                base.join("conversation_summaries.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            conn.busy_timeout(Duration::from_millis(100))?;
            let mut stmt=conn.prepare("SELECT conversation_id,parent_conversation_id,status,killed,last_user_input_time,last_modified_time,workspace_uris,agent_name FROM conversation_summaries ORDER BY last_modified_time DESC LIMIT 500")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let parents: HashMap<_, _> =
                rows.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
            let workspace_paths: HashMap<_, _> = rows
                .iter()
                .map(|r| {
                    let uris: Vec<String> = serde_json::from_str(&r.6).unwrap_or_default();
                    (
                        r.0.as_str(),
                        uris.first().and_then(|s| file_uri(s)).unwrap_or_default(),
                    )
                })
                .collect();
            let mut agents = vec![];
            for (id, parent, status, killed, start, end, _, name) in &rows {
                let mut ancestor = id.as_str();
                let mut seen = HashSet::new();
                while !live.contains(ancestor) && seen.insert(ancestor) {
                    let Some(next) = parents.get(ancestor).filter(|p| !p.is_empty()) else {
                        break;
                    };
                    ancestor = next;
                }
                let owned = live.contains(ancestor);
                let mut project = String::new();
                let mut workspace_id = id.as_str();
                let mut visited = HashSet::new();
                while visited.insert(workspace_id) {
                    if let Some(path) = workspace_paths.get(workspace_id).filter(|p| !p.is_empty())
                    {
                        project = path.clone();
                        break;
                    }
                    let Some(next) = parents.get(workspace_id).filter(|s| !s.is_empty()) else {
                        break;
                    };
                    workspace_id = next;
                }
                let started_at = timestamp(&Value::String(start.clone()));
                let updated_at = timestamp(&Value::String(end.clone())).unwrap_or(0);
                if project.is_empty()
                    || (!owned
                        && (!known_projects.contains(&project)
                            || updated_at < now - self.history_secs))
                {
                    continue;
                }
                let mut status = if *killed {
                    Status::Cancelled
                } else {
                    match status.as_str() {
                        "CASCADE_RUN_STATUS_RUNNING" | "CASCADE_RUN_STATUS_BUSY" => Status::Running,
                        "CASCADE_RUN_STATUS_DONE" | "CASCADE_RUN_STATUS_COMPLETED" => {
                            if parent.is_empty() {
                                Status::Paused
                            } else {
                                Status::Finished
                            }
                        }
                        "CASCADE_RUN_STATUS_IDLE" => Status::Paused,
                        "CASCADE_RUN_STATUS_ERROR" => Status::Failed,
                        _ => Status::Unknown,
                    }
                };
                if !owned && status == Status::Running {
                    status = Status::Exited;
                }
                if !owned && status == Status::Paused {
                    status = Status::Finished;
                }
                agents.push(Agent {
                    id: id.clone(),
                    parent: (!parent.is_empty()).then(|| parent.clone()),
                    kind: Kind::Antigravity,
                    project,
                    name: if name.is_empty() {
                        format!("agent-{}", id.chars().take(8).collect::<String>())
                    } else {
                        clean(name)
                    },
                    summary: if status == Status::Running {
                        "Working"
                    } else if status == Status::Paused {
                        "Waiting for input"
                    } else {
                        "Work completed"
                    }
                    .into(),
                    status,
                    started_at: started_at.filter(|t| *t <= now),
                    ended_at: (!matches!(status, Status::Running | Status::Exited)
                        && updated_at > 0)
                        .then_some(updated_at),
                    updated_at,
                });
            }
            Ok(agents)
        })();
        match result {
            Ok(agents) => out.agents.extend(agents),
            Err(_) => out
                .warnings
                .push("Antigravity lifecycle index unavailable or unsupported".into()),
        }
    }
}
fn file_uri(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    if !raw.starts_with('/') {
        return None;
    }
    let mut bytes = vec![];
    let source = raw.as_bytes();
    let mut i = 0;
    while i < source.len() {
        if source[i] == b'%'
            && i + 2 < source.len()
            && let Ok(n) = std::str::from_utf8(&source[i + 1..i + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .ok_or(())
        {
            bytes.push(n);
            i += 3;
            continue;
        }
        bytes.push(source[i]);
        i += 1;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod uri_tests {
    #[test]
    fn malformed_non_ascii_percent_paths_never_panic() {
        assert_eq!(
            super::file_uri("file:///work/%あ"),
            Some("/work/%あ".into())
        );
        assert_eq!(
            super::file_uri("file:///work/a%20b"),
            Some("/work/a b".into())
        );
    }
}
