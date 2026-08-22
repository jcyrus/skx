//! Rendering: a function from `&App` to what's on screen.
//!
//! The one deliberate mutation is the preview pane writing its measured
//! viewport height and content length back into `App`, so scrolling can be
//! clamped to real content — the renderer is the only place those numbers
//! are known.
//!
//! Four layers, drawn in order:
//!
//! * **L0 chrome** — the header and status line. Never scrolls, never takes
//!   focus, always present so the user keeps their bearings.
//! * **L1 workspace** — the skill table above a preview / agent-matrix
//!   detail row. Exactly one pane carries the focus ring.
//! * **L2 overlay** — help and confirmations, over a dimmed backdrop so the
//!   modal dominates rather than competing with the table behind it.
//! * **L3 drawer** — discover, which replaces the workspace but keeps L0.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, TableState, Wrap,
};

use crate::app::{
    App, DiscoverState, Focus, Level, LoadedSkill, MATRIX_COLUMNS, Overlay, PaneRects, Region,
    Screen, SyncJob, TargetStatus,
};
use crate::theme::{self, Palette};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let palette = app.theme;

    // L0 base. This single call is what makes the cockpit legible on a
    // light terminal: without it every cell inherits whatever the user's
    // profile sets, and the palette is being measured against a background
    // we never established.
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.bg_base).fg(palette.fg)),
        frame.area(),
    );

    let [header, workspace, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);

    match &app.screen {
        Screen::Main => {
            // The table is what's being navigated, and a long skill list is
            // far more common than a long preview, so it takes the larger
            // share rather than an even split.
            let [table_area, detail] =
                Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(workspace);
            let [preview_area, side] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(MATRIX_PANE_W)])
                    .areas(detail);
            // The matrix needs exactly one row per target and never more,
            // so give it that and hand the remainder to the artifact list
            // rather than leaving two thirds of the column empty.
            let [matrix_area, artifacts_area] =
                Layout::vertical([Constraint::Length(MATRIX_H), Constraint::Min(0)]).areas(side);

            // Pointer hit-testing needs to know where the panes actually
            // landed, and this is the only place that knows.
            app.panes = PaneRects {
                table: region(table_area),
                preview: region(preview_area),
                matrix: region(matrix_area),
            };

            draw_skill_table(frame, app, table_area);
            draw_preview(frame, app, preview_area);
            draw_agent_matrix(frame, app, matrix_area);
            draw_artifacts(frame, app, artifacts_area);
        }
        Screen::Discover(state) => draw_discover(frame, state, &palette, workspace),
    }

    draw_status_line(frame, app, status);

    if let Some(overlay) = app.overlay.clone() {
        dim(frame, workspace, &palette);
        match overlay {
            Overlay::Help => draw_help(frame, &palette, frame.area()),
            Overlay::ConfirmSyncAll { skills } => draw_confirm(
                frame,
                &palette,
                Confirm {
                    title: "Confirm",
                    question: format!("Sync all {skills} skills?"),
                    detail: "This writes and symlinks across every declared target.".to_string(),
                    accept: "sync all",
                    tone: palette.success,
                },
                frame.area(),
            ),
            Overlay::ConfirmQuit { pending } => {
                // The prompt earns its place only when it says something
                // the user doesn't already know. With work outstanding it
                // names the cost; otherwise it's a plain courtesy stop.
                let (detail, tone) = if pending > 0 {
                    (
                        format!(
                            "{pending} skill(s) have targets that were never written. \
                             Press S to sync before leaving."
                        ),
                        palette.warning,
                    )
                } else {
                    ("Everything is synced.".to_string(), palette.fg_dim)
                };
                draw_confirm(
                    frame,
                    &palette,
                    Confirm {
                        title: "Quit skx",
                        question: "Quit?".to_string(),
                        detail,
                        accept: "quit",
                        tone,
                    },
                    frame.area(),
                )
            }
        }
    }
}

/// `ratatui::Rect` → the app's own geometry type, so hit-testing stays
/// testable without constructing a terminal.
fn region(rect: Rect) -> Region {
    Region {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

/// Knocks the workspace back so an overlay reads as the only live surface.
///
/// Re-styling in place rather than blanking preserves the glyphs — the user
/// keeps their context — while removing the colour that would otherwise
/// compete with the modal for attention.
fn dim(frame: &mut Frame, area: Rect, palette: &Palette) {
    let buffer = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_fg(palette.border).set_bg(palette.bg_base);
            }
        }
    }
}

// ── Header ──────────────────────────────────────────────────────────────

/// Identity and workspace on the first line, vitals on the second.
///
/// Deliberately borderless: two rows of chrome around two rows of data
/// would spend a third of the header's cells saying nothing.
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let p = &app.theme;
    let health = app.health();
    let [title_line, stats_line] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let workspace = compact_path(
        &app.root.display().to_string(),
        &app.home.display().to_string(),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" skx ", Style::default().fg(p.bg_base).bg(p.accent).bold()),
            Span::styled(
                format!(" v{}  ", env!("CARGO_PKG_VERSION")),
                Style::default().fg(p.fg_dim),
            ),
            Span::styled(workspace, Style::default().fg(p.fg)),
        ])),
        title_line,
    );

    let ratio = health.ratio();
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            theme::meter(ratio, 14),
            Style::default().fg(p.health(ratio)),
        ),
        Span::styled(
            format!(" {:>3.0}%  ", ratio * 100.0),
            Style::default().fg(p.health(ratio)).bold(),
        ),
        Span::styled("│  ", Style::default().fg(p.border)),
        Span::styled(
            format!("{} ", health.skills),
            Style::default().fg(p.fg).bold(),
        ),
        Span::styled("skills", Style::default().fg(p.fg_dim)),
        Span::styled("  ·  ", Style::default().fg(p.border)),
        Span::styled(
            format!("{} ", theme::compact_count(health.tokens)),
            // Coloured by the *mean* cost per skill rather than the total,
            // so the thresholds mean the same thing here as in the table's
            // per-row TOKENS column.
            Style::default()
                .fg(token_color(p, health.tokens / health.skills.max(1)))
                .bold(),
        ),
        Span::styled("tokens", Style::default().fg(p.fg_dim)),
    ];
    // Only surface counts that are non-zero — a healthy workspace should
    // read as a short line, not a row of zeroes to scan past.
    for (count, label, color) in [
        (health.in_sync, "synced", p.success),
        (health.not_synced, "pending", p.warning),
        (health.needs_attention, "drift", p.alt),
        (health.error, "error", p.danger),
    ] {
        if count == 0 {
            continue;
        }
        spans.push(Span::styled("  ·  ", Style::default().fg(p.border)));
        spans.push(Span::styled(
            format!("{count} "),
            Style::default().fg(color).bold(),
        ));
        spans.push(Span::styled(label, Style::default().fg(p.fg_dim)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), stats_line);
}

