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

/// The alias file: what the config names (mutt's $alias_file), else
/// $RMUT_ALIASES, else ~/.config/rmut/aliases. A leading `~` is the
/// home directory, as it is everywhere else.
pub fn path_for(configured: Option<&str>) -> Option<PathBuf> {
    match configured.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => Some(expand_home(path)),
        None => default_path(),
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home).join(rest),
            Err(_) => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

/// The aliases in the file the config names, or the default one.
pub fn load(configured: Option<&str>) -> HashMap<String, String> {
    path_for(configured)
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
/// The same, into the file the config names.
pub fn append_to(configured: Option<&str>, nick: &str, expansion: &str) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    use std::io::Write;
    let path = path_for(configured).context("no alias file path ($HOME unset)")?;
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

/// mutt's unalias: drop these nicks (or every alias, for `*`) from
/// the alias file, rewriting it without them. How many lines went.
pub fn remove_from(configured: Option<&str>, nicks: &[String]) -> anyhow::Result<usize> {
    use anyhow::Context;
    let path = path_for(configured).context("no alias file path ($HOME unset)")?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(0);
    };
    let all = nicks.iter().any(|n| n == "*");
    let mut removed = 0;
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let nick = line
                .trim()
                .strip_prefix("alias ")
                .and_then(|rest| rest.split_whitespace().next());
            let goes = nick.is_some_and(|n| all || nicks.iter().any(|w| w == n));
            removed += usize::from(goes);
            !goes
        })
        .collect();
    if removed > 0 {
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(removed)
}

/// Completion candidates for a partial address: expansions of every
/// alias whose nick starts with `word` (case-insensitive), then
/// query_command results, sorted and deduplicated. `sort` is mutt's
/// $sort_alias: "address" (by the expansion; the default), "alias"
/// (by nick; "unsorted" reads the same, the file being a map), with
/// "reverse-" flipping either.
pub fn complete(
    word: &str,
    aliases: &HashMap<String, String>,
    query_command: Option<&str>,
    sort: Option<&str>,
) -> Vec<String> {
    let lower = word.to_lowercase();
    let mut hits: Vec<(&String, &String)> = aliases
        .iter()
        .filter(|(nick, _)| nick.to_lowercase().starts_with(&lower))
        .collect();
    let (reverse, key) = match sort.unwrap_or("address").strip_prefix("reverse-") {
        Some(key) => (true, key),
        None => (false, sort.unwrap_or("address")),
    };
    match key {
        "alias" | "unsorted" => hits.sort_by(|a, b| a.0.cmp(b.0)),
        _ => hits.sort_by(|a, b| a.1.cmp(b.1)),
    }
    if reverse {
        hits.reverse();
    }
    let mut out: Vec<String> = hits.into_iter().map(|(_, e)| e.clone()).collect();
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
            complete("PE", &map, None, None),
            vec![
                "Petr Novak <petr@example.com>".to_string(),
                "pete@example.org".into(),
            ]
        );
        assert_eq!(complete("jane", &map, None, None).len(), 1);
        assert!(complete("zz", &map, None, None).is_empty());
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
            None,
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

    #[test]
    fn unalias_rewrites_the_file_without_the_nicks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aliases");
        std::fs::write(
            &path,
            "# mine\nalias jane Jane <jane@example.com>\nalias bob bob@example.com\nalias al Al <al@example.com>\n",
        )
        .unwrap();
        let configured = path.to_string_lossy().to_string();
        assert_eq!(
            remove_from(Some(&configured), &["bob".to_string()]).unwrap(),
            1
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("bob") && text.contains("jane") && text.starts_with("# mine"));
        assert_eq!(
            remove_from(Some(&configured), &["nobody".to_string()]).unwrap(),
            0
        );
        assert_eq!(
            remove_from(Some(&configured), &["*".to_string()]).unwrap(),
            2
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine\n");
    }

    #[test]
    fn completion_order_follows_sort_alias() {
        let aliases: HashMap<String, String> = [
            ("zed".to_string(), "Aaron <aaron@example.com>".to_string()),
            ("amy".to_string(), "Zoe <zoe@example.com>".to_string()),
        ]
        .into_iter()
        .collect();
        let by_address = complete("", &aliases, None, None);
        assert!(by_address[0].starts_with("Aaron"), "{by_address:?}");
        let by_alias = complete("", &aliases, None, Some("alias"));
        assert!(
            by_alias[0].starts_with("Zoe"),
            "amy before zed: {by_alias:?}"
        );
        let reversed = complete("", &aliases, None, Some("reverse-alias"));
        assert!(reversed[0].starts_with("Aaron"), "{reversed:?}");
    }
}
