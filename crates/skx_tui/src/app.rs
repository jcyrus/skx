//! The TUI's data model: what's loaded, what's selected, and the pure
//! logic behind the two mutating actions (toggling a target, syncing
//! everything). Kept free of any ratatui/crossterm imports so it can be
//! exercised by plain unit tests without a terminal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use skx_adapters::{LinkStrategy, SkillAdapter};
use skx_core::{
    CompileCtx, DiscoveredSkill, DriftStatus, Manifest, ManifestEntry, Skill, StateFile,
};

use crate::theme::{ColorDepth, Palette};

/// Target ids shown as columns in the Agent Matrix pane, in display order.
/// `"mcp"` is last and isn't toggleable — it's driven by `mcp_dependencies`,
/// not a `targets.*` block (see `skx_adapters::McpAdapter`'s doc comment).
pub const MATRIX_COLUMNS: &[&str] = &["antigravity", "claude_code", "cursor", "copilot", "mcp"];

/// Added to every name-match score in `recompute_visible` to lift the whole
/// name tier above the description tier. Larger than any score nucleo
/// produces for the short strings involved.
const SCORE_NAME_TIER: u32 = 1_000_000;

/// Where one target stands for one skill, relative to the recorded sync
/// state (not a live filesystem re-check — that's `skx audit`'s job; the
/// TUI shows the last-known picture and lets `s` refresh it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetStatus {
    /// The skill doesn't declare this target.
    NotDeclared,
    /// Declared, but never synced — no recorded artifact yet.
    NotSynced,
    /// Declared and in sync as of the last sync/audit computation.
    InSync,
    /// Declared, but at least one artifact is missing, user-modified, or
    /// stale relative to a fresh compile.
    NeedsAttention,
    /// The adapter failed to compile this skill for this target (e.g.
    /// Cursor with no glob and no triggers).
    Error,
}

impl TargetStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            TargetStatus::NotDeclared => "·",
            TargetStatus::NotSynced => "○",
            TargetStatus::InSync => "●",
            TargetStatus::NeedsAttention => "▲",
            TargetStatus::Error => "✕",
        }
    }
}

/// One manifest entry plus its parsed skill (if the cache copy still loads)
/// and its computed per-target status row.
pub struct LoadedSkill {
    pub entry: ManifestEntry,
    pub skill: Option<Skill>,
    pub load_error: Option<String>,
    /// Aligned with [`MATRIX_COLUMNS`] by index.
    pub statuses: Vec<TargetStatus>,
    /// Approximate context cost in tokens, derived once at load time so
    /// the renderer doesn't re-measure every skill on every frame.
    pub tokens: usize,
}

impl LoadedSkill {
    /// Who wrote this skill. Attribution comes from the spec's
    /// `metadata.author`; skills that don't declare one are shown as
    /// unknown rather than guessed at, since inferring an author from a
    /// file path would confidently misattribute vendored skills.
    pub fn author(&self) -> &str {
        self.skill
            .as_ref()
            .and_then(|s| s.frontmatter.author())
            .unwrap_or("—")
    }

    /// The version to display: the manifest's copy is a snapshot from
    /// install time, so the skill's own frontmatter wins when they differ.
    pub fn version(&self) -> &str {
        self.skill
            .as_ref()
            .map(|s| s.frontmatter.effective_version())
            .unwrap_or(&self.entry.version)
    }
}

/// A pane's on-screen rectangle, recorded by the renderer so pointer
/// events can be routed back to whatever was actually clicked.
///
/// Deliberately not `ratatui::Rect`: hit-testing is app logic and stays
/// unit-testable without constructing a terminal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Region {
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// The zero-based row inside this region, ignoring a `chrome`-row
    /// offset for the border and any header.
    pub fn row_at(self, y: u16, chrome: u16) -> Option<usize> {
        let first = self.y + chrome;
        (y >= first && y < self.y + self.height.saturating_sub(1)).then(|| (y - first) as usize)
    }
}

/// Where each pane was drawn on the last frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneRects {
    pub table: Region,
    pub preview: Region,
    pub matrix: Region,
}

/// Which pane `j`/`k` currently drives. Preview is in the cycle: scrolling
/// a skill body is the most frequent reading action there is, and hiding it
/// behind a `Shift`-chord made it both a two-key gesture and undiscoverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    SkillList,
    Preview,
    AgentMatrix,
}

/// How urgent a status message is; drives its colour in the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Muted,
    Info,
    Success,
    Warning,
    Danger,
}

/// A status-line message with a lifetime.
///
/// Transient messages decay back to a neutral hint. A message that outlives
/// the thing it describes is worse than none: it asserts a stale state with
/// exactly the same confidence as a live one.
#[derive(Debug, Clone)]
pub struct Status {
    text: String,
    level: Level,
    expires_at: Option<Instant>,
}

impl Status {
    pub fn idle() -> Self {
        Self {
            text: "ready".to_string(),
            level: Level::Muted,
            expires_at: None,
        }
    }

    /// Expires after `TTL`, then reads as idle.
    const TTL: Duration = Duration::from_secs(6);

    pub fn transient(text: String, level: Level, now: Instant) -> Self {
        Self {
            text,
            level,
            expires_at: Some(now + Self::TTL),
        }
    }

    /// Sticky — for states the user must actively resolve (errors, or a
    /// mode they are currently inside), which must not quietly vanish.
    pub fn sticky(text: String, level: Level) -> Self {
        Self {
            text,
            level,
            expires_at: None,
        }
    }

    pub fn resolve(&self, now: Instant) -> (&str, Level) {
        match self.expires_at {
            Some(deadline) if now >= deadline => ("ready", Level::Muted),
            _ => (&self.text, self.level),
        }
    }

    /// The message as written, ignoring expiry — for tests and logging.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn has_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|deadline| now >= deadline)
    }
}

/// A blocking modal above the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Help,
    /// Syncing every skill writes across the whole workspace, so it asks
    /// first — the one action here whose blast radius exceeds the row the
    /// cursor is on.
    ConfirmSyncAll {
        skills: usize,
    },
    /// Asked on the way out. `pending` is how many skills have declared
    /// targets that were never written, which is the only thing an exit
    /// can actually cost you.
    ConfirmQuit {
        pending: usize,
    },
}

/// What a sync run should cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncScope {
    Selected,
    All,
}

/// Progress messages from the sync worker to the UI thread.
pub enum SyncProgress {
    Skill {
        name: String,
        done: usize,
    },
    Finished {
        written: usize,
        state: Box<StateFile>,
    },
    Failed(String),
}

/// A sync in flight.
///
/// The worker owns the I/O and the UI thread only ever reads progress, so
/// the event loop keeps repainting while 53 skills are compiled and
/// written. Running this inline froze the terminal for the duration, which
/// is indistinguishable from a hang.
pub struct SyncJob {
    progress: mpsc::Receiver<SyncProgress>,
    pub done: usize,
    pub total: usize,
    pub current: String,
    /// Advanced once per repaint rather than per unit of work: a spinner
    /// tied to throughput stutters, and a stuttering spinner reads as a
    /// hang — the exact anxiety it exists to prevent.
    pub frame: usize,
}

/// Which full-screen view is active. `Discover` carries its own state so an
/// import review can't be "half open" — either there's a `DiscoverState` to
/// operate on, or the screen is `Main` and there isn't.
pub enum Screen {
    Main,
    Discover(DiscoverState),
}

/// One `d`-keypress's worth of discovery: everything found on this pass,
/// grouped by name so same-named finds from different old projects show up
/// as a single conflict to resolve rather than two independent rows.
pub struct DiscoverState {
    pub candidates: Vec<DiscoveredSkill>,
    pub groups: BTreeMap<String, Vec<usize>>,
    /// Candidate indices in the order they're displayed — every conflict's
    /// alternatives adjacent, grouped alphabetically by name. `selected` is
    /// a position in *this* order, not a raw index into `candidates`, so
    /// `j`/`k` always moves exactly one row on screen regardless of the
    /// order candidates were discovered in.
    pub display_order: Vec<usize>,
    /// Aligned with `candidates` by index. At most one `true` per name
    /// group at any time — see `toggle_selected`.
    pub included: Vec<bool>,
    pub selected: usize,
}

