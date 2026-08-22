//! The cockpit's design tokens: one resolved [`Palette`] plus the small
//! primitives every pane builds from.
//!
//! Three rules keep the surface coherent across a long session:
//!
//! 1. **Tokens are named for meaning, not hue.** A light theme maps
//!    `success` onto a dark green and every call site keeps working,
//!    because none of them ever asked for "green".
//! 2. **Chrome recedes, data advances.** Borders and labels sit at
//!    `border`/`fg_dim`; only real data gets a saturated colour.
//! 3. **Severity is a ramp, not a lookup.** Anything meaning "how healthy
//!    is this" goes through [`Palette::severity`], so a status dot, a
//!    health meter and a summary count describing the same drift always
//!    land on the same colour.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};

/// How much colour the terminal can actually render.
///
/// `Color::Rgb` is emitted as a 24-bit SGR sequence regardless of what the
/// terminal supports, so on a 256- or 16-colour terminal the palette has to
/// be quantised before it reaches the backend or it degrades unpredictably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorDepth {
    /// `COLORTERM=truecolor|24bit` is the de-facto signal; otherwise fall
    /// back to reading `TERM` for a 256-colour suffix.
    pub fn detect() -> Self {
        if matches!(
            std::env::var("COLORTERM").as_deref(),
            Ok("truecolor") | Ok("24bit")
        ) {
            return Self::TrueColor;
        }
        match std::env::var("TERM") {
            Ok(term) if term.contains("256") => Self::Ansi256,
            Ok(term) if term.contains("color") => Self::Ansi16,
            Ok(_) => Self::Ansi256,
            Err(_) => Self::Ansi16,
        }
    }

    /// Maps an RGB token down to what the terminal can show.
    fn quantize(self, color: Color) -> Color {
        let Color::Rgb(r, g, b) = color else {
            return color;
        };
        match self {
            Self::TrueColor => color,
            // The 6×6×6 cube plus the 24-step greyscale ramp. Near-grey
            // colours go to the ramp, which has far finer resolution there
            // than the cube's 51-unit steps.
            Self::Ansi256 => {
                let (max, min) = (r.max(g).max(b), r.min(g).min(b));
                if max - min < 16 {
                    let level = (r as u16 + g as u16 + b as u16) / 3;
                    if level < 8 {
                        return Color::Indexed(16);
                    }
                    if level > 248 {
                        return Color::Indexed(231);
                    }
                    return Color::Indexed(232 + ((level - 8) * 24 / 240) as u8);
                }
                let cube = |v: u8| (v as u16 * 5 / 255) as u8;
                Color::Indexed(16 + 36 * cube(r) + 6 * cube(g) + cube(b))
            }
            // Content colours only — surfaces are hand-authored, see
            // `ANSI16_DARK`. A generic mapping turns desaturated greys into
            // saturated cyans and collapses three near-black surfaces onto
            // the same value.
            Self::Ansi16 => {
                let (max, min) = (r.max(g).max(b), r.min(g).min(b));
                if max - min < 40 {
                    return match max {
                        0..=64 => Color::Black,
                        65..=128 => Color::DarkGray,
                        129..=200 => Color::Gray,
                        _ => Color::White,
                    };
                }
                let bright = max > 160;
                let bit = |v: u8| u8::from(v as i16 > (max as i16 + min as i16) / 2);
                match bit(r) | (bit(g) << 1) | (bit(b) << 2) {
                    1 => {
                        if bright {
                            Color::LightRed
                        } else {
                            Color::Red
                        }
                    }
                    2 => {
                        if bright {
                            Color::LightGreen
                        } else {
                            Color::Green
                        }
                    }
                    3 => {
                        if bright {
                            Color::LightYellow
                        } else {
                            Color::Yellow
                        }
                    }
                    4 => {
                        if bright {
                            Color::LightBlue
                        } else {
                            Color::Blue
                        }
                    }
                    5 => {
                        if bright {
                            Color::LightMagenta
                        } else {
                            Color::Magenta
                        }
                    }
                    6 => {
                        if bright {
                            Color::LightCyan
                        } else {
                            Color::Cyan
                        }
                    }
                    _ => {
                        if bright {
                            Color::White
                        } else {
                            Color::Gray
                        }
                    }
                }
            }
        }
    }
}