/// Replaces a leading home directory with `~`, so the header shows
/// `~/Projects/skx` rather than spending half the line on `/Users/...`.
fn compact_path(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_string(),
    }
}

// ── Skill table ─────────────────────────────────────────────────────────

/// Which optional columns fit at a given width. Columns drop in reverse
/// priority order as the terminal narrows, so the table degrades to
/// "status + name" rather than truncating everything into uselessness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    author: bool,
    version: bool,
    tokens: bool,
    description: bool,
    matrix: bool,
}

impl Columns {
    /// `inner` is the usable width inside the panel borders.
    ///
    /// `has_authors` gates the AUTHOR column on it carrying any data at
    /// all: when every row would read `—`, sixteen columns of placeholder
    /// is worse than the column not existing.
    fn resolve(inner: u16, has_authors: bool) -> Self {
        let fits = |extra: u16| inner >= FIXED_W + MATRIX_W + extra;
        let author = has_authors && fits(TOKENS_W + VERSION_W + AUTHOR_W);
        let version = fits(TOKENS_W + VERSION_W);
        let tokens = fits(TOKENS_W);
        let used = FIXED_W
            + MATRIX_W
            + u16::from(tokens) * TOKENS_W
            + u16::from(version) * VERSION_W
            + u16::from(author) * AUTHOR_W;
        Self {
            matrix: fits(0),
            tokens,
            version,
            author,
            // Slack becomes information rather than a gap. Below this the
            // description is too clipped to be worth the column.
            description: inner.saturating_sub(used) >= MIN_DESCRIPTION_W,
        }
    }

    fn optional_width(self) -> u16 {
        u16::from(self.author) * AUTHOR_W
            + u16::from(self.version) * VERSION_W
            + u16::from(self.tokens) * TOKENS_W
            + u16::from(self.matrix) * MATRIX_W
    }
}

const FIXED_W: u16 = 4 + MIN_NAME_W;
const MIN_NAME_W: u16 = 14;
const AUTHOR_W: u16 = 16;
const VERSION_W: u16 = 8;
const TOKENS_W: u16 = 8;
/// Five target cells at three columns each, matching the two-letter header
/// codes above them.
const MATRIX_W: u16 = 15;
/// Names past this are truncated rather than allowed to soak up every spare
/// column and push the data columns to the far edge.
const MAX_NAME_W: u16 = 30;
/// Below this a description is clipped past usefulness, so the column is
/// dropped instead.
const MIN_DESCRIPTION_W: u16 = 24;
/// Wide enough for `" ▸ antigrav ███  needs attention "` plus borders.
const MATRIX_PANE_W: u16 = 38;
/// Borders plus exactly one row per target — the matrix never grows.
const MATRIX_H: u16 = 2 + MATRIX_COLUMNS.len() as u16;

fn draw_skill_table(frame: &mut Frame, app: &App, area: Rect) {
    let p = &app.theme;
    let title = if app.filter.is_empty() {
        format!("Skills {}", app.skills.len())
    } else {
        format!("Skills {}/{}", app.visible.len(), app.skills.len())
    };
    let block = p.panel(&title, app.focus == Focus::SkillList);

    let inner = area.width.saturating_sub(2);
    let has_authors = app.visible.iter().any(|&i| app.skills[i].author() != "—");
    let columns = Columns::resolve(inner, has_authors);
    let name_width = inner
        .saturating_sub(4 + columns.optional_width())
        .clamp(1, MAX_NAME_W);

    let mut widths = vec![Constraint::Length(3), Constraint::Length(name_width)];
    let mut header = vec![Cell::from(""), Cell::from("NAME")];
    if columns.author {
        widths.push(Constraint::Length(AUTHOR_W));
        header.push(Cell::from("AUTHOR"));
    }
    if columns.version {
        widths.push(Constraint::Length(VERSION_W));
        header.push(Cell::from("VER"));
    }
    if columns.tokens {
        widths.push(Constraint::Length(TOKENS_W));
        header.push(Cell::from(Line::from("TOKENS").right_aligned()));
    }
    // Whatever is left over goes to the description.
    //
    // This slot used to be an empty spacer that pinned the matrix to the
    // right edge. On a wide terminal that produced ~110 columns of nothing
    // between a row's name and its status glyphs — far enough apart that
    // associating the two took a deliberate saccade across dead space,
    // which is the proximity problem the layout was supposed to avoid.
    // Filling it with the description turns the slack into the one piece
    // of information you otherwise had to move the cursor to read.
    if columns.description {
        widths.push(Constraint::Min(MIN_DESCRIPTION_W));
        header.push(Cell::from("DESCRIPTION"));
    }
    if columns.matrix {
        widths.push(Constraint::Length(MATRIX_W));
        header.push(Cell::from(matrix_header()));
    }

    let rows: Vec<Row> = app
        .visible
        .iter()
        .map(|&i| skill_row(p, &app.skills[i], columns))
        .collect();

    if rows.is_empty() {
        let message = if app.skills.is_empty() {
            "No skills installed — run `skx add <path>`"
        } else {
            "No skills match this filter"
        };
        draw_empty_state(frame, app, message, block, area);
        return;
    }

    let table = Table::new(rows, widths)
        .header(
            // `bg_raised`, never `bg_selected`: when the header shared the
            // selection colour the two were indistinguishable, and on first
            // paint the header read as "row 0 is selected".
            Row::new(header).style(Style::default().fg(p.fg_dim).bg(p.bg_raised).bold()),
        )
        .block(block)
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(p.bg_selected).bold());

    let mut state = TableState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);

    draw_scrollbar(frame, p, area, app.selected, app.visible.len());
}

