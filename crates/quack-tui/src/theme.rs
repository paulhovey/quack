//! The two compiled-in themes. Further themes ship as TOML in the defaults bundle (M3).

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

#[derive(Debug, Clone)]
pub(crate) struct Theme {
    pub accent: Style,
    pub dim: Style,
    pub user: Style,
    pub ok: Style,
    pub warn: Style,
    pub error: Style,
    pub border: Style,
    pub selected: Style,
    /// Secondary text (hints, descriptions) on the selected row, where `dim` would vanish.
    pub sel_text: Style,
    /// Letters a fuzzy filter matched.
    pub hit: Style,
    /// Background band behind the user's messages.
    pub user_bg: Style,
    /// Second accent, for plan mode and the welcome duck's beak.
    pub magenta: Style,
    /// Text on a coloured pill (status-bar mode, approval keys).
    pub on_pill: Color,
    /// Gradient stops for the header wordmark; empty means a flat accent.
    pub gradient: &'static [(u8, u8, u8)],
}

impl Theme {
    pub fn resolve(name: &str, no_color: bool) -> Self {
        if no_color {
            return Self::plain();
        }
        match name {
            "light" => Self::light(),
            // `auto` will query the background colour (OSC 11) in M1; dark until then.
            _ => Self::dark(),
        }
    }

    fn dark() -> Self {
        Self {
            accent: Style::new().fg(Color::Cyan),
            dim: Style::new().fg(Color::DarkGray),
            user: Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ok: Style::new().fg(Color::Green),
            warn: Style::new().fg(Color::Yellow),
            error: Style::new().fg(Color::Red),
            border: Style::new().fg(Color::DarkGray),
            selected: Style::new()
                .bg(Color::Rgb(38, 62, 88))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            sel_text: Style::new().fg(Color::Rgb(184, 196, 212)),
            hit: Style::new()
                .fg(Color::Rgb(255, 214, 10))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            user_bg: Style::new().bg(Color::Rgb(34, 38, 46)),
            magenta: Style::new().fg(Color::Magenta),
            on_pill: Color::Black,
            gradient: &[(255, 214, 10), (255, 159, 67), (94, 200, 229)],
        }
    }

    fn light() -> Self {
        Self {
            accent: Style::new().fg(Color::Blue),
            dim: Style::new().fg(Color::Gray),
            user: Style::new().fg(Color::Black).add_modifier(Modifier::BOLD),
            ok: Style::new().fg(Color::Green),
            warn: Style::new().fg(Color::Rgb(160, 110, 0)),
            error: Style::new().fg(Color::Red),
            border: Style::new().fg(Color::Gray),
            selected: Style::new()
                .bg(Color::Rgb(206, 224, 248))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            sel_text: Style::new().fg(Color::Rgb(60, 70, 88)),
            hit: Style::new()
                .fg(Color::Rgb(200, 90, 0))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            user_bg: Style::new().bg(Color::Rgb(236, 239, 244)),
            magenta: Style::new().fg(Color::Magenta),
            on_pill: Color::White,
            gradient: &[(200, 140, 0), (220, 90, 30), (20, 110, 170)],
        }
    }

    /// `NO_COLOR`: modifiers only.
    fn plain() -> Self {
        let none = Style::new();
        Self {
            accent: none.add_modifier(Modifier::BOLD),
            dim: none.add_modifier(Modifier::DIM),
            user: none.add_modifier(Modifier::BOLD),
            ok: none,
            warn: none,
            error: none.add_modifier(Modifier::BOLD),
            border: none,
            selected: none.add_modifier(Modifier::REVERSED),
            sel_text: none,
            hit: none.add_modifier(Modifier::UNDERLINED),
            user_bg: none,
            magenta: none,
            on_pill: Color::Reset,
            gradient: &[],
        }
    }

    /// A filled label: the style's colour as background. Without colour, reversed.
    pub fn pill(&self, style: Style) -> Style {
        match style.fg {
            Some(c) => Style::new()
                .bg(c)
                .fg(self.on_pill)
                .add_modifier(Modifier::BOLD),
            None => Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }

    /// The colour for a permission mode.
    pub fn mode(&self, mode: &str) -> Style {
        match mode {
            "read-only" => self.user,
            "auto-edit" => self.ok,
            "full-auto" => self.warn,
            "plan" => self.magenta,
            "yolo" => self.error,
            _ => self.accent,
        }
    }

    /// The colour for a tool, by the first word of its summary.
    pub fn tool(&self, name: &str) -> Style {
        match name {
            "read" | "ls" | "glob" | "grep" | "find" | "lsp" | "index" => self.accent,
            "edit" | "write" | "edit_hashline" | "patch" | "apply_patch" => self.warn,
            "shell" | "bash" | "$" | "run" => self.magenta,
            "plan" | "todo" => self.ok,
            _ => self.accent,
        }
    }

    /// `text` in `base`, with the chars at `hits` (sorted positions) in [`Theme::hit`].
    pub fn highlight(
        &self,
        text: &str,
        hits: &[usize],
        base: Style,
    ) -> Vec<ratatui::text::Span<'static>> {
        use ratatui::text::Span;
        if hits.is_empty() {
            return vec![Span::styled(text.to_owned(), base)];
        }
        let mut out: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_hit = false;
        for (i, c) in text.chars().enumerate() {
            let is_hit = hits.binary_search(&i).is_ok();
            if is_hit != run_hit && !run.is_empty() {
                let style = if run_hit { base.patch(self.hit) } else { base };
                out.push(Span::styled(std::mem::take(&mut run), style));
            }
            run_hit = is_hit;
            run.push(c);
        }
        if !run.is_empty() {
            let style = if run_hit { base.patch(self.hit) } else { base };
            out.push(Span::styled(run, style));
        }
        out
    }