/// One resolved set of semantic colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Painted across every cell. Without it the app inherits whatever the
    /// terminal profile sets, and the whole palette is being measured
    /// against an unknown background.
    pub bg_base: Color,
    /// Header rows and overlay fills — raised above `bg_base`.
    pub bg_raised: Color,
    /// The cursor row, and nothing else. Sharing this with `bg_raised`
    /// makes a table header indistinguishable from the selected row.
    pub bg_selected: Color,
    pub border: Color,
    pub fg_dim: Color,
    pub fg: Color,
    pub accent: Color,
    /// Decorative emphasis — scope badges, secondary keys. Deliberately
    /// distinct from `success`: when the two shared a value, a green cell
    /// stopped reliably meaning "in sync".
    pub alt: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
}

impl Palette {
    /// Every text token is ≥4.5:1 against `bg_base` and `border` is ≥3:1
    /// per WCAG 1.4.11. `contrast_tests` re-checks this on every build.
    pub const DARK: Self = Self {
        bg_base: Color::Rgb(26, 27, 38),
        bg_raised: Color::Rgb(36, 40, 59),
        bg_selected: Color::Rgb(47, 53, 77),
        border: Color::Rgb(96, 105, 130),
        fg_dim: Color::Rgb(138, 148, 175),
        fg: Color::Rgb(202, 211, 232),
        accent: Color::Rgb(122, 162, 247),
        alt: Color::Rgb(187, 154, 247),
        success: Color::Rgb(158, 206, 106),
        warning: Color::Rgb(224, 175, 104),
        danger: Color::Rgb(247, 118, 142),
        info: Color::Rgb(125, 207, 255),
    };

    pub const LIGHT: Self = Self {
        bg_base: Color::Rgb(250, 250, 252),
        bg_raised: Color::Rgb(238, 240, 245),
        bg_selected: Color::Rgb(219, 226, 240),
        border: Color::Rgb(128, 136, 157),
        fg_dim: Color::Rgb(94, 102, 124),
        fg: Color::Rgb(38, 42, 56),
        accent: Color::Rgb(40, 79, 178),
        alt: Color::Rgb(104, 58, 183),
        success: Color::Rgb(58, 110, 26),
        warning: Color::Rgb(140, 84, 10),
        danger: Color::Rgb(178, 30, 58),
        info: Color::Rgb(20, 95, 140),
    };

    /// The `NO_COLOR` palette: every token is the terminal's own default.
    ///
    /// Honouring <https://no-color.org> means emitting no colour at all,
    /// not a low-saturation theme — so this resolves to `Color::Reset`
    /// throughout and lets the terminal decide. Everything the cockpit
    /// encodes in colour is also encoded in glyph or position (status
    /// characters, the matrix's block meters, the focus caret, bold
    /// headers), so nothing becomes unreadable when hue is removed. The
    /// three surfaces collapse together here by design: with no colour
    /// there is nothing to separate them with except the reverse-video the
    /// selection already uses.
    pub const NO_COLOR: Self = Self {
        bg_base: Color::Reset,
        bg_raised: Color::Reset,
        bg_selected: Color::Reset,
        border: Color::Reset,
        fg_dim: Color::Reset,
        fg: Color::Reset,
        accent: Color::Reset,
        alt: Color::Reset,
        success: Color::Reset,
        warning: Color::Reset,
        danger: Color::Reset,
        info: Color::Reset,
    };

    /// Whether this palette carries any colour information, so callers can
    /// substitute a non-colour cue (reverse video, a marker glyph) where
    /// they would otherwise rely on hue alone.
    pub fn is_monochrome(&self) -> bool {
        self.fg == Color::Reset && self.accent == Color::Reset
    }