fn skill_row<'a>(p: &Palette, loaded: &'a LoadedSkill, columns: Columns) -> Row<'a> {
    let overall = overall_status(loaded);
    let scope_tag = match loaded.entry.scope {
        skx_core::Scope::Global => "G",
        skx_core::Scope::Local => "L",
    };

    let mut cells = vec![
        Cell::from(Line::from(vec![
            Span::styled(overall.glyph(), status_style(p, overall)),
            Span::raw(" "),
            // `alt`, not `success`: the scope badge is decorative, and
            // painting it green made a green cell stop meaning "in sync".
            Span::styled(scope_tag, Style::default().fg(p.alt)),
        ])),
        Cell::from(Span::styled(
            loaded.entry.name.clone(),
            Style::default().fg(p.fg),
        )),
    ];
    if columns.author {
        let author = loaded.author();
        // An unknown author is chrome, not data — dimming it keeps the eye
        // on the rows that are actually attributed.
        let style = if author == "—" {
            Style::default().fg(p.border)
        } else {
            Style::default().fg(p.fg_dim)
        };
        cells.push(Cell::from(Span::styled(author.to_string(), style)));
    }
    if columns.version {
        cells.push(Cell::from(Span::styled(
            loaded.version().to_string(),
            Style::default().fg(p.fg_dim),
        )));
    }
    if columns.tokens {
        cells.push(Cell::from(
            Line::from(Span::styled(
                theme::compact_count(loaded.tokens),
                Style::default().fg(token_color(p, loaded.tokens)),
            ))
            .right_aligned(),
        ));
    }
    if columns.description {
        cells.push(Cell::from(Span::styled(
            loaded
                .skill
                .as_ref()
                .map(|s| first_sentence(&s.frontmatter.description))
                .unwrap_or_default(),
            Style::default().fg(p.fg_dim),
        )));
    }
    if columns.matrix {
        cells.push(Cell::from(Line::from(
            loaded
                .statuses
                .iter()
                .flat_map(|&st| {
                    [
                        Span::styled(st.glyph(), status_style(p, st)),
                        Span::raw("  "),
                    ]
                })
                .collect::<Vec<_>>(),
        )));
    }
    Row::new(cells)
}

/// The opening sentence of a description, for the table's one-line cell.
///
/// Skill descriptions are written for a model, not a column: they run to
/// several hundred characters and enumerate trigger phrases. The first
/// sentence is the part that says what the skill *is*, and the rest is
/// matching material the preview pane already shows in full.
fn first_sentence(description: &str) -> String {
    let end = description
        .find(". ")
        .map(|i| i + 1)
        .unwrap_or(description.len());
    description[..end].trim_end_matches('.').to_string()
}

/// Two-letter codes above the matrix cells, so the columns are
/// self-labelling. Single letters won't do: claude, cursor and copilot all
/// start with `C`.
fn matrix_code(target_id: &str) -> &'static str {
    match target_id {
        "antigravity" => "AG",
        "claude_code" => "CL",
        "cursor" => "CU",
        "copilot" => "CP",
        "mcp" => "MC",
        _ => "??",
    }
}

fn matrix_header() -> String {
    MATRIX_COLUMNS
        .iter()
        .map(|id| format!("{} ", matrix_code(id)))
        .collect()
}

/// Colours a token count by absolute cost. Thresholds are fixed rather than
/// relative to the largest skill: "is this expensive" shouldn't change
/// meaning just because you uninstalled something bigger.
fn token_color(p: &Palette, tokens: usize) -> Color {
    match tokens {
        0..=1_999 => p.fg_dim,
        2_000..=4_999 => p.fg,
        5_000..=9_999 => p.warning,
        _ => p.alt,
    }
}

