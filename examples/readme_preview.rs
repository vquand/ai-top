//! Render the actual terminal buffer as HTML for reproducible README screenshots.
use ratatui::{Terminal, backend::TestBackend, style::Color};
fn color(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Yellow => "#e9cc7e".into(),
        Color::LightRed => "#ff7777".into(),
        _ => "#d9e1e8".into(),
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let width: u16 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(180);
    let height: u16 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(40);
    if !(60..=250).contains(&width) || !(12..=100).contains(&height) {
        return Err("Dimensions must be 60–250 × 12–100".into());
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-03T05:00:00Z")?.timestamp();
    let mut app = ai_top::demo::app(now);
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|f| app.draw(f, now))?;
    println!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>ai-top terminal preview</title><style>body{{margin:0;padding:20px;background:#101419}}pre{{margin:0;color:#d9e1e8;font:14px/20px 'DejaVu Sans Mono',monospace;white-space:pre}}</style><pre aria-label=\"ai-top actual terminal buffer, demo data\">"
    );
    for row in terminal.backend().buffer().content.chunks(width as usize) {
        for cell in row {
            print!(
                "<span style=\"color:{}\">{}</span>",
                color(cell.fg),
                escape(cell.symbol())
            );
        }
        println!();
    }
    println!("</pre></html>");
    Ok(())
}