    /// Resolution order: explicit setting, then a best-effort environment
    /// sniff, then dark.
    ///
    /// There is no portable way to *ask* a terminal for its background
    /// colour — the OSC 11 query needs a raw-mode read with a timeout and
    /// several emulators ignore it silently, so treating no-answer as
    /// "dark" would mislabel every terminal that merely didn't reply in
    /// time. `COLORFGBG` is a decent hint where it exists and absent
    /// everywhere else, which is why the explicit setting stays
    /// authoritative rather than being a mere override.
    pub fn resolve(explicit: Option<&str>) -> Self {
        match explicit.map(str::trim) {
            Some("light") => return Self::LIGHT,
            Some("dark") => return Self::DARK,
            _ => {}
        }
        if let Ok(value) = std::env::var("SKX_THEME") {
            match value.trim() {
                "light" => return Self::LIGHT,
                "dark" => return Self::DARK,
                _ => {}
            }
        }
        // COLORFGBG is "fg;bg" (sometimes "fg;;bg"); a high background
        // index means a light background.
        if let Ok(value) = std::env::var("COLORFGBG")
            && let Some(bg) = value.rsplit(';').next()
            && let Ok(index) = bg.trim().parse::<u8>()
            && (index == 7 || index == 15)
        {
            return Self::LIGHT;
        }
        Self::DARK
    }

    /// The 16-colour fallback, authored rather than derived.
    ///
    /// Sixteen colours genuinely cannot express three near-black surfaces,
    /// so quantising `bg_base`/`bg_raised`/`bg_selected` collapses them all
    /// onto `Black` — which silently undoes the one thing those tokens
    /// exist to do, namely keep a table header distinguishable from the
    /// selected row. Picking the three by hand is the only way to preserve
    /// the hierarchy at this depth.
    pub const ANSI16_DARK: Self = Self {
        bg_base: Color::Black,
        bg_raised: Color::DarkGray,
        bg_selected: Color::Blue,
        border: Color::DarkGray,
        fg_dim: Color::Gray,
        fg: Color::White,
        accent: Color::LightBlue,
        alt: Color::LightMagenta,
        success: Color::LightGreen,
        warning: Color::LightYellow,
        danger: Color::LightRed,
        info: Color::LightCyan,
    };

    pub const ANSI16_LIGHT: Self = Self {
        bg_base: Color::White,
        bg_raised: Color::Gray,
        bg_selected: Color::LightCyan,
        border: Color::DarkGray,
        fg_dim: Color::DarkGray,
        fg: Color::Black,
        accent: Color::Blue,
        alt: Color::Magenta,
        success: Color::Green,
        warning: Color::Yellow,
        danger: Color::Red,
        info: Color::Cyan,
    };

    /// Applies `depth` to every token, so quantisation happens once at
    /// startup rather than on every span.
    pub fn quantized(self, depth: ColorDepth) -> Self {
        if depth == ColorDepth::Ansi16 {
            return if self == Self::LIGHT {
                Self::ANSI16_LIGHT
            } else {
                Self::ANSI16_DARK
            };
        }
        let q = |c| depth.quantize(c);
        Self {
            bg_base: q(self.bg_base),
            bg_raised: q(self.bg_raised),
            bg_selected: q(self.bg_selected),
            border: q(self.border),
            fg_dim: q(self.fg_dim),
            fg: q(self.fg),
            accent: q(self.accent),
            alt: q(self.alt),
            success: q(self.success),
            warning: q(self.warning),
            danger: q(self.danger),
            info: q(self.info),
        }
    }

    /// Maps a 0.0 (perfect) → 1.0 (broken) severity onto the shared ramp.
    ///
    /// Bucketed rather than interpolated: five discrete stops are
    /// classifiable at a glance, where a smooth blend produces in-between
    /// hues the eye cannot name.
    pub fn severity(&self, severity: f64) -> Color {
        match severity {
            s if s <= 0.0 => self.success,
            s if s < 0.25 => self.info,
            s if s < 0.50 => self.warning,
            s if s < 0.75 => self.alt,
            _ => self.danger,
        }
    }

    /// Inverse of [`Self::severity`] for "how much is healthy" meters,
    /// where 1.0 is good — a full bar is green and an empty one red.
    pub fn health(&self, ratio: f64) -> Color {
        self.severity(1.0 - ratio.clamp(0.0, 1.0))
    }

