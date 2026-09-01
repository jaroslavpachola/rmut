//! How text is colored, said without a toolkit: the sixteen named
//! colors a muttrc can name, and a style that is a color pair plus
//! the three attributes mutt's `color` lines carry. Each front end
//! maps this onto its own kind of style.

/// A terminal palette color. `Reset` is the front end's default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    /// A truecolor value, from `#rrggbb` in the config. Terminals
    /// carry it as 24-bit color; the window uses it directly.
    Rgb(u8, u8, u8),
}

/// A style patch: what a color rule sets, leaving the rest alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub underline: bool,
    pub reversed: bool,
}

impl Style {
    pub const fn new() -> Style {
        Style {
            fg: None,
            bg: None,
            bold: false,
            underline: false,
            reversed: false,
        }
    }

    pub const fn fg(mut self, color: Color) -> Style {
        self.fg = Some(color);
        self
    }

    pub const fn bg(mut self, color: Color) -> Style {
        self.bg = Some(color);
        self
    }

    pub const fn bold(mut self) -> Style {
        self.bold = true;
        self
    }

    pub const fn underline(mut self) -> Style {
        self.underline = true;
        self
    }

    pub const fn reversed(mut self) -> Style {
        self.reversed = true;
        self
    }

    /// This style with `other` laid over it: `other`'s colors where
    /// it has them, its attributes added.
    pub fn patch(self, other: Style) -> Style {
        Style {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            underline: self.underline || other.underline,
            reversed: self.reversed || other.reversed,
        }
    }
}

/// A color as a muttrc or the config names it.
pub fn parse_color(name: &str) -> Option<Color> {
    if let Some(hex) = name.strip_prefix('#')
        && hex.len() == 6
        && let Ok(v) = u32::from_str_radix(hex, 16)
    {
        return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    Some(match name.to_lowercase().as_str() {
        "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" | "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        _ => return None,
    })
}

/// A `color_index` / `color_body` rule's look: a color, or an
/// attribute name (bold, underline, reverse, standout; none clears
/// nothing, as in the TUI it always did), in either slot.
pub fn rule_style(
    rule: &rmut_core::config::ColorRule,
    what: &str,
    warnings: &mut Vec<String>,
) -> Style {
    let mut style = Style::new();
    for (name, is_fg) in [(&rule.fg, true), (&rule.bg, false)] {
        let Some(name) = name else { continue };
        match name.as_str() {
            "bold" => style = style.bold(),
            "underline" => style = style.underline(),
            "reverse" | "standout" => style = style.reversed(),
            "none" => {}
            _ => match parse_color(name) {
                Some(color) if is_fg => style = style.fg(color),
                Some(color) => style = style.bg(color),
                None => warnings.push(format!("unknown {what} color {name:?}")),
            },
        }
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_parse_and_bad_ones_do_not() {
        assert_eq!(parse_color("#ff8000"), Some(Color::Rgb(255, 128, 0)));
        assert_eq!(parse_color("#FF8000"), Some(Color::Rgb(255, 128, 0)));
        assert_eq!(parse_color("#f80"), None, "three digits stay unknown");
        assert_eq!(parse_color("#zzzzzz"), None);
    }

    #[test]
    fn patch_overlays_colors_and_adds_attributes() {
        let base = Style::new().fg(Color::Red).bold();
        let over = Style::new().bg(Color::Blue).reversed();
        let got = base.patch(over);
        assert_eq!(
            got,
            Style::new()
                .fg(Color::Red)
                .bg(Color::Blue)
                .bold()
                .reversed()
        );
        let got = got.patch(Style::new().fg(Color::Green));
        assert_eq!(got.fg, Some(Color::Green));
        assert!(got.bold && got.reversed);
    }

    #[test]
    fn rules_read_attributes_and_colors_in_either_slot() {
        let rule = rmut_core::config::ColorRule {
            pattern: String::new(),
            fg: Some("bold".into()),
            bg: Some("blue".into()),
        };
        let mut warnings = Vec::new();
        let style = rule_style(&rule, "color_index", &mut warnings);
        assert_eq!(style, Style::new().bold().bg(Color::Blue));
        assert!(warnings.is_empty());
        let rule = rmut_core::config::ColorRule {
            pattern: String::new(),
            fg: Some("chartreuse".into()),
            bg: None,
        };
        assert_eq!(rule_style(&rule, "color_body", &mut warnings), Style::new());
        assert_eq!(warnings.len(), 1);
    }
}
