# rmut

A mutt replacement in Rust: terminal mail client with mutt keybindings,
built on ratatui. See [docs/PLAN.md](docs/PLAN.md) for the roadmap.

## Status

**1.4** — everything from the 1.0 roadmap plus R5–R7: mutt-style index
with delete/flag/read toggles and real maildir sync, sort orders,
limit/search patterns, mailbox switching, wrapped pager, attachment
menu, **threading** (References/In-Reply-To, JWZ-style, `o t`,
Alt+v/Alt+V to fold), **compose/reply/forward** via `$EDITOR` +
sendmail(1) with postpone/recall, Fcc, and aliases, **configuration**
(TOML: identity, mailboxes, sendmail/editor, index format string,
themes/colors, key remapping), **new-mail detection**, a `?` help
screen generated from the active keymap, and now **IMAP and SMTP
accounts**: open `imap:account/FOLDER` mailboxes over TLS, with local
caching, `$` sync mapped to the server, and sending via SMTP
submission with an IMAP Fcc, and **PGP via gpg(1)**: decrypt/verify on
view, sign/encrypt from the send prompt. A pty-driven e2e suite
(including fake IMAP/SMTP servers and a stub gpg) lives in
`tests/e2e/`.

## Install & run

```sh
cargo install --path crates/rmut-tui   # installs the `rmut` binary
# or during development:
cargo run -p rmut-tui -- ~/Maildir     # or: just run ~/Maildir
```

```
usage: rmut [MAILDIR | imap:ACCOUNT[/FOLDER]]   (-V version, -h help)
       rmut --import-muttrc [MUTTRC]
```

Without an argument, rmut opens the first configured mailbox, `$MAIL`,
or `~/Maildir`.

## Keys

Index: `j`/`k` move, `Enter` view, `=`/`*` first/last, PgUp/PgDn or
Ctrl+B/Ctrl+F page, `d`/`u` delete/undelete, `F` flag, `N` toggle
read, `$` sync changes to disk, `o` sort (`d`ate `f`rom `s`ubject
si`z`e `t`hreads, uppercase reverses), Alt+v/Alt+V fold thread/all
(with thread sort), `l` limit, `/` search + `n` next, `c` open mailbox
by path, `y` folder browser, `v` attachments, `m` compose, `r` reply,
`g` group reply, `f` forward, `p` print (pipes the message to
`mail.print`, default `lpr`), `q` quit (asks when changes are
pending), `x` abort without saving.

Pager: `j`/`k` scroll, `Space`/`-` page down/up, `J`/`K` next/previous
message, `d` delete and advance, `h` toggle full headers,
`v` attachments, `m`/`r`/`g`/`f` compose/reply/forward, `p` print,
`q`/`i` back.

Attachments: `Enter` view a text part, `s` save part to a file.

Patterns (limit/search): `~f x` from, `~s x` subject, `~b x` body,
`~N` new, `~F` flagged, `~D` deleted, `~U` unread; a bare word matches
subject or from; several terms AND together.

## IMAP

`rmut imap:work` (or `imap:work/Archive`) opens an account folder;
`c` and the folder browser `y` take the same specs, and `y` lists the
account's folders via LIST. Messages are mirrored into a cache maildir
under `~/.cache/rmut/imap/` — headers up front, full bodies fetched on
first view — so the index is fast and old mail reopens offline. `$`
pushes your changes to the server (flags via UID STORE, deletes via
EXPUNGE) and new mail is picked up by polling (`poll_seconds`). The
password comes from `password_command` (e.g. `pass show mail/work`),
run once per session — or from a stored `password`, if you accept a
secret sitting in the config file (keep it chmod 600).

## PGP