    /// A rounded panel whose border brightens and whose title goes bold
    /// when the pane holds focus — the only focus cue that survives at any
    /// width, and the reason borders stay thin: heavy box-drawing raises
    /// the border's luminance mass above the data it frames.
    pub fn panel<'a>(&self, title: &'a str, focused: bool) -> Block<'a> {
        let (border, title_style) = if focused {
            (
                self.accent,
                Style::default()
                    .fg(self.accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (self.border, Style::default().fg(self.fg_dim))
        };
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .style(Style::default().bg(self.bg_base))
            .title(Span::styled(format!(" {title} "), title_style))
    }

    /// A status-line key hint: the key reversed into a chip, then its label.
    pub fn key_hint<'a>(&self, key: &'a str, label: &'a str) -> Vec<Span<'a>> {
        vec![
            Span::styled(
                format!(" {key} "),
                Style::default()
                    .fg(self.bg_base)
                    .bg(self.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {label}"), Style::default().fg(self.fg_dim)),
            Span::raw("   "),
        ]
    }
}

/// The wordmark, in half-block glyphs. Shown only where there is already
/// dead space — the empty state and the help overlay — never in persistent
/// chrome, where four rows out of ~34 is a tenth of the screen every frame.
pub const LOGO: [&str; 3] = ["█▀▀  █ ▄▀  ▀▄ ▄▀", "▀▀█  █▀▄    ▄▀▄ ", "▀▀▀  ▀  ▀  ▄▀ ▀▄"];

/// Formats a count for a narrow column: `980`, `3.3k`, `10.3k`, `173k`.
pub fn compact_count(n: usize) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        // One decimal while it still fits in five columns, so 3.3k and
        // 10.3k stay distinguishable; whole thousands once it doesn't.
        // Checking the formatted width rather than the input avoids the
        // boundary bug where 99_999 rounds up to a six-character "100.0k".
        let precise = format!("{:.1}k", n as f64 / 1000.0);
        return if precise.chars().count() <= 5 {
            precise
        } else {
            format!("{}k", n / 1000)
        };
    }
    format!("{:.1}M", n as f64 / 1_000_000.0)
}

