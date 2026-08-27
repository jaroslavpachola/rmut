//! The front-end styles as ratatui has them.

use rmut_front::style::{Color, Style};

pub fn color(c: Color) -> ratatui::style::Color {
    use ratatui::style::Color as R;
    match c {
        Color::Reset => R::Reset,
        Color::Black => R::Black,
        Color::Red => R::Red,
        Color::Green => R::Green,
        Color::Yellow => R::Yellow,
        Color::Blue => R::Blue,
        Color::Magenta => R::Magenta,
        Color::Cyan => R::Cyan,
        Color::White => R::White,
        Color::DarkGray => R::DarkGray,
        Color::LightRed => R::LightRed,
        Color::LightGreen => R::LightGreen,
        Color::LightYellow => R::LightYellow,
        Color::LightBlue => R::LightBlue,
        Color::LightMagenta => R::LightMagenta,
        Color::LightCyan => R::LightCyan,
    }
}

pub fn style(s: Style) -> ratatui::style::Style {
    use ratatui::style::Modifier;
    let mut out = ratatui::style::Style::new();
    if let Some(fg) = s.fg {
        out = out.fg(color(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(color(bg));
    }
    if s.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if s.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if s.reversed {
        out = out.add_modifier(Modifier::REVERSED);
    }
    out
}
