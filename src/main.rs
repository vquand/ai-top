use ai_top::{
    antigravity,
    collector::{Collector, Snapshot},
    config::{Config, Mode},
    data::{self, Fetched},
    demo,
    model::Kind,
    ui::{App, Focus},
};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
#[derive(Default)]
struct Options {
    demo: bool,
    once: bool,
    json: bool,
    no_network: bool,
    width: Option<u16>,
    height: Option<u16>,
    history: Option<i64>,
}
fn options() -> Result<Option<Options>, String> {
    let mut out = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "ai-top — local AI quota and agent monitor\n\nUsage: ai-top [--demo] [--no-network] [--history-hours 24]\n       ai-top --once [--demo] [--width 160] [--height 45]\n       ai-top --json\n\n  --demo          Explicit offline sample data\n  --no-network    Disable quota API calls; local activity still updates\n  --once          Print one text view instead of opening the TUI\n  --json          Print read-only local activity metadata; no network\n  --history-hours Retain recent sessions in active projects (1–168; default 24)\n  --width/height  Dimensions for --once (default 140 × 40)\n\nTUI: A/Esc all providers · 1/2/3 filter · Tab focus · ↑↓ select\n     Enter toggle · PgUp/Dn scroll · U used/remaining · R refresh · Q quit"
                );
                return Ok(None);
            }
            "--version" | "-V" => {
                println!("ai-top {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--demo" => out.demo = true,
            "--once" => out.once = true,
            "--json" => out.json = true,
            "--no-network" => out.no_network = true,
            "--width" | "--height" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} needs a number"))?
                    .parse::<u16>()
                    .map_err(|_| format!("Invalid {arg}"))?;
                if !(1..=500).contains(&value) {
                    return Err(format!("{arg} must be 1–500"));
                }
                if arg == "--width" {
                    out.width = Some(value)
                } else {
                    out.height = Some(value)
                }
            }
            "--history-hours" => {
                let n = args
                    .next()
                    .ok_or("--history-hours needs a number")?
                    .parse::<i64>()
                    .map_err(|_| "Invalid history duration")?;
                if !(1..=168).contains(&n) {
                    return Err("History duration must be 1–168 hours".into());
                }
                out.history = Some(n * 3600)
            }
            _ => return Err(format!("Unknown option: {arg}. Use --help.")),
        }
    }
    Ok(Some(out))
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn fetch(kind: Kind, jio: bool) -> Fetched {
    match kind {
        Kind::Claude => data::fetch_claude(),
        Kind::Codex => data::fetch_codex(),
        Kind::Antigravity => antigravity::fetch(jio),
    }
}
enum Update {
    Activity(Snapshot),
    Quota(Kind, Fetched),
    Refreshing(Kind),
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let Some(opts) = options().map_err(io::Error::other)? else {
        return Ok(());
    };
    let cfg = Config::load();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let mut collector = Collector::new(home, PathBuf::from("/proc"));
    collector.history_secs = opts.history.unwrap_or(86400);
    if opts.json {
        let snapshot = if opts.demo {
            Snapshot {
                agents: demo::app(now()).agents,
                warnings: vec!["DEMO".into()],
            }
        } else {
            collector.snapshot(now())
        };
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let mut app = if opts.demo {
        demo::app(now())
    } else {
        App::default()
    };
    app.remaining = cfg.display == Mode::Remaining;
    if opts.no_network && !opts.demo {
        for q in &mut app.quotas {
            q.refreshing = false;
            q.error = Some("Quota refresh disabled (--no-network)".into());
        }
    }
    if opts.once {
        if !opts.demo {
            let snapshot = collector.snapshot(now());
            app.agents = snapshot.agents;
            app.warnings = snapshot.warnings;
            if !opts.no_network {
                thread::scope(|s| {
                    let handles: Vec<_> = Kind::ALL
                        .into_iter()
                        .map(|kind| (kind, s.spawn(move || fetch(kind, cfg.antigravity_jio))))
                        .collect();
                    for (kind, h) in handles {
                        app.quotas
                            .iter_mut()
                            .find(|q| q.kind == kind)
                            .unwrap()
                            .update(
                                h.join()
                                    .unwrap_or_else(|_| Err("Quota worker failed".into())),
                                now(),
                            );
                    }
                });
            }
        }
        let width = opts.width.unwrap_or(140);
        let height = opts.height.unwrap_or(40);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|f| app.draw(f, now()))?;
        for row in terminal.backend().buffer().content.chunks(width as usize) {
            println!(
                "{}",
                row.iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .trim_end()
            );
        }
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "ai-top needs an interactive terminal. Use --once or --json for piped output.".into(),
        );
    }
    let (tx, rx) = mpsc::channel();
    let mut refreshers = vec![];
    if !opts.demo {
        let activity_tx = tx.clone();
        thread::spawn(move || {
            loop {
                if activity_tx
                    .send(Update::Activity(collector.snapshot(now())))
                    .is_err()
                {
                    break;
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        if !opts.no_network {
            for kind in Kind::ALL {
                let tx = tx.clone();
                let (refresh_tx, refresh_rx) = mpsc::sync_channel(1);
                refreshers.push(refresh_tx);
                let interval = cfg.refresh_secs.max(15);
                let jio = cfg.antigravity_jio;
                thread::spawn(move || {
                    loop {
                        if tx.send(Update::Refreshing(kind)).is_err() {
                            break;
                        }
                        let result = fetch(kind, jio);
                        if tx.send(Update::Quota(kind, result)).is_err() {
                            break;
                        }
                        match refresh_rx.recv_timeout(Duration::from_secs(interval)) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(_) => break,
                        }
                    }
                });
            }
        }
    }
    // Signal handlers only set a lock-free flag; terminal I/O stays on this thread.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let _restore = Restore;
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    while !STOP.load(Ordering::Relaxed) {
        for update in rx.try_iter() {
            match update {
                Update::Activity(s) => {
                    app.agents = s.agents;
                    app.warnings = s.warnings
                }
                Update::Refreshing(kind) => {
                    app.quotas
                        .iter_mut()
                        .find(|q| q.kind == kind)
                        .unwrap()
                        .refreshing = true
                }
                Update::Quota(kind, result) => app
                    .quotas
                    .iter_mut()
                    .find(|q| q.kind == kind)
                    .unwrap()
                    .update(result, now()),
            }
        }
        terminal.draw(|f| app.draw(f, now()))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if app.help {
                    if matches!(k.code, KeyCode::Esc | KeyCode::Char('?')) {
                        app.help = false
                    }
                    continue;
                }
                match k.code {
                    KeyCode::Char('q' | 'Q') => break,
                    KeyCode::Char('?') => app.help = true,
                    KeyCode::Esc | KeyCode::Char('a' | 'A') => app.select(None),
                    KeyCode::Char('1') => app.toggle_provider(Kind::Claude),
                    KeyCode::Char('2') => app.toggle_provider(Kind::Codex),
                    KeyCode::Char('3') => app.toggle_provider(Kind::Antigravity),
                    KeyCode::Tab | KeyCode::BackTab => {
                        app.focus = if app.focus == Focus::Providers {
                            Focus::Projects
                        } else {
                            Focus::Providers
                        }
                    }
                    KeyCode::Left => app.focus = Focus::Providers,
                    KeyCode::Right => app.focus = Focus::Projects,
                    KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if app.focus == Focus::Providers {
                            if let Some(kind) = app.selected {
                                app.toggle_provider(kind)
                            }
                        } else {
                            app.toggle_project()
                        }
                    }
                    KeyCode::PageDown => app.page(true),
                    KeyCode::PageUp => app.page(false),
                    KeyCode::Home => {
                        if app.focus == Focus::Providers {
                            app.sidebar_scroll = 0
                        } else {
                            app.scroll = 0
                        }
                    }
                    KeyCode::End => {
                        if app.focus == Focus::Providers {
                            app.sidebar_scroll = u16::MAX
                        } else {
                            app.scroll = u16::MAX
                        }
                    }
                    KeyCode::Char('u' | 'U') => app.remaining = !app.remaining,
                    KeyCode::Char('r' | 'R') => {
                        for refresher in &refreshers {
                            let _ = refresher.try_send(());
                        }
                    }
                    _ => {}
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(event::MouseButton::Left) => app.click(m.column, m.row),
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    app.focus = if app.sidebar_area.contains((m.column, m.row).into()) {
                        Focus::Providers
                    } else {
                        Focus::Projects
                    };
                    let scroll = if app.focus == Focus::Providers {
                        &mut app.sidebar_scroll
                    } else {
                        &mut app.scroll
                    };
                    *scroll = if m.kind == MouseEventKind::ScrollDown {
                        scroll.saturating_add(3)
                    } else {
                        scroll.saturating_sub(3)
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("ai-top: {e}");
        std::process::exit(1)
    }
}