impl DiscoverState {
    fn new(candidates: Vec<DiscoveredSkill>) -> Self {
        let groups = skx_core::group_by_name(&candidates);
        let mut included = vec![false; candidates.len()];
        for indices in groups.values() {
            included[skx_core::default_pick(&candidates, indices)] = true;
        }
        let display_order: Vec<usize> = groups.values().flatten().copied().collect();
        Self {
            candidates,
            groups,
            display_order,
            included,
            selected: 0,
        }
    }

    pub fn selected_candidate_index(&self) -> usize {
        self.display_order[self.selected]
    }

    /// Toggles inclusion of the selected candidate. Turning one on turns
    /// off every other candidate with the same name — the manifest can
    /// only hold one entry per name, so including two at once would just
    /// mean the second silently overwrites the first at commit time.
    fn toggle_selected(&mut self) {
        let i = self.selected_candidate_index();
        if self.included[i] {
            self.included[i] = false;
            return;
        }
        let name = self.candidates[i].skill.frontmatter.name.to_string();
        for &j in &self.groups[&name] {
            self.included[j] = false;
        }
        self.included[i] = true;
    }

    fn move_selection(&mut self, delta: isize) {
        self.selected = clamp_index(self.selected, delta, self.display_order.len());
    }

    pub fn included_count(&self) -> usize {
        self.included.iter().filter(|&&b| b).count()
    }
}

pub struct App {
    pub root: PathBuf,
    pub home: PathBuf,
    pub manifest: Manifest,
    pub state: StateFile,
    pub skills: Vec<LoadedSkill>,
    pub selected: usize,
    pub matrix_selected: usize,
    pub focus: Focus,
    pub screen: Screen,
    /// Vertical scroll offset for the preview pane, in rendered lines.
    pub preview_scroll: u16,
    /// Height of the preview viewport as of the last render, so `move_preview`
    /// can clamp scrolling to the actual content instead of letting the user
    /// page off the bottom into blank space. Set by `ui::draw`.
    pub preview_viewport: u16,
    /// Total rendered line count of the current preview, same source.
    pub preview_len: u16,
    /// The blocking modal above the workspace, if any.
    pub overlay: Option<Overlay>,
    /// A sync running on a worker thread.
    pub sync: Option<SyncJob>,
    pub theme: Palette,
    pub config: skx_core::Config,
    /// Pane geometry from the last render, for pointer hit-testing.
    pub panes: PaneRects,
    /// Active fuzzy filter over the skill list. Empty means "show everything".
    pub filter: String,
    /// Whether keystrokes are being captured into `filter` (`/` mode).
    pub filter_active: bool,
    /// Indices into `skills`, in display order, after `filter` is applied.
    /// `selected` indexes *this*, not `skills` — see `selected_index`.
    pub visible: Vec<usize>,
    pub status: Status,
    pub should_quit: bool,
    /// Set by `Ctrl-Z`; the event loop owns the terminal, so it performs
    /// the actual suspend and clears this.
    pub suspend_requested: bool,
}

/// Aggregate drift picture across every installed skill, for the header
/// dashboard. Counts are per (skill, declared target) pair rather than per
/// skill, so one skill drifting on one of four targets moves the meter by
/// a quarter of a skill instead of the whole thing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Health {
    pub skills: usize,
    /// Combined approximate context cost of every installed skill — the
    /// scarce resource a skill manager actually spends.
    pub tokens: usize,
    pub declared: usize,
    pub in_sync: usize,
    pub not_synced: usize,
    pub needs_attention: usize,
    pub error: usize,
}

impl Health {
    /// Fraction of declared targets that are in sync, for the header meter.
    /// A workspace that declares nothing reads as 1.0 (nothing is wrong)
    /// rather than 0.0 — an empty workspace isn't a broken one.
    pub fn ratio(self) -> f64 {
        if self.declared == 0 {
            return 1.0;
        }
        self.in_sync as f64 / self.declared as f64
    }
}

impl App {
    pub fn load(root: PathBuf, home: PathBuf) -> anyhow::Result<Self> {
        Self::load_with(root, home, skx_core::Config::default())
    }

    pub fn load_with(
        root: PathBuf,
        home: PathBuf,
        config: skx_core::Config,
    ) -> anyhow::Result<Self> {
        let manifest = Manifest::load(&skx_core::manifest_path(&root))?;
        let state = StateFile::load(&skx_core::state_path(&root))?;
        let mut skills = load_skills(&manifest, &root, &home);
        compute_statuses(&mut skills, &root, &home, &state);
        let skill_count = skills.len();

        Ok(Self {
            root,
            home,
            manifest,
            state,
            skills,
            selected: 0,
            matrix_selected: 0,
            focus: Focus::SkillList,
            screen: Screen::Main,
            preview_scroll: 0,
            preview_viewport: 0,
            preview_len: 0,
            overlay: None,
            sync: None,
            theme: resolve_palette(&config),
            config,
            panes: PaneRects::default(),
            filter: String::new(),
            filter_active: false,
            visible: (0..skill_count).collect(),
            status: Status::idle(),
            should_quit: false,
            suspend_requested: false,
        })
    }

    /// Scans for unmanaged skills (global caches + this workspace, same
    /// bounded scope as `skx discover`) and opens the review screen if it
    /// found anything. A clean scan just leaves a status message — there's
    /// nothing to review.
    pub fn open_discover(&mut self) {
        let found =
            skx_core::scan_for_unmanaged_skills(&self.manifest, &self.root, &self.home, &[]);
        if found.is_empty() {
            self.set_status("No unmanaged skills found.", Level::Info);
            return;
        }
        self.status = Status::sticky(
            format!(
                "{} unmanaged skill(s) found — Space toggle · Enter import · Esc cancel",
                found.len()
            ),
            Level::Info,
        );
        self.screen = Screen::Discover(DiscoverState::new(found));
    }

    pub fn cancel_discover(&mut self) {
        self.screen = Screen::Main;
        self.set_status("discover cancelled — nothing imported", Level::Info);
    }

    pub fn toggle_discover_selected(&mut self) {
        if let Screen::Discover(state) = &mut self.screen {
            state.toggle_selected();
        }
    }

    pub fn move_discover_selection(&mut self, delta: isize) {
        if let Screen::Discover(state) = &mut self.screen {
            state.move_selection(delta);
        }
    }

    /// Copies every included candidate into the cache and registers it in
    /// the manifest, then returns to the main screen and reloads the skill
    /// list so the import shows up immediately.
    pub fn commit_discover(&mut self) {
        let Screen::Discover(state) = std::mem::replace(&mut self.screen, Screen::Main) else {
            return;
        };

        let mut imported = 0usize;
        for (i, candidate) in state.candidates.iter().enumerate() {
            if !state.included[i] {
                continue;
            }
            let dest = skx_core::skill_path(
                candidate.scope_hint,
                &self.root,
                &self.home,
                candidate.skill.frontmatter.name.as_str(),
            );
            let Some(parent) = dest.parent() else {
                continue;
            };
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }

            // Keep the skill working exactly where it was already being
            // read from directly: if it was found under a Claude Code or
            // Antigravity skills directory and doesn't already declare
            // that target, declare it now. Without this, an imported
            // skill sits inert until someone manually toggles a target on
            // for it in the Agent Matrix — defeating the point of a bulk
            // import from "a lot of skills already on the machine".
            let mut skill = candidate.skill.clone();
            if let Some(key) = candidate.found_in.default_target_key() {
                skill
                    .frontmatter
                    .targets
                    .entry(key.to_string())
                    .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
            }
            let Ok(rendered) = skx_core::render_skill(&skill) else {
                continue;
            };
            if std::fs::write(&dest, rendered).is_err() {
                continue;
            }
            self.manifest.upsert(ManifestEntry {
                name: candidate.skill.frontmatter.name.to_string(),
                source: candidate.path.display().to_string(),
                scope: candidate.scope_hint,
                version: candidate.skill.frontmatter.version.clone(),
            });
            imported += 1;
        }

