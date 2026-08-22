//! Renders the cockpit to an ANSI dump at a fixed size, for eyeballing
//! layout changes without launching a terminal session.
//! `cargo run -p skx_tui --example screenshot [width] [height] [keys]`
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let w: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(120);
    let h: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(34);
    let keys = args.get(3).cloned().unwrap_or_default();

    let (root, home) = skx_tui::app::default_root_and_home()?;
    let mut app = skx_tui::app::App::load(root, home)?;
    for c in keys.chars() {
        match c {
            'j' => app.move_selection(1),
            'k' => app.move_selection(-1),
            't' => app.toggle_focus(),
            '?' => app.toggle_help(),
            'J' => app.move_preview(1),
            'f' => app.begin_filter(),
            'd' => app.open_discover(),
            'S' => app.request_sync(skx_tui::app::SyncScope::All),
            c if app.filter_active => app.push_filter(c),
            _ => {}
        }
    }

    let mut terminal = Terminal::new(TestBackend::new(w, h))?;
    terminal.draw(|frame| skx_tui::ui::draw(frame, &mut app))?;
    println!("{}", terminal.backend());
    Ok(())
}