/// A position indicator for any list long enough to need one. Without it a
/// 53-row table gives no sense of where in the set the cursor sits.
fn draw_scrollbar(frame: &mut Frame, p: &Palette, area: Rect, position: usize, total: usize) {
    let viewport = area.height.saturating_sub(3) as usize; // borders + header
    if total <= viewport {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(1)).position(position);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .track_style(Style::default().fg(p.border))
            .thumb_style(Style::default().fg(p.accent)),
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

/// The empty state doubles as the splash screen — it's the one surface with
/// room to spare, so the wordmark lives here rather than costing rows in the
/// persistent header.
fn draw_empty_state(frame: &mut Frame, app: &App, message: &str, block: Block, area: Rect) {
    let p = &app.theme;
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    if inner.height >= theme::LOGO.len() as u16 + 3 {
        // Each logo row is centred independently, so the tagline has to be
        // its own line — appending it to the middle row would shift that
        // row relative to the two around it and shear the wordmark.
        for row in theme::LOGO {
            lines.push(Line::from(Span::styled(row, Style::default().fg(p.accent))).centered());
        }
        lines.push(
            Line::from(Span::styled(
                "skill exchange",
                Style::default().fg(p.fg_dim),
            ))
            .centered(),
        );
        lines.push(Line::raw(""));
    }
    lines.push(
        Line::from(Span::styled(
            message,
            Style::default().fg(p.fg_dim).italic(),
        ))
        .centered(),
    );
    if app.skills.is_empty() {
        lines.push(
            Line::from(Span::styled(
                "press d to discover skills already on disk",
                Style::default().fg(p.border),
            ))
            .centered(),
        );
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ── Agent matrix ────────────────────────────────────────────────────────

fn draw_agent_matrix(frame: &mut Frame, app: &App, area: Rect) {
    let p = &app.theme;
    let focused = app.focus == Focus::AgentMatrix;
    let block = p.panel("Agent Matrix", focused);

    let Some(loaded) = app.selected_skill() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no skill selected",
                Style::default().fg(p.fg_dim).italic(),
            ))
            .block(block),
            area,
        );
        return;
    };

    let lines: Vec<Line> = MATRIX_COLUMNS
        .iter()
        .enumerate()
        .map(|(i, &target_id)| {
            let status = loaded
                .statuses
                .get(i)
                .copied()
                .unwrap_or(TargetStatus::Error);
            let selected = focused && app.matrix_selected == i;
            let style = status_style(p, status);

            // A caret on the focused row rather than reversed video, which
            // fought with the status colour and made the row hard to read.
            let cursor = if selected { "▸" } else { " " };
            let label_style = if selected {
                Style::default().fg(p.fg).bold()
            } else {
                Style::default().fg(p.fg_dim)
            };
            Line::from(vec![
                Span::styled(format!(" {cursor} "), Style::default().fg(p.accent)),
                Span::styled(format!("{:<9}", short_target(target_id)), label_style),
                Span::styled(status_bar_glyphs(status), style),
                Span::raw("  "),
                Span::styled(status_label(status), style),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Where the selected skill actually landed on disk.
///
/// The matrix says a target is in sync; this says *what file that means* —
/// the question the matrix raises and can't answer. It's read straight from
/// the recorded sync state, so it also exposes artifacts belonging to
/// targets the skill no longer declares.
fn draw_artifacts(frame: &mut Frame, app: &App, area: Rect) {
    let p = &app.theme;
    let Some(loaded) = app.selected_skill() else {
        frame.render_widget(p.panel("Artifacts", false), area);
        return;
    };

    let records: Vec<_> = app
        .state
        .artifacts
        .iter()
        .filter(|record| record.skill == loaded.entry.name)
        .collect();

    let title = format!("Artifacts {}", records.len());
    let block = p.panel(&title, false);
    if records.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "nothing written yet — press s to sync",
                Style::default().fg(p.fg_dim).italic(),
            ))
            .block(block)
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let home = app.home.display().to_string();
    let width = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    for record in records {
        lines.push(Line::from(Span::styled(
            matrix_code(&record.target),
            Style::default().fg(p.alt).bold(),
        )));
        // Paths are long and the interesting end is the right-hand side, so
        // elide from the left when they don't fit.
        let path = compact_path(&record.path.display().to_string(), &home);
        lines.push(Line::from(Span::styled(
            elide_left(&path, width.saturating_sub(1)),
            Style::default().fg(p.fg_dim),
        )));
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Truncates from the left with a leading ellipsis, keeping the tail — for
/// paths, where the filename matters more than the mount point.
fn elide_left(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width || width == 0 {
        return format!(" {text}");
    }
    let tail: String = text.chars().skip(len - width.saturating_sub(1)).collect();
    format!(" …{tail}")
}

fn short_target(target_id: &str) -> &'static str {
    match target_id {
        "antigravity" => "antigrav",
        "claude_code" => "claude",
        "cursor" => "cursor",
        "copilot" => "copilot",
        "mcp" => "mcp",
        _ => "?",
    }
}

/// A three-cell "LED" per target — reads as a meter at a glance where a
/// single dot reads as punctuation.
fn status_bar_glyphs(status: TargetStatus) -> &'static str {
    match status {
        TargetStatus::NotDeclared => "░░░",
        TargetStatus::NotSynced => "▓░░",
        TargetStatus::InSync => "███",
        TargetStatus::NeedsAttention => "██░",
        TargetStatus::Error => "▚▚▚",
    }
}

// ── Preview ─────────────────────────────────────────────────────────────

fn draw_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let p = app.theme;
    let focused = app.focus == Focus::Preview;

    let Some(index) = app.selected_index() else {
        app.preview_len = 0;
        frame.render_widget(p.panel("Preview", focused), area);
        return;
    };
    let loaded = &app.skills[index];

    let Some(skill) = &loaded.skill else {
        let text = loaded
            .load_error
            .clone()
            .unwrap_or_else(|| "failed to load".to_string());
        app.preview_len = 1;
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(p.danger))
                .block(p.panel("Preview", focused))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            skill.frontmatter.name.to_string(),
            Style::default().fg(p.accent).bold(),
        )),
        Line::from(Span::styled(
            skill.frontmatter.description.clone(),
            Style::default().fg(p.fg),
        )),
    ];

    let mut meta = vec![
        Span::styled("v", Style::default().fg(p.fg_dim)),
        Span::styled(
            skill.frontmatter.effective_version().to_string(),
            Style::default().fg(p.alt),
        ),
    ];
    if let Some(license) = &skill.frontmatter.license {
        meta.push(Span::styled("  ", Style::default()));
        meta.push(Span::styled(license.clone(), Style::default().fg(p.fg_dim)));
    }
    if !skill.frontmatter.triggers.is_empty() {
        meta.push(Span::styled("  triggers ", Style::default().fg(p.fg_dim)));
        meta.push(Span::styled(
            skill.frontmatter.triggers.join(" "),
            Style::default().fg(p.info),
        ));
    }
    lines.push(Line::from(meta));
    lines.push(Line::from(Span::styled(
        "─".repeat(area.width.saturating_sub(2) as usize),
        Style::default().fg(p.border),
    )));

    // A real markdown renderer handles emphasis, inline code and
    // syntax-highlighted fenced blocks; `restyle_markdown` then pulls the
    // structural markers into our palette, which the library leaves bare.
    lines.extend(restyle_markdown(tui_markdown::from_str(&skill.body), &p));

    app.preview_len = lines.len() as u16;
    app.preview_viewport = area.height.saturating_sub(2);
    let scroll = app.preview_scroll.min(app.preview_len.saturating_sub(1));

    let title = if app.preview_len > app.preview_viewport {
        let shown = (scroll + app.preview_viewport).min(app.preview_len);
        format!("Preview  {shown}/{}", app.preview_len)
    } else {
        "Preview".to_string()
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(p.panel(&title, focused))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
    draw_scrollbar(frame, &p, area, scroll as usize, app.preview_len as usize);
}

/// Repaints `tui-markdown`'s structural markers in the cockpit's palette.
///
/// The renderer emits headings, list bullets and block-quote pips as
/// *unstyled* leading spans (`"# "`, `"- "`, `">"`) and leaves body text at
/// the terminal's default foreground — exactly the flat look this pane is
/// trying to avoid. Every marker arrives as its own span, so recolouring is
/// a matter of rewriting span 0 and tinting the rest. Spans the renderer
/// *did* style are left alone.
fn restyle_markdown<'a>(text: Text<'a>, p: &Palette) -> Vec<Line<'a>> {
    text.lines
        .into_iter()
        .map(|mut line| {
            // The renderer paints a background behind H1 headings — set on
            // the `Line`, not its spans — which lands as an off-palette
            // cyan bar in a pane that owns its own surface. Backgrounds are
            // the pane's to decide; only foregrounds survive.
            line.style.bg = None;
            for span in line.spans.iter_mut() {
                span.style.bg = None;
            }
            let Some(marker) = line.spans.first().map(|s| s.content.to_string()) else {
                return line;
            };

            let level = marker
                .trim_end()
                .chars()
                .take_while(|&c| c == '#')
                .count()
                .min(6);
            if level > 0 && marker.trim_end().len() == level {
                // Swap `###` for a coloured left rule: the depth still reads
                // from colour and indent without spending width on hashes.
                let (color, indent) = match level {
                    1 => (p.accent, ""),
                    2 => (p.info, ""),
                    _ => (p.alt, " "),
                };
                line.spans[0] = Span::styled(format!("{indent}▌ "), Style::default().fg(color));
                for span in line.spans.iter_mut().skip(1) {
                    span.style = span.style.patch(Style::default().fg(color).bold());
                }
                return line;
            }

            if matches!(marker.as_str(), "- " | "* " | "+ ") {
                line.spans[0] = Span::styled("• ", Style::default().fg(p.alt));
            } else if marker == ">" {
                line.spans[0] = Span::styled("┃", Style::default().fg(p.alt));
                for span in line.spans.iter_mut().skip(1) {
                    span.style = span.style.patch(Style::default().fg(p.fg_dim).italic());
                }
                return line;
            } else if marker.starts_with("```") {
                line.spans[0] = Span::styled(marker, Style::default().fg(p.border));
            }

            for span in line.spans.iter_mut() {
                if span.style.fg.is_none() {
                    span.style = span.style.fg(p.fg);
                }
            }
            line
        })
        .collect()
}

