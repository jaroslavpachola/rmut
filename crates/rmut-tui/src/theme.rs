use ratatui::style::{Color, Modifier, Style};
use rmut_core::config::Config;

#[derive(Clone, Copy)]
pub struct Theme {
    pub bar_fg: Color,
    pub bar_bg: Color,
    bar_reversed: bool,
    pub deleted: Color,
    pub flagged: Color,
    pub header: Color,
}

impl Theme {
    fn preset(name: &str) -> Theme {
        match name {
            "mono" => Theme {
                bar_fg: Color::Reset,
                bar_bg: Color::Reset,
                bar_reversed: true,
                deleted: Color::Reset,
                flagged: Color::Reset,
                header: Color::Reset,
            },
            // mutt's default look
            _ => Theme {
                bar_fg: Color::Black,
                bar_bg: Color::Cyan,
                bar_reversed: false,
                deleted: Color::Red,
                flagged: Color::Yellow,
                header: Color::Green,
            },
        }
    }

    pub fn from_config(cfg: &Config) -> (Theme, Vec<String>) {
        let mut theme = Theme::preset(cfg.ui.theme.as_deref().unwrap_or("default"));
        let mut warnings = Vec::new();
        for (key, value) in &cfg.colors {
            let Some(color) = parse_color(value) else {
                warnings.push(format!("unknown color {value:?}"));
                continue;
            };
            match key.as_str() {
                "status_fg" => {
                    theme.bar_fg = color;
                    theme.bar_reversed = false;
                }
                "status_bg" => {
                    theme.bar_bg = color;
                    theme.bar_reversed = false;
                }
                "deleted" => theme.deleted = color,
                "flagged" => theme.flagged = color,
                "header" => theme.header = color,
                other => warnings.push(format!("unknown color key {other:?}")),
            }
        }
        (theme, warnings)
    }

    pub fn bar_style(&self) -> Style {
        if self.bar_reversed {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.bar_fg).bg(self.bar_bg)
        }
    }
}

pub fn parse_color(name: &str) -> Option<Color> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_apply_and_warn() {
        let cfg: Config =
            toml::from_str("[colors]\ndeleted = \"blue\"\nbogus = \"red\"\nflagged = \"nope\"\n")
                .unwrap();
        let (theme, warnings) = Theme::from_config(&cfg);
        assert_eq!(theme.deleted, Color::Blue);
        assert_eq!(theme.flagged, Color::Yellow); // bad value kept default
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn mono_preset_reverses_bar() {
        let cfg: Config = toml::from_str("[ui]\ntheme = \"mono\"\n").unwrap();
        let (theme, warnings) = Theme::from_config(&cfg);
        assert!(warnings.is_empty());
        assert_eq!(
            theme.bar_style(),
            Style::new().add_modifier(Modifier::REVERSED)
        );
    }
}