        match self.manifest.save(&skx_core::manifest_path(&self.root)) {
            Ok(()) => {
                self.skills = load_skills(&self.manifest, &self.root, &self.home);
                compute_statuses(&mut self.skills, &self.root, &self.home, &self.state);
                self.recompute_visible();
                self.set_status(
                    &format!("imported {imported} skill(s) — press S to sync them"),
                    Level::Success,
                );
            }
            Err(e) => {
                self.set_sticky(
                    format!("import wrote files but failed to save manifest: {e}"),
                    Level::Danger,
                );
            }
        }
    }

    /// The `skills` index currently under the cursor, resolved through the
    /// filter. `None` when the filter matched nothing (or nothing is
    /// installed), which every caller has to handle anyway.
    pub fn selected_index(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    pub fn selected_skill(&self) -> Option<&LoadedSkill> {
        self.skills.get(self.selected_index()?)
    }

    /// Rolls up every skill's per-target statuses into one dashboard figure.
    pub fn health(&self) -> Health {
        let mut health = Health {
            skills: self.skills.len(),
            ..Health::default()
        };
        for loaded in &self.skills {
            health.tokens += loaded.tokens;
            for status in &loaded.statuses {
                match status {
                    TargetStatus::NotDeclared => continue,
                    TargetStatus::InSync => health.in_sync += 1,
                    TargetStatus::NotSynced => health.not_synced += 1,
                    TargetStatus::NeedsAttention => health.needs_attention += 1,
                    TargetStatus::Error => health.error += 1,
                }
                health.declared += 1;
            }
        }
        health
    }

    pub fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::SkillList => {
                if self.visible.is_empty() {
                    return;
                }
                self.selected = clamp_index(self.selected, delta, self.visible.len());
                // A different skill means the old scroll offset is
                // meaningless — jumping to line 400 of a two-line skill
                // would show an empty pane.
                self.preview_scroll = 0;
            }
            Focus::Preview => self.move_preview(delta),
            Focus::AgentMatrix => {
                self.matrix_selected =
                    clamp_index(self.matrix_selected, delta, MATRIX_COLUMNS.len());
            }
        }
    }

    /// Jumps the skill cursor to the first or last visible row.
    pub fn jump_selection(&mut self, to_end: bool) {
        match self.focus {
            Focus::Preview => {
                self.preview_scroll = if to_end {
                    self.preview_len.saturating_sub(1)
                } else {
                    0
                };
            }
            Focus::AgentMatrix => {
                self.matrix_selected = if to_end { MATRIX_COLUMNS.len() - 1 } else { 0 };
            }
            Focus::SkillList => {
                if self.visible.is_empty() {
                    return;
                }
                self.selected = if to_end { self.visible.len() - 1 } else { 0 };
                self.preview_scroll = 0;
            }
        }
    }

    /// Scrolls the preview pane, clamped so the last line can reach the top
    /// of the viewport but no further.
    pub fn move_preview(&mut self, delta: isize) {
        let max = self.preview_len.saturating_sub(1) as isize;
        let next = (self.preview_scroll as isize + delta).clamp(0, max.max(0));
        self.preview_scroll = next as u16;
    }

    /// One viewport's worth of preview scroll, minus a line of overlap so
    /// the reader keeps their place across the jump.
    pub fn page_preview(&mut self, down: bool) {
        let page = self.preview_viewport.saturating_sub(1).max(1) as isize;
        self.move_preview(if down { page } else { -page });
    }

    // ── Status & overlays ───────────────────────────────────────────────

    /// A message that decays back to the idle hint on its own.
    pub fn set_status(&mut self, text: &str, level: Level) {
        self.status = Status::transient(text.to_string(), level, Instant::now());
    }

    /// A message that stays until something replaces it — errors, and modes
    /// the user is currently inside.
    pub fn set_sticky(&mut self, text: String, level: Level) {
        self.status = Status::sticky(text, level);
    }

    pub fn status_expired(&self) -> bool {
        self.status.has_expired(Instant::now())
    }

    pub fn toggle_help(&mut self) {
        self.overlay = match self.overlay {
            Some(Overlay::Help) => None,
            _ => Some(Overlay::Help),
        };
    }

    /// Backs out exactly one level of nesting. Never quits: `Esc` means
    /// "undo the last thing that put me here", and wiring it to termination
    /// meant every safe use of the key trained a reflex that eventually
    /// killed the session.
    pub fn escape(&mut self) {
        if self.overlay.take().is_some() {
            return;
        }
        if !self.filter.is_empty() || self.filter_active {
            self.clear_filter();
            return;
        }
        if matches!(self.screen, Screen::Discover(_)) {
            self.cancel_discover();
            return;
        }
        if self.focus != Focus::SkillList {
            self.focus = Focus::SkillList;
            return;
        }
        self.set_status("nothing to go back to — press q to quit", Level::Muted);
    }

    pub fn confirm_overlay(&mut self) {
        match self.overlay.take() {
            Some(Overlay::ConfirmSyncAll { .. }) => self.start_sync(SyncScope::All),
            Some(Overlay::ConfirmQuit { .. }) => self.should_quit = true,
            Some(Overlay::Help) | None => {}
        }
    }

    /// How many skills declare a target that has never been written.
    ///
    /// This is the only thing quitting can actually cost: `skx` persists
    /// target toggles to the skill file immediately, so the unsaved work is
    /// never the *edit* — it's the sync that hasn't happened yet.
    pub fn pending_sync_count(&self) -> usize {
        self.skills
            .iter()
            .filter(|loaded| {
                loaded
                    .statuses
                    .iter()
                    .any(|s| matches!(s, TargetStatus::NotSynced | TargetStatus::NeedsAttention))
            })
            .count()
    }

    /// Handles `q`.
    ///
    /// Always asks when there is something to lose — unsynced changes, or a
    /// sync still running — regardless of preference, because that is the
    /// case a confirmation exists for. Otherwise it obeys `confirm_quit`.
    /// A prompt shown on every quiet exit is one people learn to dismiss
    /// without reading, which is exactly how a confirmation stops working.
    pub fn request_quit(&mut self) {
        let pending = self.pending_sync_count();
        if pending > 0 || self.sync.is_some() {
            self.overlay = Some(Overlay::ConfirmQuit { pending });
            return;
        }
        if self.config.confirm_quit {
            self.overlay = Some(Overlay::ConfirmQuit { pending: 0 });
            return;
        }
        self.should_quit = true;
    }

    /// Leaves immediately, no questions asked — for `Ctrl-C`, which every
    /// terminal user expects to mean exactly that.
    pub fn force_quit(&mut self) {
        self.should_quit = true;
    }

    // ── Pointer ─────────────────────────────────────────────────────────

    /// Routes a click. Returns whether anything changed.
    ///
    /// Clicking a pane focuses it, and clicking a row inside the table or
    /// matrix also selects that row — one gesture doing the obvious thing,
    /// rather than requiring a focus click and then a select click.
    pub fn click(&mut self, x: u16, y: u16) -> bool {
        // An overlay owns the screen; a stray click behind it must not
        // quietly change the selection the user will return to.
        if self.overlay.is_some() || !matches!(self.screen, Screen::Main) {
            return false;
        }

        if self.panes.table.contains(x, y) {
            self.focus = Focus::SkillList;
            // Two rows of chrome: the panel border and the column header.
            if let Some(row) = self.panes.table.row_at(y, 2)
                && row < self.visible.len()
            {
                self.selected = row;
                self.preview_scroll = 0;
            }
            return true;
        }
        if self.panes.preview.contains(x, y) {
            self.focus = Focus::Preview;
            return true;
        }
        if self.panes.matrix.contains(x, y) {
            self.focus = Focus::AgentMatrix;
            if let Some(row) = self.panes.matrix.row_at(y, 1)
                && row < MATRIX_COLUMNS.len()
            {
                self.matrix_selected = row;
            }
            return true;
        }
        false
    }

    /// Routes a scroll-wheel tick to whichever pane is under the pointer —
    /// not to whatever holds focus, because the mouse's own position is a
    /// clearer statement of intent than the keyboard's.
    pub fn scroll_at(&mut self, x: u16, y: u16, delta: isize) -> bool {
        if self.overlay.is_some() {
            return false;
        }
        if self.panes.preview.contains(x, y) {
            self.move_preview(delta);
            return true;
        }
        // Scrolling moves the pane's own cursor but doesn't take the focus
        // ring: the wheel is a glance, not a commitment, and stealing focus
        // would silently change what the next keystroke does.
        if self.panes.table.contains(x, y) {
            if self.visible.is_empty() {
                return false;
            }
            self.selected = clamp_index(self.selected, delta, self.visible.len());
            self.preview_scroll = 0;
            return true;
        }
        if self.panes.matrix.contains(x, y) {
            self.matrix_selected = clamp_index(self.matrix_selected, delta, MATRIX_COLUMNS.len());
            return true;
        }
        false
    }

    /// Asks before a full-workspace write; a single-skill sync is scoped to
    /// the row under the cursor and goes straight through.
    pub fn request_sync(&mut self, scope: SyncScope) {
        match scope {
            SyncScope::Selected => self.start_sync(SyncScope::Selected),
            SyncScope::All => {
                self.overlay = Some(Overlay::ConfirmSyncAll {
                    skills: self.skills.len(),
                })
            }
        }
    }

    // ── Filtering ───────────────────────────────────────────────────────

    pub fn begin_filter(&mut self) {
        self.filter_active = true;
        self.status = Status::sticky(
            "filter: type to narrow · Enter keep · Esc clear".to_string(),
            Level::Info,
        );
    }

    /// Leaves input mode but keeps whatever was typed, so the list stays
    /// narrowed while `j`/`k`/`Space`/`s` work normally again.
    pub fn commit_filter(&mut self) {
        self.filter_active = false;
        self.status = if self.filter.is_empty() {
            Status::idle()
        } else {
            Status::sticky(
                format!("filtered by \"{}\" — Esc to clear", self.filter),
                Level::Info,
            )
        };
    }

    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_active = false;
        self.recompute_visible();
        self.set_status("filter cleared", Level::Info);
    }

    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.recompute_visible();
    }

    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.recompute_visible();
    }

    /// Recomputes `visible` from `filter`. An empty filter is identity
    /// (manifest order); otherwise rows are matched and reordered
    /// best-match-first.
    ///
    /// Names are matched **fuzzily** (so `pgseo` finds `programmatic-seo`)
    /// but descriptions only by literal substring. Fuzzy-matching a
    /// paragraph-length description is worse than useless: a three-letter
    /// query like `seo` appears as a subsequence in nearly every prose
    /// blurb, so every skill "matches" and the filter stops filtering.
    /// Name hits always outrank description hits.
    pub fn recompute_visible(&mut self) {
        if self.filter.trim().is_empty() {
            self.visible = (0..self.skills.len()).collect();
        } else {
            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(&self.filter, CaseMatching::Ignore, Normalization::Smart);
            let needle = self.filter.to_lowercase();

            let mut scored: Vec<(usize, u32)> = self
                .skills
                .iter()
                .enumerate()
                .filter_map(|(i, loaded)| {
                    let mut buf = Vec::new();
                    let name_score =
                        pattern.score(Utf32Str::new(&loaded.entry.name, &mut buf), &mut matcher);
                    if let Some(score) = name_score {
                        // Offset name hits clear of every description hit
                        // so the two tiers never interleave.
                        return Some((i, score.saturating_add(SCORE_NAME_TIER)));
                    }
                    let hit = loaded.author().to_lowercase().contains(&needle)
                        || loaded.skill.as_ref().is_some_and(|s| {
                            s.frontmatter.description.to_lowercase().contains(&needle)
                        });
                    hit.then_some((i, 1))
                })
                .collect();

            // Ties broken by name so the order is stable as the user types
            // rather than reshuffling on every equal-scoring keystroke.
            scored.sort_by(|a, b| {
                b.1.cmp(&a.1).then_with(|| {
                    self.skills[a.0]
                        .entry
                        .name
                        .cmp(&self.skills[b.0].entry.name)
                })
            });
            self.visible = scored.into_iter().map(|(i, _)| i).collect();
        }
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        self.preview_scroll = 0;
    }

    pub fn focus_back(&mut self) {
        self.focus = match self.focus {
            Focus::SkillList => Focus::AgentMatrix,
            Focus::Preview => Focus::SkillList,
            Focus::AgentMatrix => Focus::Preview,
        };
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::SkillList => Focus::Preview,
            Focus::Preview => Focus::AgentMatrix,
            Focus::AgentMatrix => Focus::SkillList,
        };
    }

    /// Toggles the currently selected Agent Matrix column's target on the
    /// currently selected skill: adds an empty `targets.<id>` block if
    /// absent, removes it if present, then rewrites the skill's cache file
    /// so the change is visible to `skx sync`/`skx audit` immediately —
    /// not just inside this TUI session.
    pub fn toggle_selected_target(&mut self) {
        // Space used to refuse unless the Matrix already had focus, which
        // made the status line scold the user for an action the footer had
        // just advertised. Moving focus *is* the obvious intent, so do it.
        self.focus = Focus::AgentMatrix;
        let target_id = MATRIX_COLUMNS[self.matrix_selected];
        if target_id == "mcp" {
            self.set_status(
                "mcp targets follow mcp_dependencies and aren't toggled here",
                Level::Warning,
            );
            return;
        }

        let Some(index) = self.selected_index() else {
            return;
        };
        let Some(loaded) = self.skills.get_mut(index) else {
            return;
        };
        let Some(skill) = loaded.skill.as_mut() else {
            let name = loaded.entry.name.clone();
            self.set_sticky(
                format!("{name}: cache copy failed to load, can't edit"),
                Level::Danger,
            );
            return;
        };

        let now_declared = if skill.frontmatter.targets.contains_key(target_id) {
            skill.frontmatter.targets.remove(target_id);
            false
        } else {
            skill.frontmatter.targets.insert(
                target_id.to_string(),
                serde_yaml::Value::Mapping(Default::default()),
            );
            true
        };

        let Some(path) = skill.source_path.clone() else {
            self.set_sticky(
                "skill has no source path to write back to".to_string(),
                Level::Danger,
            );
            return;
        };
        match skx_core::render_skill(skill) {
            Ok(rendered) => match std::fs::write(&path, rendered) {
                Ok(()) => {
                    let verb = if now_declared { "enabled" } else { "disabled" };
                    let name = loaded.entry.name.clone();
                    self.set_status(
                        &format!("{verb} {target_id} for {name} — press s to sync"),
                        Level::Success,
                    );
                    compute_statuses(&mut self.skills, &self.root, &self.home, &self.state);
                }
                Err(e) => self.set_sticky(
                    format!("failed to write {}: {e}", path.display()),
                    Level::Danger,
                ),
            },
            Err(e) => self.set_sticky(format!("failed to render skill: {e}"), Level::Danger),
        }
    }

    /// Spawns a sync on a worker thread and returns immediately.
    ///
    /// The plan is built here (on the UI thread, where the parsed skills
    /// live) and moved wholesale into the worker, so the worker never
    /// borrows from `App` and the event loop stays free to repaint.
    pub fn start_sync(&mut self, scope: SyncScope) {
        if self.sync.is_some() {
            self.set_status("a sync is already running", Level::Warning);
            return;
        }

        let chosen: Vec<(ManifestEntry, Skill)> = match scope {
            SyncScope::All => self
                .skills
                .iter()
                .filter_map(|l| Some((l.entry.clone(), l.skill.clone()?)))
                .collect(),
            SyncScope::Selected => self
                .selected_skill()
                .and_then(|l| Some((l.entry.clone(), l.skill.clone()?)))
                .into_iter()
                .collect(),
        };

        if chosen.is_empty() {
            self.set_status("nothing to sync", Level::Warning);
            return;
        }

        let plan = SyncPlan {
            root: self.root.clone(),
            home: self.home.clone(),
            state: self.state.clone(),
            skills: chosen,
        };
        let total = plan.skills.len();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || run_sync(plan, &tx));

        self.sync = Some(SyncJob {
            progress: rx,
            done: 0,
            total,
            current: String::new(),
            frame: 0,
        });
    }

    /// Drains whatever the worker has produced since the last frame.
    ///
    /// Non-blocking by construction: the UI must never wait on the worker,
    /// or we are back to a frozen terminal with extra steps.
    pub fn poll_sync(&mut self) {
        let Some(job) = &mut self.sync else { return };

        let mut outcome = None;
        for message in job.progress.try_iter() {
            match message {
                SyncProgress::Skill { name, done } => {
                    job.current = name;
                    job.done = done;
                }
                SyncProgress::Finished { written, state } => {
                    outcome = Some(Ok((written, state)));
                }
                SyncProgress::Failed(e) => outcome = Some(Err(e)),
            }
        }

        match outcome {
            None => {}
            Some(Ok((written, state))) => {
                let skills = self.sync.take().map(|j| j.total).unwrap_or(0);
                self.state = *state;
                compute_statuses(&mut self.skills, &self.root, &self.home, &self.state);
                self.set_status(
                    &format!("synced {skills} skill(s) — {written} artifact(s) written"),
                    Level::Success,
                );
            }
            Some(Err(e)) => {
                self.sync = None;
                self.set_sticky(format!("sync failed: {e}"), Level::Danger);
            }
        }
    }
}

