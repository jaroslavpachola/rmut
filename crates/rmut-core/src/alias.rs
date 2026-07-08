//! Mutt-style address aliases: lines of `alias nick expansion...` read
//! from $RMUT_ALIASES or ~/.config/rmut/aliases.

use std::collections::HashMap;
use std::path::PathBuf;

pub fn default_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RMUT_ALIASES") {
        return Some(PathBuf::from(p));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/rmut/aliases"))
}

pub fn load_default() -> HashMap<String, String> {
    default_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse(&t))
        .unwrap_or_default()
}

pub fn parse(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("alias ") {
            let mut parts = rest.trim().splitn(2, char::is_whitespace);
            if let (Some(nick), Some(expansion)) = (parts.next(), parts.next()) {
                map.insert(nick.to_string(), expansion.trim().to_string());
            }
        }
    }
    map
}

/// Expand comma-separated recipients; a bare token exactly matching an
/// alias nick is replaced by its expansion.
pub fn expand(input: &str, aliases: &HashMap<String, String>) -> String {
    input
        .split(',')
        .map(|token| {
            let t = token.trim();
            aliases.get(t).cloned().unwrap_or_else(|| t.to_string())
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_expand() {
        let map = parse(
            "# my aliases\nalias jane Jane Doe <jane@example.com>\nalias team a@x, b@y\nnot an alias line\n",
        );
        assert_eq!(map.len(), 2);
        assert_eq!(
            expand("jane, chief@corp", &map),
            "Jane Doe <jane@example.com>, chief@corp"
        );
        assert_eq!(expand("team", &map), "a@x, b@y");
        assert_eq!(expand("nobody", &HashMap::new()), "nobody");
    }
}
