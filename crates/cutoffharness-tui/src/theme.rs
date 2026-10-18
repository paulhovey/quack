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
                .bg(Color::Rgb(40, 52, 70))
                .add_modifier(Modifier::BOLD),
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
                .bg(Color::Rgb(215, 228, 245))
                .add_modifier(Modifier::BOLD),
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
        }
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
                assistant: "●",
            }
        }
    }
}
