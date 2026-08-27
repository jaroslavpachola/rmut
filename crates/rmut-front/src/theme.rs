//! mutt's look, from the config: the slot colors, the quote
//! palette, the status bar. In front-end terms; each front end maps
//! them onto its own styles.

use rmut_core::config::Config;

use crate::style::{Color, Style, parse_color};

#[derive(Clone)]
pub struct Theme {
    pub bar_fg: Color,
    pub bar_bg: Color,
    bar_reversed: bool,
    pub deleted: Color,
    pub flagged: Color,
    pub tagged: Color,
    pub header: Color,
    /// Quote-depth palette (mutt's `color quoted`, `quoted1`…): depth
    /// d takes entry (d-1) mod len; empty = quotes stay untinted.
    pub quoted: Vec<Color>,
    /// Search-hit highlight in the pager (mutt's `color search`).
    pub search: Style,
    /// Error statuses on the bottom line (mutt's `color error`,
    /// bold bright red by default).
    pub error: Style,
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
                tagged: Color::Reset,
                header: Color::Reset,
                quoted: Vec::new(),
                search: Style::new().reversed(),
                error: Style::new().bold().reversed(),
            },
            // mutt's default look
            _ => Theme {
                bar_fg: Color::Black,
                bar_bg: Color::Cyan,
                bar_reversed: false,
                deleted: Color::Red,
                flagged: Color::Yellow,
                tagged: Color::Cyan,
                header: Color::Green,
                quoted: vec![Color::Cyan],
                search: Style::new().reversed(),
                error: Style::new().fg(Color::LightRed).bold(),
            },
        }
    }

    pub fn from_config(cfg: &Config) -> (Theme, Vec<String>) {
        let mut theme = Theme::preset(cfg.ui.theme.as_deref().unwrap_or("default"));
        let mut warnings = Vec::new();
        // `quoted`, `quoted1`… build the depth palette in N order
        // (HashMap iteration is unordered, so collect first).
        let mut quoted: std::collections::BTreeMap<usize, Color> =
            std::collections::BTreeMap::new();
        let (mut search_fg, mut search_bg) = (None, None);
        for (key, value) in &cfg.colors {
            let Some(color) = parse_color(value) else {
                warnings.push(format!("unknown color {value:?}"));
                continue;
            };
            if let Some(n) = key.strip_prefix("quoted")
                && let Ok(depth) = if n.is_empty() { Ok(0) } else { n.parse() }
            {
                quoted.insert(depth, color);
                continue;
            }
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
                "tagged" => theme.tagged = color,
                "header" => theme.header = color,
                "search_fg" => search_fg = Some(color),
                "search_bg" => search_bg = Some(color),
                "error" => {
                    theme.error = Style::new().fg(color).bold();
                }
                other => warnings.push(format!("unknown color key {other:?}")),
            }
        }
        if !quoted.is_empty() {
            theme.quoted = quoted.into_values().collect();
        }
        if search_fg.is_some() || search_bg.is_some() {
            let mut style = Style::new();
            if let Some(fg) = search_fg {
                style = style.fg(fg);
            }
            if let Some(bg) = search_bg {
                style = style.bg(bg);
            }
            theme.search = style;
        }
        (theme, warnings)
    }

    pub fn bar_style(&self) -> Style {
        if self.bar_reversed {
            Style::new().reversed()
        } else {
            Style::new().fg(self.bar_fg).bg(self.bar_bg)
        }
    }
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
    fn quoted_palette_and_search_override() {
        let cfg: Config = toml::from_str(
            "[colors]\nquoted = \"red\"\nquoted2 = \"blue\"\nsearch_bg = \"yellow\"\n",
        )
        .unwrap();
        let (theme, warnings) = Theme::from_config(&cfg);
        assert!(warnings.is_empty());
        assert_eq!(theme.quoted, vec![Color::Red, Color::Blue]);
        assert_eq!(theme.search, Style::new().bg(Color::Yellow));
        // Untouched, the default theme tints quotes cyan and reverses
        // search hits.
        let (plain, _) = Theme::from_config(&Config::default());
        assert_eq!(plain.quoted, vec![Color::Cyan]);
        assert_eq!(plain.search, Style::new().reversed());
    }

    #[test]
    fn mono_preset_reverses_bar() {
        let cfg: Config = toml::from_str("[ui]\ntheme = \"mono\"\n").unwrap();
        let (theme, warnings) = Theme::from_config(&cfg);
        assert!(warnings.is_empty());
        assert_eq!(theme.bar_style(), Style::new().reversed());
    }
}