/// Everything a sync needs, owned, so it can cross a thread boundary.
struct SyncPlan {
    root: PathBuf,
    home: PathBuf,
    state: StateFile,
    skills: Vec<(ManifestEntry, Skill)>,
}

/// The worker body. Reports one message per skill so the UI can show a
/// determinate bar — the work is countable, and a progress bar that answers
/// "how much longer" is reassuring in a way a bare spinner never is.
fn run_sync(mut plan: SyncPlan, tx: &mpsc::Sender<SyncProgress>) {
    let adapters = skx_adapters::default_adapters();
    let mut written = 0usize;

    for (index, (entry, skill)) in plan.skills.iter().enumerate() {
        // A closed channel means the UI is gone (quit mid-sync); stop
        // rather than finishing work nobody will ever see.
        if tx
            .send(SyncProgress::Skill {
                name: entry.name.clone(),
                done: index,
            })
            .is_err()
        {
            return;
        }

        let cache_file = skx_core::skill_path(entry.scope, &plan.root, &plan.home, &entry.name);
        let cache = skx_core::skill_dir(entry.scope, &plan.root, &plan.home, &entry.name);
        let ctx = CompileCtx {
            root: &plan.root,
            home: &plan.home,
            scope: entry.scope,
            cache: &cache,
        };
        for adapter in &adapters {
            let Ok(output) = adapter.compile(skill, &ctx) else {
                continue;
            };
            for artifact in &output.artifacts {
                let cache_source = matches!(adapter.link_strategy(), LinkStrategy::Symlink)
                    .then_some(cache_file.as_path());
                let Ok(write) = skx_core::apply(artifact, adapter.link_strategy(), cache_source)
                else {
                    continue;
                };
                let (kind, sub_key) = skx_core::artifact_kind_and_sub_key(artifact);
                plan.state.upsert(skx_core::ArtifactRecord {
                    path: artifact.path().to_path_buf(),
                    sub_key,
                    skill: entry.name.clone(),
                    skill_version: skill.frontmatter.version.clone(),
                    target: adapter.target_name().to_string(),
                    kind,
                    content_hash: write.content_hash,
                    symlink_target: write.symlink_target,
                });
                written += 1;
            }
        }
    }

    let message = match plan.state.save(&skx_core::state_path(&plan.root)) {
        Ok(()) => SyncProgress::Finished {
            written,
            state: Box::new(plan.state),
        },
        Err(e) => SyncProgress::Failed(format!("wrote artifacts but could not save state: {e}")),
    };
    let _ = tx.send(message);
}