    /// `text` with each character coloured along the gradient.
    pub fn gradient_spans(&self, text: &str) -> Vec<ratatui::text::Span<'static>> {
        use ratatui::text::Span;
        let bold = Modifier::BOLD;
        if self.gradient.len() < 2 {
            return vec![Span::styled(
                text.to_owned(),
                self.accent.add_modifier(bold),
            )];
        }
        let n = text.chars().count().max(2) - 1;
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                #[allow(clippy::cast_precision_loss)]
                let pos = i as f32 / n as f32 * (self.gradient.len() - 1) as f32;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let seg = (pos.floor() as usize).min(self.gradient.len() - 2);
                let f = pos - seg as f32;
                let (a, b) = (self.gradient[seg], self.gradient[seg + 1]);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let mix =
                    |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * f).round() as u8;
                Span::styled(
                    c.to_string(),
                    Style::new()
                        .fg(Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2)))
                        .add_modifier(bold),
                )
            })
            .collect()
    }
}

/// A small meter, e.g. `▰▰▰▱▱`, filled `frac` of `width` cells.
pub(crate) fn meter(frac: f32, width: usize, ascii: bool) -> String {
    let (on, off) = if ascii { ('#', '-') } else { ('▰', '▱') };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let filled = ((frac.clamp(0.0, 1.0) * width as f32).round() as usize).min(width);
    std::iter::repeat_n(on, filled)
        .chain(std::iter::repeat_n(off, width - filled))
        .collect()
}

/// `1.2s`, `14s`, `2m05s`.
pub(crate) fn elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs_f32();
    if s < 10.0 {
        format!("{s:.1}s")
    } else if s < 60.0 {
        format!("{s:.0}s")
    } else {
        let s = d.as_secs();
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// Glyphs, with an ASCII fallback for fonts lacking box-drawing or braille.
#[derive(Debug, Clone)]
pub(crate) struct Glyphs {
    pub ok: &'static str,
    pub fail: &'static str,
    pub disabled: &'static str,
    pub attention: &'static str,
    pub pending: &'static str,
    pub spinner: &'static [&'static str],
    pub border: border::Set<'static>,
    pub prompt: &'static str,
    pub assistant: &'static str,
    /// Plan items: done, in progress, pending.
    pub todo: [&'static str; 3],
    /// Gutter beside diff lines.
    pub gutter: &'static str,
    pub rule: &'static str,
    pub think: &'static str,
    pub branch: &'static str,
    /// Left edge of the selected row in lists.
    pub bar: &'static str,
    pub scroll: [&'static str; 2],
}

pub(crate) const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

impl Glyphs {
    pub fn new(ascii: bool) -> Self {
        if ascii {
            Self {
                ok: "+",
                fail: "x",
                disabled: "-",
                attention: "?",
                pending: ".",
                spinner: &["|", "/", "-", "\\"],
                border: ASCII_BORDER,
                prompt: ">",
                assistant: "*",
                todo: ["[x]", "[~]", "[ ]"],
                gutter: "|",
                rule: "-",
                think: "~",
                branch: "git:",
                bar: ">",
                scroll: ["#", "|"],
            }
        } else {
            Self {
                ok: "✓",
                fail: "✗",
                disabled: "–",
                attention: "?",
                pending: "·",
                spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
                border: border::ROUNDED,
                prompt: "›",
                assistant: "🦆",
                todo: ["✓", "▶", "·"],
                gutter: "▎",
                rule: "─",
                think: "✻",
                branch: "⎇ ",
                bar: "▌",
                scroll: ["┃", "│"],
            }
        }
    }
}

/// Token counts for display: `850`, `12.3k`, `256k`, `1M`, `1.05M`.
pub(crate) fn tokens(n: u32) -> String {
    let n = f64::from(n);
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s
        }
    };
    if n >= 1_000_000.0 {
        format!("{}M", trim(format!("{:.2}", n / 1_000_000.0)))
    } else if n >= 100_000.0 {
        format!("{:.0}k", n / 1000.0)
    } else if n >= 1000.0 {
        format!("{}k", trim(format!("{:.1}", n / 1000.0)))
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod token_tests {
    #[test]
    fn token_display() {
        assert_eq!(super::tokens(850), "850");
        assert_eq!(super::tokens(12_345), "12.3k");
        assert_eq!(super::tokens(262_144), "262k");
        assert_eq!(super::tokens(1_000_000), "1M");
        assert_eq!(super::tokens(1_048_576), "1.05M");
    }
}