// ── Discover (L3 drawer) ────────────────────────────────────────────────

fn draw_discover(frame: &mut Frame, state: &DiscoverState, p: &Palette, area: Rect) {
    // A preview beside the list: conflicting same-named candidates were
    // otherwise chosen on a path string alone, with no way to see what
    // either one actually contains.
    let [list_area, preview_area] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);

    let conflicts = state.groups.values().filter(|g| g.len() > 1).count();
    let title = format!(
        "Discover  {} found · {} selected · {conflicts} conflict(s)",
        state.candidates.len(),
        state.included_count()
    );

    let items: Vec<ListItem> = state
        .display_order
        .iter()
        .map(|&i| {
            let candidate = &state.candidates[i];
            let is_conflict = state
                .groups
                .get(candidate.skill.frontmatter.name.as_str())
                .is_some_and(|g| g.len() > 1);
            let included = state.included[i];

            let mut spans = vec![
                Span::styled(
                    if included { " ▣ " } else { " ▢ " },
                    Style::default().fg(if included { p.success } else { p.border }),
                ),
                Span::styled(
                    match candidate.scope_hint {
                        skx_core::Scope::Global => "G",
                        skx_core::Scope::Local => "L",
                    },
                    Style::default().fg(p.alt),
                ),
                Span::raw(" "),
                Span::styled(
                    candidate.skill.frontmatter.name.to_string(),
                    Style::default().fg(p.fg).bold(),
                ),
                Span::styled(
                    format!(" v{}", candidate.skill.frontmatter.effective_version()),
                    Style::default().fg(p.fg_dim),
                ),
            ];
            if is_conflict {
                spans.push(Span::styled(
                    "  conflict",
                    Style::default().fg(p.warning).bold(),
                ));
            }
            if let Some(key) = candidate.found_in.default_target_key()
                && !candidate.skill.frontmatter.targets.contains_key(key)
            {
                spans.push(Span::styled(
                    format!("  +{key}"),
                    Style::default().fg(p.info),
                ));
            }
            spans.push(Span::styled(
                format!("  {}", candidate.path.display()),
                Style::default().fg(p.border),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(p.panel(&title, true))
        .highlight_style(Style::default().bg(p.bg_selected).bold());

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);
    draw_scrollbar(
        frame,
        p,
        list_area,
        state.selected,
        state.display_order.len(),
    );

    let candidate = &state.candidates[state.selected_candidate_index()];
    let mut lines = vec![
        Line::from(Span::styled(
            candidate.skill.frontmatter.description.clone(),
            Style::default().fg(p.fg),
        )),
        Line::from(Span::styled(
            "─".repeat(preview_area.width.saturating_sub(2) as usize),
            Style::default().fg(p.border),
        )),
    ];
    lines.extend(restyle_markdown(
        tui_markdown::from_str(&candidate.skill.body),
        p,
    ));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(p.panel("Candidate", false))
            .wrap(Wrap { trim: false }),
        preview_area,
    );
}

// ── Status line ─────────────────────────────────────────────────────────

/// Hints are a function of focus, so the status line never advertises a key
/// that the current pane will refuse. Capped at five: the footer is scanned
/// peripherally, and past roughly five items it stops being read at all.
fn hints_for(app: &App) -> &'static [(&'static str, &'static str)] {
    match (&app.screen, app.focus) {
        (Screen::Discover(_), _) => &[
            ("j/k", "move"),
            ("spc", "toggle"),
            ("ent", "import"),
            ("esc", "cancel"),
        ],
        (Screen::Main, Focus::SkillList) => &[
            ("j/k", "move"),
            ("/", "filter"),
            ("tab", "pane"),
            ("S", "sync all"),
            ("?", "help"),
        ],
        (Screen::Main, Focus::Preview) => &[
            ("j/k", "scroll"),
            ("g/G", "top/end"),
            ("tab", "pane"),
            ("?", "help"),
        ],
        (Screen::Main, Focus::AgentMatrix) => &[
            ("j/k", "target"),
            ("spc", "toggle"),
            ("s", "sync skill"),
            ("tab", "pane"),
            ("?", "help"),
        ],
    }
}

fn level_color(p: &Palette, level: Level) -> Color {
    match level {
        Level::Muted => p.fg_dim,
        Level::Info => p.info,
        Level::Success => p.success,
        Level::Warning => p.warning,
        Level::Danger => p.danger,
    }
}

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    let p = &app.theme;

    // A running sync owns the whole line: it is the only thing happening,
    // and its progress is more useful than any key hint.
    if let Some(job) = &app.sync {
        draw_sync_status(frame, job, p, area);
        return;
    }

    // While typing a filter the line *is* the input — showing key hints
    // beside a live text cursor would imply those keys still work.
    if app.filter_active {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" / ", Style::default().fg(p.bg_base).bg(p.accent).bold()),
                Span::styled(format!(" {}", app.filter), Style::default().fg(p.fg)),
                Span::styled("█", Style::default().fg(p.accent)),
            ])),
            area,
        );
        return;
    }

    let (text, level) = app.status.resolve(std::time::Instant::now());
    let message = format!("{text} ");
    let width = (message.chars().count() as u16).min(area.width.saturating_sub(10));

    let [keys, status] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(width)]).areas(area);

    let spans: Vec<Span> = hints_for(app)
        .iter()
        .flat_map(|(k, l)| p.key_hint(k, l))
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            message,
            Style::default().fg(level_color(p, level)),
        )))
        .alignment(Alignment::Right),
        status,
    );
}

