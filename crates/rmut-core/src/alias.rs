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

/// Append `alias nick expansion` to the alias file (create-alias),
/// creating the file if needed. A repeated nick wins by coming later.
/// Returns the path written.
pub fn append(nick: &str, expansion: &str) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    use std::io::Write;
    let path = default_path().context("no alias file path ($HOME unset)")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "alias {nick} {expansion}")?;
    Ok(path)
}

/// Completion candidates for a partial address: expansions of every
/// alias whose nick starts with `word` (case-insensitive), then
/// query_command results — sorted, deduplicated.
pub fn complete(
    word: &str,
    aliases: &HashMap<String, String>,
    query_command: Option<&str>,
) -> Vec<String> {
    let lower = word.to_lowercase();
    let mut out: Vec<String> = aliases
        .iter()
        .filter(|(nick, _)| nick.to_lowercase().starts_with(&lower))
        .map(|(_, expansion)| expansion.clone())
        .collect();
    out.sort();
    if let Some(command) = query_command {
        out.extend(query(command, word));
    }
    out.dedup();
    out
}

/// Run mutt's query_command (`%s` = the search word, appended when the
/// command has no `%s`) and parse its output: the first line is a
/// human message, then one `address<TAB>name[<TAB>extra]` per line.
pub fn query(command: &str, word: &str) -> Vec<String> {
    let quoted = format!("'{}'", word.replace('\'', r"'\''"));
    let command = if command.contains("%s") {
        command.replace("%s", &quoted)
    } else {
        format!("{command} {quoted}")
    };
    let Ok(out) = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let addr = fields.next()?.trim();
            if addr.is_empty() {
                return None;
            }
            Some(
                match fields.next().map(str::trim).filter(|n| !n.is_empty()) {
                    Some(name) => format!("{name} <{addr}>"),
                    None => addr.to_string(),
                },
            )
        })
        .collect()
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
    fn complete_matches_nick_prefixes() {
        let map = parse(
            "alias petr Petr Novak <petr@example.com>\nalias pete pete@example.org\nalias jane jane@example.com\n",
        );
        assert_eq!(
            complete("PE", &map, None),
            vec![
                "Petr Novak <petr@example.com>".to_string(),
                "pete@example.org".into(),
            ]
        );
        assert_eq!(complete("jane", &map, None).len(), 1);
        assert!(complete("zz", &map, None).is_empty());
    }

    #[test]
    fn query_parses_mutt_output() {
        // First line is a message; addr\tname\textra lines follow.
        let cmd =
            "printf 'Searching %s...\\nzdenka@example.com\\tZdenka Q\\tnote\\nbare@example.com\\n'";
        assert_eq!(
            query(cmd, "zd"),
            vec![
                "Zdenka Q <zdenka@example.com>".to_string(),
                "bare@example.com".into(),
            ]
        );
        // The word reaches the command shell-quoted, quotes included.
        assert_eq!(query("echo dummy; echo %s", "a'b"), vec!["a'b".to_string()]);
        assert!(query("false", "x").is_empty());
        // Query results merge behind alias matches in complete().
        let map = parse("alias zdeno zdeno@example.net\n");
        let all = complete(
            "zd",
            &map,
            Some("printf 'found\\nzdenka@example.com\\tZdenka Q\\n'"),
        );
        assert_eq!(
            all,
            vec![
                "zdeno@example.net".to_string(),
                "Zdenka Q <zdenka@example.com>".into(),
            ]
        );
    }

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
