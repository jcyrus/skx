//! Interactive `ratatui` cockpit for browsing and managing installed skills.
//!
//! Under a header summarising drift and context cost across the whole
//! workspace: a full-width skill table (status, name, author, version,
//! approximate token cost, and a per-target status column), above a
//! markdown-rendered preview and an agent-target matrix for the selected
//! skill. `j`/`k` navigate whatever pane holds focus, `Tab` cycles the
//! three panes, `/` filters, `Space` toggles a target, `s` syncs the
//! selected skill and `S` syncs everything, `?` shows the key map, `q`
//! quits. The mouse works too, where the terminal reports it.

pub mod app;
pub mod theme;
pub mod ui;

use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::{App, Screen, SyncScope};

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Launches the full-screen TUI cockpit against the current directory's
/// workspace (same `skx.toml`/cache resolution as every other `skx`
/// command).
pub fn run(theme_flag: Option<&str>) -> Result<()> {
    let (root, home) = app::default_root_and_home()?;
    let config =
        skx_core::Config::load(&skx_core::config_path(&home))?.with_theme_override(theme_flag);
    let mut application = App::load_with(root, home, config)?;

    let mut terminal = init_terminal(&application)?;
    let result = event_loop(&mut terminal, &mut application);
    restore_terminal();
    result
}

fn init_terminal(app: &App) -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    if app.config.mouse {
        // Best-effort: a terminal that doesn't report mouse events simply
        // never sends any, and every action remains reachable by keyboard.
        let _ = execute!(stdout, EnableMouseCapture);
    }
    if app.config.set_terminal_title {
        let workspace = app
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| app.root.display().to_string());
        let _ = execute!(stdout, SetTitle(format!("skx — {workspace}")));
    }

    // If we panic mid-render, restore the terminal first so the panic
    // message is actually readable instead of stuck inside raw mode /
    // the alternate screen.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

/// Undoes everything `init_terminal` did.
///
/// Safe to call more than once and never fails loudly: it runs from the
/// panic hook, from normal exit, and around suspend, and a terminal left in
/// raw mode is a worse outcome than a swallowed error.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        SetTitle("")
    );
}

fn event_loop(terminal: &mut Tui, app: &mut App) -> Result<()> {
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| ui::draw(frame, app))?;
            dirty = false;
        }

        // A running sync animates, so poll fast enough that the spinner
        // reads as motion; otherwise wait long enough to be genuinely idle
        // rather than repainting an unchanged screen five times a second.
        let timeout = if app.sync.is_some() {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(500)
        };

        if event::poll(timeout)? {
            // Exactly one `read()` per ready event: reading twice would
            // consume — and silently discard — a second keystroke.
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(app, key.code, key.modifiers);
                    dirty = true;
                }
                Event::Mouse(mouse) => dirty |= handle_mouse(app, mouse),
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }

        if app.suspend_requested {
            app.suspend_requested = false;
            suspend(terminal)?;
            dirty = true;
        }

        if let Some(job) = &mut app.sync {
            job.frame += 1;
            app.poll_sync();
            dirty = true;
        }
        if app.status_expired() {
            dirty = true;
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Routes a pointer event. Returns whether the screen needs redrawing.
///
/// Only left-click and the wheel are wired: a TUI that reacts to drags and
/// middle-clicks tends to fight the terminal's own text selection, which is
/// how users copy things out of it.
fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let (x, y) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => app.click(x, y),
        MouseEventKind::ScrollDown => app.scroll_at(x, y, 3),
        MouseEventKind::ScrollUp => app.scroll_at(x, y, -3),
        _ => false,
    }
}

/// Drops the terminal back to the shell for `Ctrl-Z`, then restores it when
/// the job is resumed.
///
/// Without this the process suspends while still in raw mode and the
/// alternate screen, leaving the shell with no echo and no prompt — the
/// terminal looks broken until the user blindly types `fg` and `reset`.
#[cfg(unix)]
fn suspend(terminal: &mut Tui) -> Result<()> {
    restore_terminal();
    // SAFETY: `raise` is async-signal-safe and this is the documented way
    // to hand control back to the shell. It returns once we're resumed.
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.clear()?;
    Ok(())
}

#[cfg(not(unix))]
fn suspend(_terminal: &mut Tui) -> Result<()> {
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    // The filter and the help overlay are modal: while either is up it
    // owns the keyboard, so a `q` typed into a filter narrows the list
    // instead of quitting the app.
    if app.filter_active {
        handle_filter_key(app, code);
        return;
    }
    if app.overlay.is_some() {
        match code {
            // `y` as well as Enter: a yes/no prompt should accept the
            // answer people actually type at one.
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_overlay(),
            KeyCode::Esc
            | KeyCode::Char('n')
            | KeyCode::Char('N')
            | KeyCode::Char('?')
            | KeyCode::Char('q') => {
                app.overlay = None;
            }
            _ => {}
        }
        return;
    }
    match &app.screen {
        Screen::Discover(_) => handle_discover_key(app, code),
        Screen::Main => handle_main_key(app, code, modifiers),
    }
}

fn handle_filter_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.clear_filter(),
        KeyCode::Enter => app.commit_filter(),
        KeyCode::Backspace => app.pop_filter(),
        KeyCode::Char(c) => app.push_filter(c),
        _ => {}
    }
}

fn handle_main_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        // `q` and Ctrl-C are the only two ways out. `Esc` used to quit when
        // no filter was set, which meant every safe use of the key — close
        // help, cancel discover, clear filter — trained a reflex that
        // eventually killed the session.
        KeyCode::Char('q') => app.request_quit(),
        // Ctrl-C leaves now. Every terminal user expects it to mean exactly
        // that, and a confirmation on the universal abort key is the one
        // place a prompt is genuinely wrong.
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.force_quit(),
        KeyCode::Char('z') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.suspend_requested = true
        }
        KeyCode::Esc => app.escape(),

        // One motion vocabulary at every level: j/k moves within whatever
        // pane holds focus, including scrolling the preview. The old
        // Shift-J/K chord was both a two-key gesture for the most common
        // reading action and invisible in the status line.
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') | KeyCode::Home => app.jump_selection(false),
        KeyCode::Char('G') | KeyCode::End => app.jump_selection(true),
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => app.page_preview(true),
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => app.page_preview(false),
        KeyCode::PageDown => app.page_preview(true),
        KeyCode::PageUp => app.page_preview(false),

        KeyCode::Tab => app.toggle_focus(),
        KeyCode::BackTab => app.focus_back(),
        KeyCode::Char('/') => app.begin_filter(),
        KeyCode::Char('?') | KeyCode::F(1) => app.toggle_help(),
        KeyCode::Char(' ') => app.toggle_selected_target(),

        // Effort scales with consequence: `s` touches only the row under
        // the cursor, `S` writes across the whole workspace and asks first.
        KeyCode::Char('s') => app.request_sync(SyncScope::Selected),
        KeyCode::Char('S') => app.request_sync(SyncScope::All),
        KeyCode::Char('d') => app.open_discover(),
        _ => {}
    }
}

fn handle_discover_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.cancel_discover(),
        KeyCode::Char('j') | KeyCode::Down => app.move_discover_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_discover_selection(-1),
        KeyCode::Char(' ') => app.toggle_discover_selected(),
        KeyCode::Enter => app.commit_discover(),
        _ => {}
    }
}