/// Renders `ratio` as a fixed-`width` bar using eighth-block characters, so
/// a 12-cell bar resolves 96 steps instead of 12 — the difference between a
/// meter that visibly moves when one skill drifts and one that doesn't.
pub fn meter(ratio: f64, width: usize) -> String {
    const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let ratio = ratio.clamp(0.0, 1.0);
    let total_eighths = (ratio * (width * 8) as f64).round() as usize;
    let full = total_eighths / 8;
    let remainder = total_eighths % 8;

    let mut bar = String::with_capacity(width * 3);
    for _ in 0..full.min(width) {
        bar.push('█');
    }
    if full < width && remainder > 0 {
        bar.push(EIGHTHS[remainder - 1]);
    }
    let drawn = full.min(width) + usize::from(full < width && remainder > 0);
    for _ in drawn..width {
        bar.push('░');
    }
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("palette tokens must be RGB");
        };
        let channel = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (x, y) = (relative_luminance(a), relative_luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// The regression this exists to prevent: the palette was authored for
    /// a dark terminal and the app never painted a background, so on a
    /// light profile eight of ten tokens sat between 1.4:1 and 2.7:1 —
    /// invisible. Both themes are checked now, every build.
    #[test]
    fn every_text_token_meets_wcag_aa_against_its_own_background() {
        for (name, p) in [("dark", Palette::DARK), ("light", Palette::LIGHT)] {
            for (token, color) in [
                ("fg", p.fg),
                ("fg_dim", p.fg_dim),
                ("accent", p.accent),
                ("alt", p.alt),
                ("success", p.success),
                ("warning", p.warning),
                ("danger", p.danger),
                ("info", p.info),
            ] {
                let ratio = contrast(color, p.bg_base);
                assert!(
                    ratio >= 4.5,
                    "{name}.{token} is {ratio:.2}:1 on bg_base, need 4.5:1"
                );
            }
        }
    }

    #[test]
    fn text_stays_legible_on_the_selected_row() {
        for (name, p) in [("dark", Palette::DARK), ("light", Palette::LIGHT)] {
            for (token, color) in [("fg", p.fg), ("success", p.success), ("danger", p.danger)] {
                let ratio = contrast(color, p.bg_selected);
                assert!(
                    ratio >= 4.5,
                    "{name}.{token} is {ratio:.2}:1 on bg_selected, need 4.5:1"
                );
            }
        }
    }

    /// WCAG 1.4.11: non-text UI components need 3:1.
    #[test]
    fn borders_meet_non_text_contrast() {
        for (name, p) in [("dark", Palette::DARK), ("light", Palette::LIGHT)] {
            let ratio = contrast(p.border, p.bg_base);
            assert!(ratio >= 3.0, "{name}.border is {ratio:.2}:1, need 3:1");
        }
    }

    #[test]
    fn decorative_and_semantic_tokens_are_distinguishable() {
        // These were literally the same RGB, which made the decorative
        // scope badge read as a success signal...
        assert_ne!(Palette::DARK.alt, Palette::DARK.success);
        // ...and these were too, which made a table header
        // indistinguishable from the selected row.
        assert_ne!(Palette::DARK.bg_selected, Palette::DARK.bg_raised);
        assert_ne!(Palette::LIGHT.alt, Palette::LIGHT.success);
        assert_ne!(Palette::LIGHT.bg_selected, Palette::LIGHT.bg_raised);
    }

    #[test]
    fn the_severity_ramp_runs_success_to_danger() {
        let p = Palette::DARK;
        assert_eq!(p.severity(0.0), p.success);
        assert_eq!(p.severity(1.0), p.danger);
        assert_eq!(p.health(1.0), p.success);
        assert_eq!(p.health(0.0), p.danger);
    }

    /// The surface hierarchy has to survive every colour depth: if the
    /// header row and the selected row land on the same value, the single
    /// most important state signal in the table disappears.
    #[test]
    fn the_no_color_palette_carries_no_colour_at_all() {
        // no-color.org asks for the absence of colour, not a muted theme.
        let p = Palette::NO_COLOR;
        for color in [
            p.bg_base, p.fg, p.accent, p.success, p.warning, p.danger, p.border,
        ] {
            assert_eq!(color, Color::Reset);
        }
        assert!(p.is_monochrome());
        assert!(!Palette::DARK.is_monochrome());
        assert!(!Palette::LIGHT.is_monochrome());
    }

    #[test]
    fn the_three_surfaces_stay_distinct_at_every_depth() {
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ] {
            for base in [Palette::DARK, Palette::LIGHT] {
                let p = base.quantized(depth);
                assert_ne!(p.bg_base, p.bg_raised, "{depth:?} collapsed base/raised");
                assert_ne!(
                    p.bg_base, p.bg_selected,
                    "{depth:?} collapsed base/selected"
                );
                assert_ne!(
                    p.bg_raised, p.bg_selected,
                    "{depth:?} collapsed raised/selected"
                );
            }
        }
    }

    #[test]
    fn quantizing_never_leaves_an_rgb_token_behind() {
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
            let p = Palette::DARK.quantized(depth);
            for color in [p.bg_base, p.fg, p.accent, p.success, p.danger, p.border] {
                assert!(
                    !matches!(color, Color::Rgb(..)),
                    "{depth:?} left an Rgb token unquantized"
                );
            }
        }
        assert_eq!(
            Palette::DARK.quantized(ColorDepth::TrueColor),
            Palette::DARK
        );
    }

    #[test]
    fn meter_is_always_exactly_width_chars() {
        for width in [1usize, 8, 12, 30] {
            for step in 0..=20 {
                let bar = meter(step as f64 / 20.0, width);
                assert_eq!(
                    bar.chars().count(),
                    width,
                    "ratio {step}/20 at width {width}"
                );
            }
        }
        assert_eq!(meter(-5.0, 4), "░░░░");
        assert_eq!(meter(9.9, 4), "████");
    }

    #[test]
    fn compact_count_stays_within_five_columns() {
        for n in [
            0usize, 7, 999, 1_000, 3_272, 10_342, 99_999, 173_451, 2_500_000,
        ] {
            assert!(
                compact_count(n).chars().count() <= 5,
                "{n} → {}",
                compact_count(n)
            );
        }
        assert_eq!(compact_count(980), "980");
        assert_eq!(compact_count(3_272), "3.3k");
        assert_eq!(compact_count(10_342), "10.3k");
        assert_eq!(compact_count(173_451), "173k");
        // The rounding boundary a naive range match gets wrong.
        assert_eq!(compact_count(99_999), "99k");
    }
}