fn clamp_index(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    next.clamp(0, len as isize - 1) as usize
}

fn load_skills(manifest: &Manifest, root: &Path, home: &Path) -> Vec<LoadedSkill> {
    manifest
        .skills
        .iter()
        .map(|entry| {
            let cache_file = skx_core::skill_path(entry.scope, root, home, &entry.name);
            match skx_core::load_skill(&cache_file) {
                Ok(skill) => LoadedSkill {
                    entry: entry.clone(),
                    tokens: skill.frontmatter.approx_tokens(&skill.body),
                    skill: Some(skill),
                    load_error: None,
                    statuses: Vec::new(),
                },
                Err(e) => LoadedSkill {
                    entry: entry.clone(),
                    skill: None,
                    load_error: Some(e.to_string()),
                    statuses: Vec::new(),
                    tokens: 0,
                },
            }
        })
        .collect()
}

fn compute_statuses(skills: &mut [LoadedSkill], root: &Path, home: &Path, state: &StateFile) {
    let adapters = skx_adapters::default_adapters();
    for loaded in skills.iter_mut() {
        loaded.statuses = match &loaded.skill {
            Some(skill) => MATRIX_COLUMNS
                .iter()
                .map(|&target_id| {
                    target_status(
                        target_id,
                        skill,
                        &loaded.entry,
                        root,
                        home,
                        state,
                        &adapters,
                    )
                })
                .collect(),
            None => vec![TargetStatus::Error; MATRIX_COLUMNS.len()],
        };
    }
}

fn target_status(
    target_id: &str,
    skill: &Skill,
    entry: &ManifestEntry,
    root: &Path,
    home: &Path,
    state: &StateFile,
    adapters: &[Box<dyn SkillAdapter>],
) -> TargetStatus {
    let declared = if target_id == "mcp" {
        !skill.frontmatter.mcp_dependencies.is_empty()
    } else {
        skill.frontmatter.targets.contains_key(target_id)
    };
    if !declared {
        return TargetStatus::NotDeclared;
    }

    let Some(adapter) = adapters.iter().find(|a| a.target_name() == target_id) else {
        return TargetStatus::NotDeclared;
    };
    let ctx = CompileCtx {
        root,
        home,
        scope: entry.scope,
        cache: &skx_core::skill_dir(entry.scope, root, home, &entry.name),
    };
    let output = match adapter.compile(skill, &ctx) {
        Ok(output) => output,
        Err(_) => return TargetStatus::Error,
    };
    if output.artifacts.is_empty() {
        return TargetStatus::NotDeclared;
    }

    let cache_file = skx_core::skill_path(entry.scope, root, home, &entry.name);
    for artifact in &output.artifacts {
        let (_, sub_key) = skx_core::artifact_kind_and_sub_key(artifact);
        let Some(record) = state.record_for(artifact.path(), sub_key.as_deref()) else {
            return TargetStatus::NotSynced;
        };
        let cache_source = matches!(adapter.link_strategy(), LinkStrategy::Symlink)
            .then_some(cache_file.as_path());
        let fresh = match skx_core::fresh_hash(artifact, adapter.link_strategy(), cache_source) {
            Ok(hash) => hash,
            Err(_) => return TargetStatus::Error,
        };
        match skx_core::audit_record(record, Some(&fresh)) {
            Ok(DriftStatus::InSync) => {}
            Ok(_) => return TargetStatus::NeedsAttention,
            Err(_) => return TargetStatus::Error,
        }
    }
    TargetStatus::InSync
}

/// Picks the palette from preferences, the environment, and terminal
/// capability. `NO_COLOR` wins over everything: it's a request to emit no
/// colour at all, so the palette collapses to the terminal's own defaults
/// rather than a chosen theme.
fn resolve_palette(config: &skx_core::Config) -> Palette {
    if !skx_core::Config::color_enabled() {
        return Palette::NO_COLOR;
    }
    Palette::resolve(config.theme.as_explicit()).quantized(ColorDepth::detect())
}