/// Braille dots: eight sub-positions in one cell, so rotation reads as
/// smooth motion rather than a character flickering between unrelated
/// shapes.
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Determinate wherever possible — the work is countable, and a bar that
/// answers "how much longer" is reassuring in a way a bare spinner is not.
/// The spinner rides alongside purely as a liveness signal, proving the
/// process is moving even while `done` sits on one slow skill.
fn draw_sync_status(frame: &mut Frame, job: &SyncJob, p: &Palette, area: Rect) {
    let ratio = if job.total == 0 {
        0.0
    } else {
        job.done as f64 / job.total as f64
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", SPINNER[job.frame % SPINNER.len()]),
                Style::default().fg(p.accent),
            ),
            Span::styled("syncing ", Style::default().fg(p.fg)),
            Span::styled(theme::meter(ratio, 12), Style::default().fg(p.accent)),
            Span::styled(
                format!("  {}/{}  ", job.done, job.total),
                Style::default().fg(p.fg),
            ),
            Span::styled(job.current.clone(), Style::default().fg(p.fg_dim)),
        ])),
        area,
    );
}

// ── Overlays (L2) ───────────────────────────────────────────────────────

/// Centres a fixed-size box in `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// Keys, grouped.
///
/// Eleven flat rows sit well past working-memory capacity; four labelled
/// groups of two to four items each are recallable, because the group name
/// is the thing being remembered rather than the individual bindings.
const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Navigate",
        &[
            ("j / k · ↓ ↑", "move in the focused pane"),
            ("g / G", "jump to first / last"),
            ("Tab · ⇧Tab", "cycle Table → Preview → Matrix"),
        ],
    ),
    (
        "Find",
        &[
            ("/", "filter by name, or description text"),
            ("Esc", "back out one level"),
        ],
    ),
    (
        "Act",
        &[
            ("Space", "toggle the selected target"),
            ("s", "sync the selected skill"),
            ("S", "sync everything (asks first)"),
            ("d", "discover skills already on disk"),
        ],
    ),
    ("Session", &[("?", "close this help"), ("q", "quit")]),
];

fn draw_help(frame: &mut Frame, p: &Palette, area: Rect) {
    let mut lines: Vec<Line> = theme::LOGO
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans = vec![
                Span::raw("   "),
                Span::styled(*row, Style::default().fg(p.accent)),
            ];
            if i == 1 {
                spans.push(Span::styled(
                    "   skill exchange",
                    Style::default().fg(p.fg_dim),
                ));
            }
            if i == 2 {
                spans.push(Span::styled(
                    format!("   v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(p.border),
                ));
            }
            Line::from(spans)
        })
        .collect();

    for (group, rows) in HELP_GROUPS {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("  {group}"),
            Style::default().fg(p.alt).bold(),
        )));
        for (keys, what) in *rows {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {keys:>14}  "),
                    Style::default().fg(p.accent).bold(),
                ),
                Span::styled(*what, Style::default().fg(p.fg)),
            ]));
        }
    }

    let height = lines.len() as u16 + 2;
    let area = centered(area, 60, height.min(area.height));

    // `Clear` first, or the dimmed panes bleed through the overlay.
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            p.panel("Keys", true)
                .style(Style::default().bg(p.bg_raised)),
        ),
        area,
    );
}

/// The content of a yes/no modal. One renderer for all of them, so every
/// confirmation in the app answers the same three questions in the same
/// places: what am I about to do, what does it cost, how do I back out.
struct Confirm {
    title: &'static str,
    question: String,
    detail: String,
    accept: &'static str,
    /// Colours the accept key by consequence — reassuring where the action
    /// is safe, cautionary where it isn't.
    tone: Color,
}

fn draw_confirm(frame: &mut Frame, p: &Palette, confirm: Confirm, area: Rect) {
    let width = 62.min(area.width);
    let area = centered(area, width, 9);
    frame.render_widget(Clear, area);

    let accept_chip = Style::default().fg(p.bg_base).bg(confirm.tone).bold();
    let cancel_chip = Style::default().fg(p.bg_base).bg(p.border).bold();

    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::styled(
                confirm.question,
                Style::default().fg(p.fg).bold(),
            ))
            .centered(),
            Line::raw(""),
            Line::from(Span::styled(confirm.detail, Style::default().fg(p.fg_dim))).centered(),
            Line::raw(""),
            Line::from(vec![
                Span::styled(" ↵ ", accept_chip),
                Span::styled(
                    format!(" {}    ", confirm.accept),
                    Style::default().fg(p.fg_dim),
                ),
                Span::styled(" Esc ", cancel_chip),
                Span::styled(" stay", Style::default().fg(p.fg_dim)),
            ])
            .centered(),
        ])
        .wrap(Wrap { trim: true })
        .block(
            p.panel(confirm.title, true)
                .style(Style::default().bg(p.bg_raised)),
        ),
        area,
    );
}

// ── Status mapping ──────────────────────────────────────────────────────

fn status_label(status: TargetStatus) -> &'static str {
    match status {
        TargetStatus::NotDeclared => "not declared",
        TargetStatus::NotSynced => "not synced",
        TargetStatus::InSync => "in sync",
        TargetStatus::NeedsAttention => "needs attention",
        TargetStatus::Error => "error",
    }
}

/// Every status colour goes through the shared severity ramp, so a red dot
/// in the table, a red bar in the matrix and a red count in the header all
/// mean the same degree of wrong.
fn status_style(p: &Palette, status: TargetStatus) -> Style {
    let color = match status {
        TargetStatus::NotDeclared => p.border,
        TargetStatus::InSync => p.severity(0.0),
        TargetStatus::NotSynced => p.severity(0.4),
        TargetStatus::NeedsAttention => p.severity(0.6),
        TargetStatus::Error => p.severity(1.0),
    };
    Style::default().fg(color)
}

