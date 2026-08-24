//! mutt's enter-command (`:`): config commands typed at runtime.
//!
//! Parsing is shared with the muttrc importer (same tokenizer, same
//! option and color vocabulary), so what `--import-muttrc` understands
//! in a file is also what `:` understands at the prompt. Applying is
//! split: everything that is pure [`Config`] state lands in [`apply`],
//! while binds, macros, aliases, and key pushing belong to the running
//! TUI and are returned to the caller untouched. A later folder-hook
//! with arbitrary commands parses the same way and applies the same
//! [`Config`] half.

use crate::config::{ColorRule, Config};
use crate::muttrc::{assignments, convert_color, is_yes, split_from, tokenize};

/// Which key table a `bind`/`macro` targets. `Generic` is mutt's
/// catch-all menu, applied to both of rmut's tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Menu {
    Index,
    Pager,
    Generic,
}

impl Menu {
    fn parse(name: &str) -> Option<Menu> {
        match name {
            "index" => Some(Menu::Index),
            "pager" => Some(Menu::Pager),
            "generic" => Some(Menu::Generic),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Menu::Index => "index",
            Menu::Pager => "pager",
            Menu::Generic => "generic",
        }
    }
}

/// One command off the `:` prompt. A single line can yield several
/// (`set a=1 b=2` is two assignments, like mutt).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Set {
        name: String,
        value: String,
    },
    Unset(String),
    Toggle(String),
    /// `set name?`: report the value instead of changing it.
    Query(String),
    Bind {
        menu: Menu,
        key: String,
        function: String,
    },
    Macro {
        menu: Menu,
        key: String,
        seq: String,
    },
    Color {
        object: String,
        fg: String,
        bg: String,
        pattern: Option<String>,
    },
    Ignore(Vec<String>),
    Unignore(Vec<String>),
    /// mutt's `alternates` / `unalternates`: regexes for my other
    /// addresses (`*` un-does the lot).
    Alternates(Vec<String>),
    Unalternates(Vec<String>),
    /// mutt's `my_hdr`: one "Name: value" line for every draft.
    MyHdr(String),
    /// `unmy_hdr NAME...`, or `*` for all of them.
    UnMyHdr(Vec<String>),
    Alias {
        nick: String,
        expansion: String,
    },
    /// Keys fed to the input queue, as a macro sequence.
    Push(String),
    /// A function name run directly.
    Exec(String),
}