/// Convenience for `run()`: resolves the workspace root (cwd) and home
/// directory the same way `skx_cli` does.
pub fn default_root_and_home() -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = std::env::current_dir()?;
    let home = skx_core::home_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine the current user's home directory"))?;
    Ok((root, home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skx_core::Scope;

    const SAMPLE_SKILL: &str = "---\nname: rust-systems-expert\ndescription: Deep systems architectural conventions\nversion: 1.0.0\ntriggers:\n  - \"*.rs\"\ntargets:\n  antigravity:\n    scope: workspace\n    auto_activate: true\n  claude_code:\n    enabled: true\n  cursor:\n    glob: \"**/*.rs\"\n  copilot:\n    enabled: true\nmcp_dependencies:\n  - name: rust-analyzer-mcp\n    command: rust-analyzer-mcp\n    args: [\"--stdio\"]\n---\n\n# Rust Systems Engineering Instructions\n\n- Prefer zero-cost abstractions.\n";

    struct Workspace {
        _dir: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
    }

    fn setup() -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let cache_file = skx_core::skill_path(Scope::Local, &root, &home, "rust-systems-expert");
        std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        std::fs::write(&cache_file, SAMPLE_SKILL).unwrap();

        let mut manifest = Manifest::default();
        manifest.upsert(ManifestEntry {
            name: "rust-systems-expert".to_string(),
            source: "/some/src".to_string(),
            scope: Scope::Local,
            version: "1.0.0".to_string(),
        });
        manifest.save(&skx_core::manifest_path(&root)).unwrap();

        Workspace {
            _dir: dir,
            root,
            home,
        }
    }

    #[test]
    fn load_finds_the_installed_skill_and_marks_declared_targets_not_synced() {
        let ws = setup();
        let app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        assert_eq!(app.skills.len(), 1);
        let loaded = &app.skills[0];
        assert!(loaded.skill.is_some());
        assert_eq!(
            loaded.statuses,
            vec![TargetStatus::NotSynced; MATRIX_COLUMNS.len()]
        );
    }

    /// Runs a sync to completion on the calling thread's timeline: starts
    /// the worker, then polls until it reports done. Real usage never
    /// blocks like this — the event loop repaints between polls — but a
    /// test needs a deterministic finish line.
    fn sync_to_completion(app: &mut App, scope: SyncScope) {
        app.start_sync(scope);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.sync.is_some() {
            assert!(Instant::now() < deadline, "sync did not finish in time");
            app.poll_sync();
            std::thread::yield_now();
        }
    }

    #[test]
    fn sync_all_marks_every_declared_target_in_sync() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        sync_to_completion(&mut app, SyncScope::All);

        assert_eq!(
            app.skills[0].statuses,
            vec![TargetStatus::InSync; MATRIX_COLUMNS.len()]
        );
        assert!(skx_core::state_path(&ws.root).exists());
    }

    #[test]
    fn toggle_moves_focus_to_the_matrix_instead_of_refusing() {
        // This used to no-op and print "press Tab to switch to the Agent
        // Matrix pane first" — the status line scolding the user for an
        // action the footer had just advertised. Moving focus is the
        // obvious intent, so Space now does it.
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        assert_eq!(app.focus, Focus::SkillList);
        app.matrix_selected = MATRIX_COLUMNS.iter().position(|&c| c == "cursor").unwrap();

        app.toggle_selected_target();

        assert_eq!(app.focus, Focus::AgentMatrix);
        let skill = app.skills[0].skill.as_ref().unwrap();
        assert!(
            !skill.frontmatter.targets.contains_key("cursor"),
            "should have toggled"
        );
    }

    #[test]
    fn toggle_removes_a_declared_target_and_persists_to_disk() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.focus = Focus::AgentMatrix;
        app.matrix_selected = MATRIX_COLUMNS.iter().position(|&c| c == "cursor").unwrap();

        app.toggle_selected_target();

        let skill = app.skills[0].skill.as_ref().unwrap();
        assert!(!skill.frontmatter.targets.contains_key("cursor"));

        // Persisted: reloading the app from disk shows the same thing.
        let reloaded = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        let reloaded_skill = reloaded.skills[0].skill.as_ref().unwrap();
        assert!(!reloaded_skill.frontmatter.targets.contains_key("cursor"));

        let cursor_idx = MATRIX_COLUMNS.iter().position(|&c| c == "cursor").unwrap();
        assert_eq!(
            app.skills[0].statuses[cursor_idx],
            TargetStatus::NotDeclared
        );
    }

    #[test]
    fn toggle_adds_a_target_that_was_absent() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        // Remove copilot first via direct edit so we can test re-adding it.
        {
            let skill = app.skills[0].skill.as_mut().unwrap();
            skill.frontmatter.targets.remove("copilot");
            let rendered = skx_core::render_skill(skill).unwrap();
            std::fs::write(skill.source_path.as_ref().unwrap(), rendered).unwrap();
        }
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.toggle_focus();
        app.matrix_selected = MATRIX_COLUMNS.iter().position(|&c| c == "copilot").unwrap();

        app.toggle_selected_target();

        let skill = app.skills[0].skill.as_ref().unwrap();
        assert!(skill.frontmatter.targets.contains_key("copilot"));
        assert!(app.status.text().contains("enabled"));
    }

    #[test]
    fn mcp_column_is_not_toggleable() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.toggle_focus();
        app.matrix_selected = MATRIX_COLUMNS.iter().position(|&c| c == "mcp").unwrap();

        app.toggle_selected_target();

        assert!(app.status.text().contains("mcp_dependencies"));
        let skill = app.skills[0].skill.as_ref().unwrap();
        assert!(!skill.frontmatter.mcp_dependencies.is_empty());
    }

    #[test]
    fn move_selection_clamps_at_the_edges() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        assert_eq!(app.selected, 0);
        app.move_selection(-5);
        assert_eq!(app.selected, 0);
        app.move_selection(5);
        assert_eq!(app.selected, 0); // only one skill installed

        // Tab cycles Table → Preview → Matrix, so it takes two hops to
        // reach the matrix.
        app.toggle_focus();
        assert_eq!(app.focus, Focus::Preview);
        app.toggle_focus();
        assert_eq!(app.focus, Focus::AgentMatrix);

        app.move_selection(-5);
        assert_eq!(app.matrix_selected, 0);
        app.move_selection(100);
        assert_eq!(app.matrix_selected, MATRIX_COLUMNS.len() - 1);
    }

    #[test]
    fn tab_and_shift_tab_are_inverses_across_the_whole_cycle() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        for _ in 0..3 {
            let before = app.focus;
            app.toggle_focus();
            app.focus_back();
            assert_eq!(app.focus, before);
            app.toggle_focus();
        }
        // Three forward hops return to the start.
        assert_eq!(app.focus, Focus::SkillList);
    }

    #[test]
    fn j_and_k_scroll_the_preview_when_it_holds_focus() {
        // The old binding was Shift-J/K, which made the most frequent
        // reading action a two-key chord and left it out of the status line.
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.preview_len = 50;
        app.preview_viewport = 10;

        app.focus = Focus::Preview;
        app.move_selection(3);
        assert_eq!(app.preview_scroll, 3);
        app.jump_selection(true);
        assert_eq!(app.preview_scroll, 49);
        app.jump_selection(false);
        assert_eq!(app.preview_scroll, 0);

        // ...and does not move the skill cursor while doing so.
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn escape_backs_out_one_level_and_never_quits() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        app.toggle_help();
        app.escape();
        assert!(app.overlay.is_none(), "first Esc closes the overlay");
        assert!(!app.should_quit);

        app.filter = "x".to_string();
        app.escape();
        assert!(app.filter.is_empty(), "next Esc clears the filter");
        assert!(!app.should_quit);

        app.focus = Focus::AgentMatrix;
        app.escape();
        assert_eq!(app.focus, Focus::SkillList, "next Esc returns focus home");
        assert!(!app.should_quit);

        // At the root there is nothing left to undo — and it still must not
        // quit, which is the whole point of the change.
        app.escape();
        assert!(!app.should_quit);
        assert!(app.status.text().contains("q to quit"));
    }

    #[test]
    fn syncing_everything_asks_before_it_writes() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        app.request_sync(SyncScope::All);
        assert!(matches!(app.overlay, Some(Overlay::ConfirmSyncAll { .. })));
        assert!(app.sync.is_none(), "must not start before confirmation");

        app.escape();
        assert!(app.sync.is_none(), "cancelling must not start a sync");

        app.request_sync(SyncScope::All);
        app.confirm_overlay();
        assert!(app.sync.is_some(), "confirming starts the worker");
    }

    #[test]
    fn syncing_one_skill_needs_no_confirmation() {
        // Its blast radius is the row under the cursor, so the effort to
        // trigger it should stay low.
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.request_sync(SyncScope::Selected);
        assert!(app.overlay.is_none());
        assert!(app.sync.is_some());
    }

    #[test]
    fn ctrl_c_leaves_immediately_but_q_asks_first() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        app.request_quit();
        assert!(matches!(app.overlay, Some(Overlay::ConfirmQuit { .. })));
        assert!(!app.should_quit, "the prompt must not pre-commit the exit");

        app.escape();
        assert!(app.overlay.is_none());
        assert!(!app.should_quit, "staying means staying");

        app.request_quit();
        app.confirm_overlay();
        assert!(app.should_quit);

        // Ctrl-C is the universal abort and never negotiates.
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.force_quit();
        assert!(app.should_quit);
        assert!(app.overlay.is_none());
    }

    #[test]
    fn a_clean_workspace_can_opt_out_of_the_quit_prompt() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.config.confirm_quit = false;

        // Still pending, so it asks regardless of preference: the setting
        // governs the quiet case, not the case with something to lose.
        assert!(app.pending_sync_count() > 0);
        app.request_quit();
        assert!(matches!(app.overlay, Some(Overlay::ConfirmQuit { .. })));

        app.overlay = None;
        sync_to_completion(&mut app, SyncScope::All);
        assert_eq!(app.pending_sync_count(), 0);

        app.request_quit();
        assert!(app.should_quit, "nothing to lose and the prompt is off");
        assert!(app.overlay.is_none());
    }

    #[test]
    fn a_running_sync_always_prompts_even_with_the_setting_off() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.config.confirm_quit = false;
        app.start_sync(SyncScope::All);

        app.request_quit();
        assert!(
            matches!(app.overlay, Some(Overlay::ConfirmQuit { .. })),
            "quitting mid-write is exactly when a prompt earns its place"
        );
    }

    #[test]
    fn clicking_a_pane_focuses_it_and_clicking_a_row_selects_it() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.panes = PaneRects {
            table: Region {
                x: 0,
                y: 2,
                width: 80,
                height: 10,
            },
            preview: Region {
                x: 0,
                y: 12,
                width: 40,
                height: 8,
            },
            matrix: Region {
                x: 40,
                y: 12,
                width: 40,
                height: 7,
            },
        };

        // Row 0 of the table sits two rows in, past the border and header.
        assert!(app.click(5, 4));
        assert_eq!(app.focus, Focus::SkillList);
        assert_eq!(app.selected, 0);

        assert!(app.click(5, 14));
        assert_eq!(app.focus, Focus::Preview);

        assert!(app.click(45, 15));
        assert_eq!(app.focus, Focus::AgentMatrix);
        assert_eq!(app.matrix_selected, 2);

        // Nothing is at the very bottom-right of the screen.
        assert!(!app.click(200, 200));
    }

    #[test]
    fn a_click_behind_an_overlay_is_swallowed() {
        // Otherwise a stray click silently changes the selection the user
        // returns to after dismissing the modal.
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.panes.table = Region {
            x: 0,
            y: 2,
            width: 80,
            height: 10,
        };
        app.toggle_help();

        assert!(!app.click(5, 6));
        assert_eq!(app.focus, Focus::SkillList);
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer_without_stealing_focus() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.preview_len = 100;
        app.panes.preview = Region {
            x: 0,
            y: 12,
            width: 40,
            height: 8,
        };
        app.panes.matrix = Region {
            x: 40,
            y: 12,
            width: 40,
            height: 7,
        };
        assert_eq!(app.focus, Focus::SkillList);

        assert!(app.scroll_at(5, 14, 3));
        assert_eq!(app.preview_scroll, 3);
        assert_eq!(
            app.focus,
            Focus::SkillList,
            "the wheel is a glance, not a commitment"
        );

        assert!(app.scroll_at(45, 14, 2));
        assert_eq!(app.matrix_selected, 2);
        assert_eq!(app.focus, Focus::SkillList);
    }

    #[test]
    fn region_hit_testing_excludes_the_far_edges() {
        let r = Region {
            x: 10,
            y: 5,
            width: 4,
            height: 3,
        };
        assert!(r.contains(10, 5));
        assert!(r.contains(13, 7));
        assert!(!r.contains(14, 7), "x is exclusive at x + width");
        assert!(!r.contains(13, 8), "y is exclusive at y + height");
        assert!(!r.contains(9, 5));
    }

    #[test]
    fn transient_status_decays_but_errors_persist() {
        let now = Instant::now();
        let transient = Status::transient("done".to_string(), Level::Success, now);
        assert_eq!(transient.resolve(now).0, "done");
        assert_eq!(
            transient.resolve(now + Duration::from_secs(60)).0,
            "ready",
            "a stale message asserts a dead state with live confidence"
        );

        let sticky = Status::sticky("sync failed".to_string(), Level::Danger);
        assert_eq!(
            sticky.resolve(now + Duration::from_secs(600)).0,
            "sync failed"
        );
    }

    #[test]
    fn missing_cache_file_is_reported_without_crashing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let mut manifest = Manifest::default();
        manifest.upsert(ManifestEntry {
            name: "ghost-skill".to_string(),
            source: "/nowhere".to_string(),
            scope: Scope::Local,
            version: "1.0.0".to_string(),
        });
        manifest.save(&skx_core::manifest_path(&root)).unwrap();

        let app = App::load(root, home).unwrap();
        assert_eq!(app.skills.len(), 1);
        assert!(app.skills[0].skill.is_none());
        assert!(app.skills[0].load_error.is_some());
        assert_eq!(
            app.skills[0].statuses,
            vec![TargetStatus::Error; MATRIX_COLUMNS.len()]
        );
    }

    fn write_hand_installed_skill(path: &Path, name: &str, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = format!("---\nname: {name}\ndescription: d\nversion: {version}\n---\nbody\n");
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn open_discover_finds_a_hand_installed_skill() {
        let ws = setup();
        write_hand_installed_skill(
            &ws.root.join(".claude/skills/found-me/SKILL.md"),
            "found-me",
            "1.0.0",
        );
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        app.open_discover();

        let Screen::Discover(state) = &app.screen else {
            panic!("expected Screen::Discover");
        };
        assert_eq!(state.candidates.len(), 1);
        assert!(
            state.included[0],
            "singleton candidates default to included"
        );
    }

    #[test]
    fn open_discover_with_nothing_new_stays_on_main() {
        let ws = setup();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        app.open_discover();

        assert!(matches!(app.screen, Screen::Main));
        assert!(app.status.text().contains("No unmanaged skills found"));
    }

    #[test]
    fn conflicting_candidates_default_to_the_highest_version() {
        let ws = setup();
        write_hand_installed_skill(&ws.root.join(".claude/skills/dup/SKILL.md"), "dup", "1.0.0");
        write_hand_installed_skill(&ws.home.join(".claude/skills/dup/SKILL.md"), "dup", "2.0.0");
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();

        app.open_discover();

        let Screen::Discover(state) = &app.screen else {
            panic!("expected Screen::Discover");
        };
        assert_eq!(state.candidates.len(), 2);
        assert_eq!(
            state.included_count(),
            1,
            "conflicts start with exactly one pick"
        );
        let included_idx = state.included.iter().position(|&b| b).unwrap();
        assert_eq!(
            state.candidates[included_idx].skill.frontmatter.version,
            "2.0.0"
        );
    }

    #[test]
    fn toggling_one_conflict_candidate_deselects_its_sibling() {
        let ws = setup();
        write_hand_installed_skill(&ws.root.join(".claude/skills/dup/SKILL.md"), "dup", "1.0.0");
        write_hand_installed_skill(&ws.home.join(".claude/skills/dup/SKILL.md"), "dup", "2.0.0");
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();

        let other_idx = {
            let Screen::Discover(state) = &app.screen else {
                unreachable!()
            };
            state.included.iter().position(|&b| !b).unwrap()
        };
        if let Screen::Discover(state) = &mut app.screen {
            state.selected = state
                .display_order
                .iter()
                .position(|&i| i == other_idx)
                .unwrap();
        }
        app.toggle_discover_selected();

        let Screen::Discover(state) = &app.screen else {
            unreachable!()
        };
        assert_eq!(
            state.included_count(),
            1,
            "still exactly one, just the other one"
        );
        assert!(state.included[other_idx]);
    }

    #[test]
    fn toggling_off_the_only_included_candidate_leaves_none_selected() {
        let ws = setup();
        write_hand_installed_skill(
            &ws.root.join(".claude/skills/solo/SKILL.md"),
            "solo",
            "1.0.0",
        );
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();

        app.toggle_discover_selected();

        let Screen::Discover(state) = &app.screen else {
            unreachable!()
        };
        assert_eq!(state.included_count(), 0);
    }

    #[test]
    fn commit_discover_copies_files_and_registers_the_manifest() {
        let ws = setup();
        write_hand_installed_skill(
            &ws.root.join(".claude/skills/found-me/SKILL.md"),
            "found-me",
            "1.0.0",
        );
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();

        app.commit_discover();

        assert!(matches!(app.screen, Screen::Main));
        assert!(app.status.text().contains("imported 1 skill"));
        assert_eq!(
            app.skills.len(),
            2,
            "the pre-existing skill plus the import"
        );
        assert!(app.manifest.get("found-me").is_some());
        let cache_file = skx_core::skill_path(Scope::Local, &ws.root, &ws.home, "found-me");
        assert!(
            cache_file.is_file(),
            "should have copied the file into the cache"
        );

        // Persisted: reloading from disk shows the same thing.
        let reloaded = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        assert!(reloaded.manifest.get("found-me").is_some());
    }

    #[test]
    fn commit_discover_declares_claude_code_for_a_skill_found_there() {
        let ws = setup();
        // A realistic hand-installed skill: no `targets:` block at all,
        // since that's an skx-only concept it never had before.
        write_hand_installed_skill(
            &ws.root.join(".claude/skills/found-me/SKILL.md"),
            "found-me",
            "1.0.0",
        );
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();

        app.commit_discover();

        let cache_file = skx_core::skill_path(Scope::Local, &ws.root, &ws.home, "found-me");
        let imported = skx_core::load_skill(&cache_file).unwrap();
        assert!(
            imported.frontmatter.targets.contains_key("claude_code"),
            "importing from a .claude/skills directory should declare claude_code so \
             the skill doesn't sit inert until someone toggles it on by hand"
        );

        // And it actually does something on sync now.
        sync_to_completion(&mut app, SyncScope::All);
        let claude_code_idx = MATRIX_COLUMNS
            .iter()
            .position(|&c| c == "claude_code")
            .unwrap();
        assert_eq!(
            app.skills[0].statuses[claude_code_idx],
            TargetStatus::InSync
        );
    }

    #[test]
    fn commit_discover_does_not_override_an_already_declared_target() {
        let ws = setup();
        let path = ws.root.join(".claude/skills/opinionated/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "---\nname: opinionated\ndescription: d\nversion: 1.0.0\ntargets:\n  claude_code:\n    enabled: false\n---\nbody\n",
        )
        .unwrap();
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();

        app.commit_discover();

        let cache_file = skx_core::skill_path(Scope::Local, &ws.root, &ws.home, "opinionated");
        let imported = skx_core::load_skill(&cache_file).unwrap();
        let claude_code_cfg = imported.frontmatter.targets.get("claude_code").unwrap();
        assert_eq!(
            claude_code_cfg.get("enabled").and_then(|v| v.as_bool()),
            Some(false),
            "an explicit existing declaration must survive import untouched"
        );
    }

    #[test]
    fn commit_discover_skips_deselected_candidates() {
        let ws = setup();
        write_hand_installed_skill(
            &ws.root.join(".claude/skills/solo/SKILL.md"),
            "solo",
            "1.0.0",
        );
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();
        app.toggle_discover_selected(); // deselect the only candidate

        app.commit_discover();

        assert!(app.status.text().contains("imported 0 skill"));
        assert!(app.manifest.get("solo").is_none());
    }

    #[test]
    fn cancel_discover_commits_nothing() {
        let ws = setup();
        write_hand_installed_skill(
            &ws.root.join(".claude/skills/found-me/SKILL.md"),
            "found-me",
            "1.0.0",
        );
        let mut app = App::load(ws.root.clone(), ws.home.clone()).unwrap();
        app.open_discover();

        app.cancel_discover();

        assert!(matches!(app.screen, Screen::Main));
        assert!(app.manifest.get("found-me").is_none());
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    /// Keeps the temp directory alive alongside the app: unlike the render
    /// tests, these exercise `toggle_selected_target`, which writes the
    /// skill back to disk and silently no-ops if the workspace is gone.
    struct Fixture {
        _dir: tempfile::TempDir,
        app: App,
    }

    impl std::ops::Deref for Fixture {
        type Target = App;
        fn deref(&self) -> &App {
            &self.app
        }
    }
    impl std::ops::DerefMut for Fixture {
        fn deref_mut(&mut self) -> &mut App {
            &mut self.app
        }
    }

    fn app_with(names_and_descriptions: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let mut manifest = Manifest::default();
        for (name, description) in names_and_descriptions {
            let path = skx_core::skill_path(skx_core::Scope::Local, &root, &home, name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                format!(
                    "---\nname: {name}\ndescription: {description}\nversion: 1.0.0\n---\nbody\n"
                ),
            )
            .unwrap();
            manifest.upsert(ManifestEntry {
                name: name.to_string(),
                source: "x".to_string(),
                scope: skx_core::Scope::Local,
                version: "1.0.0".to_string(),
            });
        }
        manifest.save(&skx_core::manifest_path(&root)).unwrap();
        Fixture {
            app: App::load(root, home).unwrap(),
            _dir: dir,
        }
    }

    fn visible_names(app: &App) -> Vec<&str> {
        app.visible
            .iter()
            .map(|&i| app.skills[i].entry.name.as_str())
            .collect()
    }

    #[test]
    fn an_empty_filter_shows_everything_in_manifest_order() {
        let mut app = app_with(&[("beta", "b"), ("alpha", "a")]);
        app.recompute_visible();
        assert_eq!(visible_names(&app), ["beta", "alpha"]);
    }

    #[test]
    fn name_matches_are_fuzzy_and_outrank_description_matches() {
        let mut app = app_with(&[
            ("analytics", "measure things with seo relevance"),
            ("seo-audit", "unrelated blurb"),
            ("programmatic-seo", "unrelated blurb"),
        ]);
        app.filter = "seo".to_string();
        app.recompute_visible();

        let names = visible_names(&app);
        // Both name hits come before the description-only hit.
        assert_eq!(names.len(), 3);
        assert!(names[..2].contains(&"seo-audit"));
        assert!(names[..2].contains(&"programmatic-seo"));
        assert_eq!(names[2], "analytics");
    }

    #[test]
    fn descriptions_match_only_on_literal_substrings() {
        // "seo" is a *subsequence* of "search engine optimization" but not a
        // substring, so this description must not match — that fuzzy-over-prose
        // behaviour is what made the filter match every skill at once.
        let mut app = app_with(&[("copywriting", "search engine optimization for pages")]);
        app.filter = "seo".to_string();
        app.recompute_visible();
        assert!(visible_names(&app).is_empty());
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_no_dangling_selection() {
        let mut app = app_with(&[("alpha", "a"), ("beta", "b")]);
        app.selected = 1;
        app.filter = "zzzz".to_string();
        app.recompute_visible();
        assert!(app.visible.is_empty());
        assert_eq!(app.selected, 0);
        assert!(app.selected_skill().is_none());
    }

    #[test]
    fn clearing_the_filter_restores_every_skill() {
        let mut app = app_with(&[("alpha", "a"), ("beta", "b")]);
        app.filter = "alp".to_string();
        app.recompute_visible();
        assert_eq!(visible_names(&app), ["alpha"]);

        app.clear_filter();
        assert_eq!(visible_names(&app), ["alpha", "beta"]);
    }

    #[test]
    fn toggling_a_target_edits_the_filtered_skill_not_the_underlying_row() {
        // Regression guard: `selected` indexes `visible`, so a filter that
        // reorders rows must not make Space edit whichever skill happens to
        // sit at that position in `skills`.
        let mut app = app_with(&[("alpha", "a"), ("beta", "b")]);
        app.filter = "beta".to_string();
        app.recompute_visible();
        app.selected = 0;
        app.focus = Focus::AgentMatrix;
        app.matrix_selected = 0; // antigravity

        app.toggle_selected_target();

        let beta = app.skills.iter().find(|s| s.entry.name == "beta").unwrap();
        let alpha = app.skills.iter().find(|s| s.entry.name == "alpha").unwrap();
        assert!(
            beta.skill
                .as_ref()
                .unwrap()
                .frontmatter
                .targets
                .contains_key("antigravity")
        );
        assert!(
            !alpha
                .skill
                .as_ref()
                .unwrap()
                .frontmatter
                .targets
                .contains_key("antigravity")
        );
    }

    #[test]
    fn preview_scroll_is_clamped_to_the_content() {
        let mut app = app_with(&[("alpha", "a")]);
        app.preview_len = 10;
        app.preview_viewport = 4;

        app.move_preview(100);
        assert_eq!(app.preview_scroll, 9);
        app.move_preview(-1000);
        assert_eq!(app.preview_scroll, 0);

        app.page_preview(true);
        assert_eq!(app.preview_scroll, 3);
    }

    #[test]
    fn health_counts_declared_targets_not_skills() {
        let mut app = app_with(&[("alpha", "a")]);
        app.focus = Focus::AgentMatrix;
        for i in 0..2 {
            app.matrix_selected = i;
            app.toggle_selected_target();
        }
        let health = app.health();
        assert_eq!(health.skills, 1);
        assert_eq!(health.declared, 2);
        // Declared but never synced, so the meter must not read healthy.
        assert_eq!(health.not_synced, 2);
        assert_eq!(health.ratio(), 0.0);
    }

    #[test]
    fn an_empty_workspace_reads_as_healthy_rather_than_broken() {
        let app = app_with(&[]);
        assert_eq!(app.health().ratio(), 1.0);
    }
}