/// The single worst status across a skill's declared targets, used for the
/// table's summary dot. `NotDeclared` only wins when nothing at all is
/// declared.
fn overall_status(loaded: &LoadedSkill) -> TargetStatus {
    fn severity(status: TargetStatus) -> u8 {
        match status {
            TargetStatus::Error => 4,
            TargetStatus::NeedsAttention => 3,
            TargetStatus::NotSynced => 2,
            TargetStatus::InSync => 1,
            TargetStatus::NotDeclared => 0,
        }
    }
    loaded
        .statuses
        .iter()
        .copied()
        .max_by_key(|s| severity(*s))
        .unwrap_or(TargetStatus::NotDeclared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, SyncScope};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use skx_core::{Manifest, ManifestEntry, Scope};

    const PLAIN: &str = "---\nname: rust-systems-expert\ndescription: Deep systems architectural conventions\nversion: 1.0.0\ntriggers:\n  - \"*.rs\"\ntargets:\n  cursor:\n    glob: \"**/*.rs\"\n---\n\n# Rust Systems Engineering\n\n- Prefer **zero-cost** abstractions.\n";

    /// Spec-compliant: author and version live under `metadata`.
    const AUTHORED: &str = "---\nname: pdf-processing\ndescription: Extract PDF text.\nlicense: Apache-2.0\nmetadata:\n  author: example-org\n  version: '2.1'\ntargets:\n  cursor:\n    glob: '**/*.pdf'\n---\n\n# PDF\nbody\n";

    struct Fixture {
        _dir: tempfile::TempDir,
        app: App,
    }

    fn fixture(skills: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let mut manifest = Manifest::default();
        for (name, raw) in skills {
            let path = skx_core::skill_path(Scope::Local, &root, &home, name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, raw).unwrap();
            manifest.upsert(ManifestEntry {
                name: name.to_string(),
                source: "x".to_string(),
                scope: Scope::Local,
                version: "0.1.0".to_string(),
            });
        }
        manifest.save(&skx_core::manifest_path(&root)).unwrap();
        Fixture {
            app: App::load(root, home).unwrap(),
            _dir: dir,
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Renders and returns the buffer, for assertions about colour rather
    /// than glyphs.
    fn render_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn draw_shows_every_pane() {
        let mut f = fixture(&[("rust-systems-expert", PLAIN)]);
        let text = render(&mut f.app, 120, 30);
        assert!(text.contains("rust-systems-expert"));
        assert!(text.contains("Skills"));
        assert!(text.contains("Agent Matrix"));
        assert!(text.contains("Preview"));
    }

    /// The highest-severity finding from the design audit: the palette was
    /// authored for a dark terminal and the app painted no background, so
    /// on a light profile every cell inherited an unknown colour.
    #[test]
    fn every_cell_gets_an_explicit_background() {
        let mut f = fixture(&[("rust-systems-expert", PLAIN)]);
        let expected = f.app.theme.bg_base;
        let buffer = render_buffer(&mut f.app, 80, 20);
        for (i, cell) in buffer.content().iter().enumerate() {
            assert_ne!(
                cell.bg,
                Color::Reset,
                "cell {i} inherits the terminal background"
            );
            // Everything is either the base or a deliberately raised surface.
            assert!(
                cell.bg == expected
                    || cell.bg == f.app.theme.bg_raised
                    || cell.bg == f.app.theme.bg_selected
                    || cell.bg == f.app.theme.accent,
                "cell {i} has an off-palette background {:?}",
                cell.bg
            );
        }
    }

    /// The table header and the selected row used to share `bg_selected`,
    /// which made the single most important state signal — where am I —
    /// indistinguishable from static chrome.
    #[test]
    fn the_header_row_is_distinguishable_from_the_selected_row() {
        let mut f = fixture(&[("a", PLAIN.replace("rust-systems-expert", "a").as_str())]);
        let buffer = render_buffer(&mut f.app, 100, 20);

        let row_bg = |y: u16| buffer.cell((5, y)).unwrap().bg;
        // Row 2 is the panel's top border, 3 the header, 4 the first row.
        assert_eq!(row_bg(3), f.app.theme.bg_raised);
        assert_eq!(row_bg(4), f.app.theme.bg_selected);
        assert_ne!(row_bg(3), row_bg(4));
    }

    #[test]
    fn preview_renders_markdown_rather_than_raw_syntax() {
        let mut f = fixture(&[("rust-systems-expert", PLAIN)]);
        let text = render(&mut f.app, 120, 40);
        assert!(text.contains("Rust Systems Engineering"));
        assert!(!text.contains("**zero-cost**"));
        assert!(!text.contains("# Rust Systems"));
        assert!(text.contains("▌"), "heading rule missing");
    }

    #[test]
    fn the_table_shows_author_version_and_token_columns() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        let text = render(&mut f.app, 120, 24);
        assert!(text.contains("AUTHOR") && text.contains("TOKENS"));
        assert!(text.contains("example-org"));
        assert!(text.contains("2.1"), "metadata version should win");
    }

    #[test]
    fn matrix_column_codes_are_unambiguous() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        let text = render(&mut f.app, 120, 24);
        for code in ["AG", "CL", "CU", "CP", "MC"] {
            assert!(text.contains(code), "missing matrix code {code}");
        }
    }

    /// Spare width has to become information, not a gap.
    ///
    /// It used to be an empty spacer pinning the matrix right, which on a
    /// wide terminal opened ~110 columns of nothing between a row's name
    /// and its status glyphs — far enough that associating the two took a
    /// deliberate saccade across dead space.
    #[test]
    fn slack_width_is_spent_on_the_description_not_left_empty() {
        // A realistically long description — real skill descriptions run to
        // hundreds of characters — so this measures the layout rather than
        // trailing space inside a legitimately short cell.
        let verbose = format!(
            "---\nname: verbose\ndescription: {}\n---\nbody\n",
            "When the user wants to do the thing that this skill does, and also \
             when they mention any of a long list of trigger phrases that keeps \
             going well past the width of any terminal anyone owns"
        );
        let mut f = fixture(&[("verbose", &verbose)]);
        let buffer = render_buffer(&mut f.app, 200, 12);

        let row: String = (0..200)
            .map(|c| buffer.cell((c, 4)).unwrap().symbol())
            .collect();
        assert!(
            row.contains("When the user wants"),
            "description column missing"
        );

        // No run of blank cells wide enough to read as a gap.
        let widest_gap = row
            .split(|c: char| c != ' ')
            .map(str::len)
            .max()
            .unwrap_or(0);
        assert!(widest_gap < 40, "row has a {widest_gap}-column dead gap");
    }

    /// A column where every value is a placeholder is worse than no column.
    #[test]
    fn the_author_column_disappears_when_nothing_declares_one() {
        let anonymous = "---\nname: anon\ndescription: no author here.\n---\nbody\n";

        let mut without = fixture(&[("anon", anonymous)]);
        let text = render(&mut without.app, 200, 12);
        assert!(
            !text.contains("AUTHOR"),
            "empty AUTHOR column should be dropped"
        );

        let mut with = fixture(&[("anon", anonymous), ("pdf-processing", AUTHORED)]);
        let text = render(&mut with.app, 200, 12);
        assert!(
            text.contains("AUTHOR"),
            "one authored skill should bring it back"
        );
        assert!(text.contains("example-org"));
    }

    #[test]
    fn the_artifacts_pane_fills_the_space_the_matrix_does_not_need() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        let text = render(&mut f.app, 120, 34);
        assert!(text.contains("Artifacts"));
        assert!(text.contains("nothing written yet"));
    }

    #[test]
    fn long_paths_elide_from_the_left_to_keep_the_filename() {
        let kept = elide_left("/very/long/prefix/that/does/not/fit/SKILL.md", 20);
        assert!(
            kept.contains("SKILL.md"),
            "the informative end must survive"
        );
        assert!(kept.contains('…'));
        assert!(kept.chars().count() <= 21);
        // Short enough to fit is returned intact.
        assert_eq!(elide_left("a.md", 20), " a.md");
    }

    #[test]
    fn a_description_is_cut_at_the_first_sentence() {
        // Skill descriptions are written for a model: hundreds of
        // characters enumerating trigger phrases. Only the opening sentence
        // says what the skill actually is.
        assert_eq!(
            first_sentence("Does a thing. Also use when the user mentions 'x', 'y'."),
            "Does a thing"
        );
        assert_eq!(first_sentence("No trailing period"), "No trailing period");
    }

    #[test]
    fn columns_drop_by_priority_as_the_terminal_narrows() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        let wide = render(&mut f.app, 120, 24);
        assert!(wide.contains("AUTHOR") && wide.contains("TOKENS"));

        let narrow = render(&mut f.app, 40, 24);
        assert!(!narrow.contains("AUTHOR"));
        assert!(narrow.contains("NAME") && narrow.contains("pdf-process"));
    }

    /// Hints must never advertise a key the focused pane will refuse.
    #[test]
    fn status_hints_follow_focus() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);

        let table = render(&mut f.app, 120, 24);
        assert!(table.contains("filter"));

        f.app.focus = Focus::Preview;
        let preview = render(&mut f.app, 120, 24);
        assert!(preview.contains("scroll"));

        f.app.focus = Focus::AgentMatrix;
        let matrix = render(&mut f.app, 120, 24);
        assert!(matrix.contains("toggle") && matrix.contains("sync skill"));
    }

    #[test]
    fn a_running_sync_shows_determinate_progress_not_a_frozen_screen() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        f.app.start_sync(SyncScope::All);
        let text = render(&mut f.app, 120, 24);
        assert!(text.contains("syncing"));
        assert!(
            text.contains("0/1") || text.contains("1/1"),
            "no progress count"
        );
    }

    #[test]
    fn the_help_overlay_is_grouped_and_dims_what_is_behind_it() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        f.app.toggle_help();
        let text = render(&mut f.app, 120, 34);
        for group in ["Navigate", "Find", "Act", "Session"] {
            assert!(text.contains(group), "help group {group} missing");
        }
        assert!(text.contains("skill exchange"), "wordmark missing");

        // The workspace behind the modal is knocked back to border colour.
        let buffer = render_buffer(&mut f.app, 120, 34);
        assert_eq!(buffer.cell((1, 4)).unwrap().fg, f.app.theme.border);
    }

    #[test]
    fn syncing_everything_renders_a_confirmation_first() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        f.app.request_sync(SyncScope::All);
        let text = render(&mut f.app, 120, 30);
        assert!(text.contains("Sync all 1 skills?"));
        assert!(text.contains("stay"));
    }

    #[test]
    fn quitting_asks_and_offers_a_way_back() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        f.app.request_quit();
        let text = render(&mut f.app, 120, 30);
        assert!(text.contains("Quit?"));
        assert!(text.contains("stay"), "must offer a way to stay");
        assert!(
            !f.app.should_quit,
            "the prompt must not pre-commit the exit"
        );
    }

    /// The prompt has to say what leaving would cost, or it is just a speed
    /// bump people learn to dismiss.
    #[test]
    fn the_quit_prompt_names_unsynced_work() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        // The fixture declares cursor but has never synced.
        assert!(f.app.pending_sync_count() > 0);
        f.app.request_quit();
        let text = render(&mut f.app, 120, 30);
        assert!(text.contains("never written"));
    }

    #[test]
    fn a_no_color_run_still_renders() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        f.app.theme = Palette::NO_COLOR;
        let text = render(&mut f.app, 120, 30);
        // Status is encoded in glyphs as well as colour, so removing hue
        // costs no information.
        assert!(text.contains("pdf-processing"));
        assert!(text.contains("○") || text.contains("●") || text.contains("·"));
    }

    #[test]
    fn the_empty_state_shows_the_wordmark() {
        let mut f = fixture(&[]);
        let text = render(&mut f.app, 90, 24);
        assert!(text.contains("skill exchange") && text.contains("No skills installed"));
    }

    #[test]
    fn the_wordmark_rows_stay_aligned_when_centred() {
        let mut f = fixture(&[]);
        let buffer = render_buffer(&mut f.app, 90, 24);
        let left_edge = |row: u16| -> Option<u16> {
            (0..90).find(|&col| {
                let s = buffer.cell((col, row)).unwrap().symbol();
                !matches!(s, " " | "│" | "╭" | "╰")
            })
        };
        let first = (0..24)
            .find(|&r| {
                (0..90)
                    .map(|c| buffer.cell((c, r)).unwrap().symbol())
                    .collect::<String>()
                    .contains("█▀▀")
            })
            .expect("logo not found");
        assert_eq!(left_edge(first), left_edge(first + 1));
        assert_eq!(left_edge(first), left_edge(first + 2));
    }

    #[test]
    fn draw_survives_a_tiny_terminal() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        render(&mut f.app, 10, 5);
        render(&mut f.app, 1, 1);
        f.app.toggle_help();
        render(&mut f.app, 12, 6);
    }

    #[test]
    fn the_discover_drawer_previews_the_selected_candidate() {
        let mut f = fixture(&[("pdf-processing", AUTHORED)]);
        let found = f.app.root.join(".claude/skills/found-me/SKILL.md");
        std::fs::create_dir_all(found.parent().unwrap()).unwrap();
        std::fs::write(
            &found,
            "---\nname: found-me\ndescription: a discovered skill\n---\n# Discovered body\n",
        )
        .unwrap();
        f.app.open_discover();

        let text = render(&mut f.app, 110, 30);
        assert!(text.contains("Discover") && text.contains("found-me"));
        // Choosing between same-named candidates on a path string alone was
        // the gap; the drawer now shows what each one actually contains.
        assert!(text.contains("Candidate"));
        assert!(text.contains("Discovered body"));
    }
}