/// Parse one `:` line. An empty line or a comment yields nothing; an
/// unknown or malformed command is an error string ready for the
/// status line.
pub fn parse(line: &str) -> Result<Vec<Command>, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(Vec::new());
    }
    let tokens = tokenize(trimmed);
    let Some(cmd) = tokens.first() else {
        return Ok(Vec::new());
    };
    let args = &tokens[1..];
    match cmd.as_str() {
        "set" => {
            if args.is_empty() {
                return Err("set what?".into());
            }
            Ok(assignments(args).into_iter().map(one_set).collect())
        }
        "unset" | "reset" => want(args, "unset what?").map(|names| {
            names
                .iter()
                .map(|n| Command::Unset(n.to_string()))
                .collect()
        }),
        "toggle" => want(args, "toggle what?").map(|names| {
            names
                .iter()
                .map(|n| Command::Toggle(n.to_string()))
                .collect()
        }),
        "bind" | "macro" => {
            let (Some(menus), Some(key)) = (args.first(), args.get(1)) else {
                return Err(format!("{cmd} MENU KEY ..."));
            };
            let Some(rest) = args.get(2) else {
                return Err(if cmd == "bind" {
                    "bind MENU KEY FUNCTION".into()
                } else {
                    "macro MENU KEY SEQUENCE".into()
                });
            };
            let mut out = Vec::new();
            for name in menus.split(',') {
                let Some(menu) = Menu::parse(name) else {
                    return Err(format!("unknown menu {name:?}"));
                };
                out.push(if cmd == "bind" {
                    Command::Bind {
                        menu,
                        key: key.clone(),
                        function: rest.clone(),
                    }
                } else {
                    Command::Macro {
                        menu,
                        key: key.clone(),
                        seq: rest.clone(),
                    }
                });
            }
            Ok(out)
        }
        "color" => {
            let (Some(object), Some(fg), Some(bg)) = (args.first(), args.get(1), args.get(2))
            else {
                return Err("color OBJECT FG BG [PATTERN]".into());
            };
            Ok(vec![Command::Color {
                object: object.clone(),
                fg: fg.clone(),
                bg: bg.clone(),
                // The pattern is the rest of the line: quoting it is
                // mutt's habit, but an unquoted one still reads whole.
                pattern: (args.len() > 3).then(|| args[3..].join(" ")),
            }])
        }
        "ignore" => want(args, "ignore which headers?").map(|h| vec![Command::Ignore(h)]),
        "unignore" => want(args, "unignore which headers?").map(|h| vec![Command::Unignore(h)]),
        "alternates" => want(args, "alternates PATTERN...").map(|p| vec![Command::Alternates(p)]),
        "unalternates" => {
            want(args, "unalternates PATTERN...").map(|p| vec![Command::Unalternates(p)])
        }
        "my_hdr" => {
            // The value carries colons and spaces, so it comes off the
            // raw line rather than from the tokens.
            let rest = trimmed["my_hdr".len()..].trim().trim_matches('"');
            if !rest.contains(':') || rest.starts_with(':') {
                return Err("my_hdr \"Name: value\"".into());
            }
            Ok(vec![Command::MyHdr(rest.to_string())])
        }
        "unmy_hdr" => want(args, "unmy_hdr NAME...").map(|n| vec![Command::UnMyHdr(n)]),
        "alias" => {
            let (Some(nick), rest) = (args.first(), &args[1.min(args.len())..]) else {
                return Err("alias NICK ADDRESS".into());
            };
            if rest.is_empty() {
                return Err("alias NICK ADDRESS".into());
            }
            Ok(vec![Command::Alias {
                nick: nick.clone(),
                expansion: rest.join(" "),
            }])
        }
        "push" => {
            if args.is_empty() {
                return Err("push what?".into());
            }
            Ok(vec![Command::Push(args.join(" "))])
        }
        "exec" => {
            let Some(function) = args.first() else {
                return Err("exec what?".into());
            };
            Ok(vec![Command::Exec(function.clone())])
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

fn want(args: &[String], msg: &str) -> Result<Vec<String>, String> {
    if args.is_empty() {
        Err(msg.to_string())
    } else {
        Ok(args.to_vec())
    }
}

/// One `set` assignment, decoding mutt's `no`/`inv` prefixes and the
/// trailing `?` that [`assignments`] leaves on the name.
fn one_set((name, value): (String, String)) -> Command {
    if let Some(base) = name.strip_suffix('?') {
        return Command::Query(base.to_string());
    }
    if let Some(base) = name.strip_prefix("inv") {
        return Command::Toggle(base.to_string());
    }
    if value == "no" {
        return Command::Unset(name);
    }
    Command::Set { name, value }
}

/// A settable option as a mutable handle into the live config.
enum Slot<'a> {
    Text(&'a mut Option<String>),
    Flag(&'a mut bool),
    FlagOpt(&'a mut Option<bool>),
    Num16(&'a mut u16),
    NumUsize(&'a mut usize),
    NumU64Opt(&'a mut Option<u64>),
    NumI64Opt(&'a mut Option<i64>),
}

/// mutt's option name to the config field it drives. Options whose
/// value needs decoding first (`from`, `mime_forward`) are handled by
/// the caller, and anything not listed here is not runtime-settable.
fn slot<'a>(cfg: &'a mut Config, name: &str) -> Option<Slot<'a>> {
    use Slot::*;
    Some(match name {
        "index_format" => Text(&mut cfg.index.format),
        "date_format" => Text(&mut cfg.index.date_format),
        "sort" => Text(&mut cfg.index.sort),
        "sort_aux" => Text(&mut cfg.index.sort_aux),
        "pager_format" => Text(&mut cfg.pager.format),
        "quote_regexp" => Text(&mut cfg.pager.quote_regexp),
        "status_format" => Text(&mut cfg.ui.status_format),
        "theme" => Text(&mut cfg.ui.theme),
        "sendmail" => Text(&mut cfg.mail.sendmail),
        "editor" | "visual" => Text(&mut cfg.mail.editor),
        "print_command" => Text(&mut cfg.mail.print),
        "query_command" => Text(&mut cfg.mail.query_command),
        "trash" => Text(&mut cfg.mail.trash),
        "record" => Text(&mut cfg.mail.sent),
        "postponed" => Text(&mut cfg.mail.postponed),
        "new_mail_command" => Text(&mut cfg.mail.new_mail_command),
        "forward" => Text(&mut cfg.mail.forward),
        "realname" => Text(&mut cfg.identity.name),
        "pgp_sign_as" | "pgp_default_key" => Text(&mut cfg.pgp.sign_key),
        "fast_reply" => Flag(&mut cfg.mail.fast_reply),
        "autoedit" => Flag(&mut cfg.mail.autoedit),
        "beep" => Flag(&mut cfg.ui.beep),
        "tilde" => Flag(&mut cfg.pager.tilde),
        "reverse_name" => Flag(&mut cfg.identity.reverse_name),
        "metoo" => Flag(&mut cfg.mail.metoo),
        "sidebar_visible" => Flag(&mut cfg.sidebar.visible),
        "crypt_autosign" | "pgp_autosign" => Flag(&mut cfg.pgp.sign_by_default),
        "crypt_autoencrypt" | "pgp_autoencrypt" => Flag(&mut cfg.pgp.encrypt_by_default),
        "edit_headers" => FlagOpt(&mut cfg.mail.edit_headers),
        "copy" => FlagOpt(&mut cfg.mail.copy),
        "notmuch" => FlagOpt(&mut cfg.mail.notmuch),
        "pager_index_lines" => Num16(&mut cfg.pager.index_lines),
        "sidebar_width" => Num16(&mut cfg.sidebar.width),
        "pager_context" => NumUsize(&mut cfg.pager.context),
        "mail_check" => NumU64Opt(&mut cfg.mail.poll_seconds),
        "wrap" => NumI64Opt(&mut cfg.pager.wrap),
        _ => return None,
    })
}

/// Sort orders the index understands, shared by `set sort` and the
/// config loader's own vocabulary.
const SORTS: [&str; 5] = ["date", "from", "subject", "size", "threads"];

/// Apply the [`Config`] half of a command. `Ok(Some(text))` is a
/// message for the status line (a query's answer); `Ok(None)` is a
/// silent success, mutt-style. Commands the TUI owns (bind, macro,
/// alias, push, exec) return `Ok(None)` without touching the config,
/// so the caller must handle those itself.
pub fn apply(cfg: &mut Config, cmd: &Command) -> Result<Option<String>, String> {
    match cmd {
        Command::Set { name, value } => set(cfg, name, value).map(|()| None),
        Command::Unset(name) => unset(cfg, name).map(|()| None),
        Command::Toggle(name) => toggle(cfg, name).map(|()| None),
        Command::Query(name) => query(cfg, name).map(Some),
        Command::Color {
            object,
            fg,
            bg,
            pattern,
        } => color(cfg, object, fg, bg, pattern.as_deref()).map(|()| None),
        Command::Ignore(headers) => {
            edit_header_list(cfg, headers, true);
            Ok(None)
        }
        Command::Unignore(headers) => {
            edit_header_list(cfg, headers, false);
            Ok(None)
        }
        Command::Alternates(patterns) => {
            for p in patterns {
                if !cfg.mail.alternates.contains(p) {
                    cfg.mail.alternates.push(p.clone());
                }
            }
            Ok(None)
        }
        Command::Unalternates(patterns) => {
            for p in patterns {
                if p == "*" {
                    cfg.mail.alternates.clear();
                } else {
                    cfg.mail.alternates.retain(|a| a != p);
                }
            }
            Ok(None)
        }
        Command::MyHdr(entry) => {
            // One my_hdr per header name, like mutt: a second line for
            // the same header replaces the first.
            if let Some((name, _)) = entry.split_once(':') {
                let name = name.trim().to_string();
                cfg.mail.my_hdr.retain(|h| !header_named(h, &name));
            }
            cfg.mail.my_hdr.push(entry.clone());
            Ok(None)
        }
        Command::UnMyHdr(names) => {
            for name in names {
                if name == "*" {
                    cfg.mail.my_hdr.clear();
                } else {
                    let name = name.trim_end_matches(':');
                    cfg.mail.my_hdr.retain(|h| !header_named(h, name));
                }
            }
            Ok(None)
        }
        Command::Bind { .. }
        | Command::Macro { .. }
        | Command::Alias { .. }
        | Command::Push(_)
        | Command::Exec(_) => Ok(None),
    }
}

/// True when a stored `my_hdr` line carries this header name.
fn header_named(entry: &str, name: &str) -> bool {
    entry
        .split_once(':')
        .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(name))
}

fn unknown(name: &str) -> String {
    format!("unknown or read-only option {name:?}")
}

fn set(cfg: &mut Config, name: &str, value: &str) -> Result<(), String> {
    // mutt's from carries an optional display name; mime_forward is a
    // boolean spelling of rmut's forward mode.
    match name {
        "from" => {
            let (display, addr) = split_from(value);
            if let Some(display) = display {
                cfg.identity.name = Some(display);
            }
            cfg.identity.email = Some(addr);
            return Ok(());
        }
        "mime_forward" => {
            cfg.mail.forward = Some(if is_yes(value) { "attach" } else { "inline" }.into());
            return Ok(());
        }
        _ => {}
    }
    if name == "sort" || name == "sort_aux" {
        let base = value.strip_prefix("reverse-").unwrap_or(value);
        let known = if name == "sort" {
            SORTS.contains(&base)
        } else {
            base == "last-date-sent" || base == "date"
        };
        if !known {
            return Err(format!("no such sort order {value:?}"));
        }
    }
    let num = |v: &str| -> Result<i64, String> {
        v.parse::<i64>()
            .map_err(|_| format!("{name} wants a number, got {v:?}"))
    };
    match slot(cfg, name).ok_or_else(|| unknown(name))? {
        Slot::Text(field) => *field = Some(value.to_string()),
        Slot::Flag(field) => *field = is_yes(value),
        Slot::FlagOpt(field) => *field = Some(is_yes(value)),
        Slot::Num16(field) => *field = u16::try_from(num(value)?.max(0)).unwrap_or(u16::MAX),
        Slot::NumUsize(field) => *field = usize::try_from(num(value)?.max(0)).unwrap_or(0),
        Slot::NumU64Opt(field) => *field = Some(num(value)?.max(0) as u64),
        Slot::NumI64Opt(field) => *field = Some(num(value)?),
    }
    Ok(())
}

/// `unset`: booleans go false, everything else goes back to rmut's
/// built-in default (an unset field), which is what a muttrc-less
/// session would have used.
fn unset(cfg: &mut Config, name: &str) -> Result<(), String> {
    if name == "from" {
        cfg.identity.email = None;
        return Ok(());
    }
    match slot(cfg, name).ok_or_else(|| unknown(name))? {
        Slot::Text(field) => *field = None,
        Slot::Flag(field) => *field = false,
        Slot::FlagOpt(field) => *field = Some(false),
        Slot::Num16(field) => *field = 0,
        Slot::NumUsize(field) => *field = 0,
        Slot::NumU64Opt(field) => *field = None,
        Slot::NumI64Opt(field) => *field = None,
    }
    Ok(())
}

fn toggle(cfg: &mut Config, name: &str) -> Result<(), String> {
    match slot(cfg, name).ok_or_else(|| unknown(name))? {
        Slot::Flag(field) => *field = !*field,
        Slot::FlagOpt(field) => *field = Some(!field.unwrap_or(false)),
        _ => return Err(format!("{name} is not a boolean")),
    }
    Ok(())
}

fn query(cfg: &mut Config, name: &str) -> Result<String, String> {
    if name == "from" {
        let value = cfg.identity.email.clone().unwrap_or_default();
        return Ok(format!("from=\"{value}\""));
    }
    let text = match slot(cfg, name).ok_or_else(|| unknown(name))? {
        Slot::Text(field) => format!("{name}=\"{}\"", field.clone().unwrap_or_default()),
        Slot::Flag(field) => flag_text(name, *field),
        Slot::FlagOpt(field) => flag_text(name, field.unwrap_or(false)),
        Slot::Num16(field) => format!("{name}={field}"),
        Slot::NumUsize(field) => format!("{name}={field}"),
        Slot::NumU64Opt(field) => match field {
            Some(n) => format!("{name}={n}"),
            None => format!("{name} is unset"),
        },
        Slot::NumI64Opt(field) => match field {
            Some(n) => format!("{name}={n}"),
            None => format!("{name} is unset"),
        },
    };
    Ok(text)
}

fn flag_text(name: &str, on: bool) -> String {
    if on {
        name.to_string()
    } else {
        format!("no{name}")
    }
}

/// `ignore`/`unignore`: rmut keeps both lists, so each command adds to
/// its own and drops the name from the other (mutt's unignore is the
/// same undo, expressed against one list).
fn edit_header_list(cfg: &mut Config, headers: &[String], ignoring: bool) {
    let clean: Vec<String> = headers
        .iter()
        .map(|h| h.trim_end_matches(':').to_lowercase())
        .collect();
    let (add, remove) = if ignoring {
        (&mut cfg.pager.ignore, &mut cfg.pager.unignore)
    } else {
        (&mut cfg.pager.unignore, &mut cfg.pager.ignore)
    };
    let list = add.get_or_insert_with(Vec::new);
    for name in &clean {
        if !list.contains(name) {
            list.push(name.clone());
        }
    }
    if let Some(list) = remove {
        list.retain(|n| !clean.contains(n));
    }
}

/// `color OBJECT FG BG [PATTERN]`, with the importer's slot mapping:
/// named slots land in `[colors]`, index patterns become
/// `[[color_index]]` rules, and body regexes `[[color_body]]`.
fn color(
    cfg: &mut Config,
    object: &str,
    fg: &str,
    bg: &str,
    pattern: Option<&str>,
) -> Result<(), String> {
    let fg = convert_color(fg);
    let bg = convert_color(bg);
    // A foreground saying nothing (black/white/default over a colored
    // background) means the background is the visible color.
    let vivid = if matches!(fg.as_str(), "black" | "white" | "default") && bg != "default" {
        bg.clone()
    } else {
        fg.clone()
    };
    if let Some(n) = object.strip_prefix("quoted")
        && let Ok(depth) = if n.is_empty() { Ok(0usize) } else { n.parse() }
    {
        let key = if depth == 0 {
            "quoted".to_string()
        } else {
            format!("quoted{depth}")
        };
        cfg.colors.insert(key, vivid);
        return Ok(());
    }
    let mut named = |key: &str, value: String| {
        cfg.colors.insert(key.to_string(), value);
    };
    match (object, pattern) {
        ("status", _) => {
            named("status_fg", fg);
            named("status_bg", bg);
        }
        ("search", _) => {
            if fg != "default" {
                named("search_fg", fg);
            }
            if bg != "default" {
                named("search_bg", bg);
            }
        }
        ("header" | "hdrdefault", _) => named("header", fg),
        ("error", _) => named("error", vivid),
        ("index", Some("~D")) => named("deleted", vivid),
        ("index", Some("~F")) => named("flagged", vivid),
        ("index", Some("~T")) => named("tagged", vivid),
        ("index", Some(pattern)) => {
            crate::pattern::parse(pattern).map_err(|err| format!("bad pattern: {err}"))?;
            cfg.color_index.push(rule(pattern, fg, bg));
        }
        ("body", Some(pattern)) => {
            regex_lite::Regex::new(pattern).map_err(|err| format!("bad regex: {err}"))?;
            cfg.color_body.push(rule(pattern, fg, bg));
        }
        ("index" | "body", None) => return Err(format!("color {object} needs a pattern")),
        _ => return Err(format!("no rmut color slot for {object:?}")),
    }
    Ok(())
}

fn rule(pattern: &str, fg: String, bg: String) -> ColorRule {
    ColorRule {
        pattern: pattern.to_string(),
        fg: (fg != "default").then_some(fg),
        bg: (bg != "default").then_some(bg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Command {
        let mut cmds = parse(line).expect("parses");
        assert_eq!(cmds.len(), 1, "{line:?} yielded {cmds:?}");
        cmds.pop().unwrap()
    }

    #[test]
    fn alternates_and_my_hdr_at_the_prompt() {
        let mut cfg = Config::default();
        for line in [
            "alternates jane@old\\.example\\.com typo@x",
            "unalternates typo@x",
            "my_hdr Organization: Acme",
            "my_hdr X-Mailer: rmut",
            "my_hdr Organization: Acme Ltd",
            "unmy_hdr X-Mailer",
            "set metoo",
        ] {
            for cmd in parse(line).expect(line) {
                apply(&mut cfg, &cmd).expect(line);
            }
        }
        assert_eq!(cfg.mail.alternates, ["jane@old\\.example\\.com"]);
        assert_eq!(cfg.mail.my_hdr, ["Organization: Acme Ltd"]);
        assert!(cfg.mail.metoo);
        for cmd in parse("unmy_hdr *").unwrap() {
            apply(&mut cfg, &cmd).unwrap();
        }
        assert!(cfg.mail.my_hdr.is_empty());
        assert!(parse("my_hdr Organization").is_err());
        assert!(parse("alternates").is_err());
    }

    #[test]
    fn set_forms() {
        assert_eq!(
            one("set index_format=\"%s %f\""),
            Command::Set {
                name: "index_format".into(),
                value: "%s %f".into()
            }
        );
        assert_eq!(one("set nobeep"), Command::Unset("beep".into()));
        assert_eq!(
            one("set beep"),
            Command::Set {
                name: "beep".into(),
                value: "yes".into()
            }
        );
        assert_eq!(one("set invbeep"), Command::Toggle("beep".into()));
        assert_eq!(one("set beep?"), Command::Query("beep".into()));
        assert_eq!(one("toggle tilde"), Command::Toggle("tilde".into()));
        assert_eq!(one("unset tilde"), Command::Unset("tilde".into()));
        assert_eq!(parse("set a=1 b=2").unwrap().len(), 2);
        assert_eq!(parse("").unwrap(), vec![]);
        assert_eq!(parse("# comment").unwrap(), vec![]);
    }

    #[test]
    fn unknown_command_and_option_report() {
        assert!(parse("frobnicate x").is_err());
        let mut cfg = Config::default();
        let err = apply(&mut cfg, &one("set nosuchthing=1")).unwrap_err();
        assert!(err.contains("nosuchthing"), "{err}");
    }

    #[test]
    fn set_applies_to_config() {
        let mut cfg = Config::default();
        apply(&mut cfg, &one("set pager_index_lines=5")).unwrap();
        assert_eq!(cfg.pager.index_lines, 5);
        apply(&mut cfg, &one("set wrap=-8")).unwrap();
        assert_eq!(cfg.pager.wrap, Some(-8));
        apply(&mut cfg, &one("set tilde")).unwrap();
        assert!(cfg.pager.tilde);
        apply(&mut cfg, &one("toggle tilde")).unwrap();
        assert!(!cfg.pager.tilde);
        apply(&mut cfg, &one("set from=\"Jane Doe <jane@x>\"")).unwrap();
        assert_eq!(cfg.identity.email.as_deref(), Some("jane@x"));
        assert_eq!(cfg.identity.name.as_deref(), Some("Jane Doe"));
        // Numbers and sort orders are checked, not silently accepted.
        assert!(apply(&mut cfg, &one("set pager_index_lines=x")).is_err());
        assert!(apply(&mut cfg, &one("set sort=nonsense")).is_err());
        apply(&mut cfg, &one("set sort=reverse-date")).unwrap();
        assert_eq!(cfg.index.sort.as_deref(), Some("reverse-date"));
    }

    #[test]
    fn query_reports_current_value() {
        let mut cfg = Config::default();
        cfg.index.format = Some("%s".into());
        let out = apply(&mut cfg, &one("set index_format?")).unwrap();
        assert_eq!(out.as_deref(), Some("index_format=\"%s\""));
        let out = apply(&mut cfg, &one("set beep?")).unwrap();
        assert_eq!(out.as_deref(), Some("beep"));
        apply(&mut cfg, &one("unset beep")).unwrap();
        let out = apply(&mut cfg, &one("set beep?")).unwrap();
        assert_eq!(out.as_deref(), Some("nobeep"));
    }

    #[test]
    fn ignore_lists_move_names_between_them() {
        let mut cfg = Config::default();
        apply(&mut cfg, &one("ignore x-spam: received")).unwrap();
        assert_eq!(
            cfg.pager.ignore.as_deref(),
            Some(["x-spam".to_string(), "received".to_string()].as_slice())
        );
        apply(&mut cfg, &one("unignore received")).unwrap();
        assert_eq!(
            cfg.pager.ignore.as_deref(),
            Some(["x-spam".to_string()].as_slice())
        );
        assert_eq!(
            cfg.pager.unignore.as_deref(),
            Some(["received".to_string()].as_slice())
        );
    }

    #[test]
    fn colors_land_in_their_slots() {
        let mut cfg = Config::default();
        apply(&mut cfg, &one("color status brightwhite blue")).unwrap();
        assert_eq!(
            cfg.colors.get("status_fg").map(String::as_str),
            Some("lightwhite")
        );
        assert_eq!(
            cfg.colors.get("status_bg").map(String::as_str),
            Some("blue")
        );
        apply(&mut cfg, &one("color quoted2 green default")).unwrap();
        assert_eq!(cfg.colors.get("quoted2").map(String::as_str), Some("green"));
        apply(&mut cfg, &one("color index red default ~F")).unwrap();
        assert_eq!(cfg.colors.get("flagged").map(String::as_str), Some("red"));
        apply(&mut cfg, &one("color index red default ~f jane")).unwrap();
        assert_eq!(cfg.color_index.len(), 1);
        apply(&mut cfg, &one("color body yellow default TODO")).unwrap();
        assert_eq!(cfg.color_body.len(), 1);
        // A pattern or regex that does not compile is refused.
        assert!(apply(&mut cfg, &one("color index red default ~q")).is_err());
        assert!(apply(&mut cfg, &one("color body red default [")).is_err());
        assert!(apply(&mut cfg, &one("color nosuchslot red default")).is_err());
    }

    #[test]
    fn bind_macro_and_push_parse_but_stay_for_the_caller() {
        assert_eq!(
            one("bind index \\Cd delete-message"),
            Command::Bind {
                menu: Menu::Index,
                key: "\\Cd".into(),
                function: "delete-message".into()
            }
        );
        assert_eq!(parse("bind index,pager d delete-message").unwrap().len(), 2);
        assert!(parse("bind nosuchmenu d delete").is_err());
        assert_eq!(
            one("macro index L \"l~f jane<enter>\""),
            Command::Macro {
                menu: Menu::Index,
                key: "L".into(),
                seq: "l~f jane<enter>".into()
            }
        );
        assert_eq!(one("push \"<enter>\""), Command::Push("<enter>".into()));
        assert_eq!(one("exec sync"), Command::Exec("sync".into()));
        assert_eq!(
            one("alias jane Jane Doe <jane@x>"),
            Command::Alias {
                nick: "jane".into(),
                expansion: "Jane Doe <jane@x>".into()
            }
        );
        // The config half leaves them alone.
        let mut cfg = Config::default();
        assert_eq!(apply(&mut cfg, &one("exec sync")).unwrap(), None);
    }
}