PGP messages are handled on view by shelling out to gpg(1):
PGP/MIME (RFC 3156) and inline/clearsigned messages are decrypted
and/or verified, with a `[-- PGP: ... --]` verdict line at the top of
the pager (good/BAD/unverified signature). Outgoing mail is treated
per message: at the send prompt, `s` opens the security menu —
(e)ncrypt, (s)ign, (b)oth, (c)lear — and the chosen state shows in the
prompt. Signing uses `sign_key` (or gpg's default key); encryption
looks keys up by recipient address and always encrypts to the sender
too, so the Fcc copy stays readable. Passphrases are gpg-agent's
business — rmut never sees them.

## Sending mail

Drafts open in `$VISUAL`/`$EDITOR` (default `vi`). If an account with
`smtp_host` applies (the open mailbox's account, or the first one
configured), the message goes out via SMTP submission — STARTTLS on
587, implicit TLS on 465, AUTH PLAIN/LOGIN, Bcc stripped from the wire
copy — and the Fcc lands in the account's `sent_folder` by IMAP
APPEND. Otherwise it is handed to `sendmail -t -oi`; setting
`$RMUT_SENDMAIL` or `mail.sendmail` forces the sendmail path. `From:`
defaults to `$EMAIL` or `user@hostname` unless the draft sets one. At
the send prompt, `p` postpones the draft into a nearby Drafts maildir
(or `.rmut-postponed`); the next `m` offers to recall it. Sent mail is
copied to a nearby Sent maildir when one exists (local mailboxes).
Aliases are read from `$RMUT_ALIASES` or `~/.config/rmut/aliases`, one
mutt-style `alias nick address...` per line.

## Configuration

`$RMUT_CONFIG` or `~/.config/rmut/config.toml`:

```toml
[identity]
name = "Jane Doe"            # From: Jane Doe <jane@example.com>
email = "jane@example.com"

[mail]
mailboxes = ["~/Maildir"]    # default mailbox + folder browser entries
sent = "~/Maildir/.Sent"     # Fcc target (else a nearby Sent is used)
postponed = "~/Maildir/.Drafts"
sendmail = "/usr/sbin/sendmail"
editor = "vim"
poll_seconds = 5             # new-mail check interval
print = "lpr"                # `p` pipes the message here

[index]
format = "%4C %Z %-6d %-20.20F %5c %s"   # %C num %Z flags %d date
                                         # %F from %c size %s subject

[ui]
theme = "default"            # or "mono"

[colors]                     # status_fg status_bg deleted flagged header
deleted = "red"

[keys.index]                 # remap: action = "key" (see ? for actions)
sync = "w"
[keys.pager]

[[accounts]]                 # remote account: open with `rmut imap:work`
name = "work"
user = "jane@example.com"
password_command = "pass show mail/work"   # first stdout line
# password = "..."                         # alternative; keep the file chmod 600
imap_host = "imap.example.com"             # imap_port = 993 (implicit TLS; 143 = STARTTLS)
smtp_host = "smtp.example.com"             # smtp_port = 587 (STARTTLS; 465 = implicit TLS)
sent_folder = "Sent"                       # Fcc target via IMAP APPEND

[pgp]                        # optional; gpg from $PATH by default
command = "gpg"
sign_key = "jane@example.com"  # --local-user; gpg's default key if unset
sign_by_default = false        # preselect security for new drafts
encrypt_by_default = false
```

Key syntax: a character, `ctrl+x`, `alt+x`, or enter/esc/space/tab/
backspace/up/down/pgup/pgdn/home/end. `?` lists all actions with their
current keys.

### Coming from mutt

```sh
rmut --import-muttrc > ~/.config/rmut/config.toml   # reads ~/.muttrc
```

translates a muttrc (identity, folder/mailboxes, record/postponed,
sendmail/editor/print_command, binds, status/header colors, PGP
defaults, IMAP/SMTP URLs into an `[[accounts]]` skeleton) into rmut
TOML on stdout for review — it never writes any file itself.
Directives with no rmut equivalent are kept as `# not imported:`
comments, and ones that match rmut's built-in behavior (ssl_starttls,
UTF-8 charset, pgp_auto_decode, ...) are acknowledged under
`# satisfied by rmut's defaults`; `imap_pass`/`smtp_pass` become the
account's stored `password`. Alias files need no
translation: rmut reads mutt-format aliases, so point `$RMUT_ALIASES`
at your existing file or copy it to `~/.config/rmut/aliases`.

## Development

```sh
just test    # cargo test --workspace
just lint    # clippy -D warnings + fmt --check
just e2e     # pty-driven end-to-end tests
just check   # test + lint + e2e
```
