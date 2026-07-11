# rmut

A mutt replacement in Rust: terminal mail client with mutt keybindings,
built on ratatui. See [docs/PLAN.md](docs/PLAN.md) for the roadmap.

## Status

**1.17** — everything from the 1.0 roadmap plus R5–R17 and hardening
(R19): mutt-style index
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
submission with an IMAP Fcc, **PGP via gpg(1)**: decrypt/verify on
view, sign/encrypt from the send prompt, **attachments and message
commands** (R8): `Attach:` pseudo-headers in the draft, copy/pipe/
bounce/resend on `C`/`|`/`b`/`e`, **identities** (R9): per-account
From, `[[identities]]` folder/recipient rules (the minimal folder-/
send-hook), and mutt's reverse_name, **patterns v2** (R10):
`!`/`|`/`()`, regexes, `~t`/`~c`/`~C`/`~e`/`~p`, `~d` date ranges, and
**new-mail awareness** (R11): counts in the folder browser, watching
the other configured mailboxes, IMAP IDLE, and **address completion**
(R12): Tab at the To prompt completes aliases and query_command
results, **macros** (R13): a key replays a sequence, prompts included,
**OAuth2** (R14): XOAUTH2/OAUTHBEARER for IMAP and SMTP via a
`token_command`, **mbox** (R15): system spools with sync write-back,
**index polish** (R16): `%l` line counts and list-aware `%L`, a
**sidebar** (R17): the configured mailboxes with new-mail counts, and
**hardening** (R19): transparent IMAP reconnect, incremental refresh,
a header cache for large maildirs, mbox rewrite backups. A pty-driven
e2e suite (including fake IMAP/SMTP servers and a stub gpg) lives in
`tests/e2e/`.

## Install & run

```sh
cargo install --path crates/rmut-tui   # installs the `rmut` binary
# or during development:
cargo run -p rmut-tui -- ~/Maildir     # or: just run ~/Maildir
```

```
usage: rmut [MAILDIR | MBOX | imap:ACCOUNT[/FOLDER]]   (-V version, -h help)
       rmut --import-muttrc [MUTTRC]
```

Without an argument, rmut opens the first configured mailbox, `$MAIL`,
or `~/Maildir`.

## Keys

Index: `j`/`k` move, `Enter` view, `=`/`*` first/last, PgUp/PgDn or
Ctrl+B/Ctrl+F page, `d`/`u` delete/undelete, `F` flag, `N` toggle
read, `t` tag + `;` apply the next d/u/F/N to all tagged, `s` save
(copy to a mailbox + mark deleted), `$` sync changes to disk (asks
before purging deleted messages, like mutt), `o` sort
(`d`ate `f`rom `s`ubject si`z`e `t`hreads, uppercase reverses),
Alt+v/Alt+V fold thread/all (with thread sort), `l` limit, `/` search
+ `n` next, `c` open mailbox by path, `y` folder browser (with
new/unseen counts; folders holding new mail show bold), `G` check
for new mail now, `B` toggle the sidebar (Ctrl+N/Ctrl+P move its
highlight, Ctrl+O opens the highlighted mailbox), `v` attachments,
`m` compose, `r` reply, `g` group
reply, `f` forward, `C` copy to a mailbox (no delete mark), `|` pipe
the raw message to a shell command, `b` bounce (resend as-is to new
recipients, with a Resent-\* block), `e` edit the message as a new
draft, `p` print (pipes the message to `mail.print`, default `lpr`),
`q` quit (writes changes; asks before purging deletions, like mutt),
`x` abort without saving.

Pager: `j`/`k` scroll, `Space`/`-` page down/up, `J`/`K` next/previous
message, `d` delete and advance, `h` toggle full headers,
`v` attachments, `m`/`r`/`g`/`f` compose/reply/forward, `p` print,
`s` save, `C`/`|`/`b`/`e` copy/pipe/bounce/resend, `q`/`i` back.

Attachments: `Enter` view a text part, `s` save part to a file.

Patterns (limit/search): `~f x` from, `~s x` subject, `~b x` body,
`~t x` to, `~c x` cc, `~C x` to-or-cc, `~e x` sender, `~d spec` date,
`~N` new, `~F` flagged, `~D` deleted, `~U` unread, `~T` tagged,
`~p` addressed to me; a bare word matches subject or from. `x` is a
case-insensitive regex (`"quotes"` keep spaces; an invalid regex falls
back to plain substring). `~d` takes a day or range —
`24/12/2026`, `1/6/2026-30/6/2026`, `24/12-`, `-1/1/2027` — or an
offset: `<1w` (within), `>2d` (older than), `=3d` (that day); units
y m w d H M. Adjacent terms AND, `!` negates, `|` ORs, `()` groups:
`!~D (~f jane | ~t jane) ~d <1m`.

## IMAP

`rmut imap:work` (or `imap:work/Archive`) opens an account folder;
`c` and the folder browser `y` take the same specs, and `y` lists the
account's folders via LIST. Messages are mirrored into a cache maildir
under `~/.cache/rmut/imap/` — headers up front, full bodies fetched on
first view — so the index is fast and old mail reopens offline. `$`
pushes your changes to the server (flags via UID STORE, deletes via
EXPUNGE). New mail is announced by **IDLE** (RFC 2177, on a second
connection) and shows up within a second; when the server doesn't
support IDLE, the NOOP poll (`poll_seconds`) picks it up as before.
The folder browser asks the server for UNSEEN counts (STATUS). A
connection dropped by laptop sleep or a server timeout is transparently
reopened and the operation retried once; polls fetch only new arrivals
unless the server reported flag changes or expunges. The
password comes from `password_command` (e.g. `pass show mail/work`),
run once per session — or from a stored `password`, if you accept a
secret sitting in the config file (keep it chmod 600).

For Gmail/O365-style **OAuth2**, set `auth = "xoauth2"` (or
`"oauthbearer"`, RFC 7628) and a `token_command` whose first output
line is a fresh access token — acquiring and refreshing tokens is the
external tool's business (oauth2ms, mutt_oauth2.py, ...). The command
runs for every connection, since tokens expire; both IMAP
(AUTHENTICATE) and SMTP (AUTH) then use the token instead of a
password.

## mbox

`rmut /var/mail/$USER` opens an mbox file: it is mirrored into a cache
maildir (like IMAP folders), so the index, pager, flags, and patterns
all work unchanged, and messages are keyed by content so your flags
survive when the spool grows. `$` sync writes changes back into the
file — deleted messages dropped, `Status:`/`X-Status:` headers
rewritten (`RO`/`AF`), mboxrd `>From` quoting preserved — under an
exclusive flock, with a crash backup kept in the cache until the
rewrite lands, and refuses (rather than clobbers) when the spool
changed since the last look; check for new mail (`G`) and sync again.
New deliveries are picked up by the regular poll.

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
defaults to the identity in effect — `[identity]` overlaid by the open
account's `identity` and any matching `[[identities]]` rules, with
`reverse_name` picking the address a replied-to message came to — or
falls back to `$EMAIL` / `user@hostname`; the draft's own From line
always wins, and rmut prefills it whenever an identity applies. At
the send prompt, `p` postpones the draft into a nearby Drafts maildir
(or `.rmut-postponed`); the next `m` offers to recall it. Sent mail is
copied to a nearby Sent maildir when one exists (local mailboxes).
Aliases are read from `$RMUT_ALIASES` or `~/.config/rmut/aliases`, one
mutt-style `alias nick address...` per line. At the To prompt (compose
and bounce), **Tab** completes the word under the cursor: alias nicks
by prefix, plus hits from `query_command` when one is configured
(mutt's protocol — `%s` is the search word, the first output line is a
message, then `address<TAB>name` lines). Repeated Tab cycles through
multiple matches.

To attach files, add `Attach:` pseudo-headers to the draft in the
editor (mutt's edit_headers style):

```
To: jane@example.com
Subject: the report
Attach: ~/report.pdf the Q2 numbers
Attach: "/tmp/two words.png"

see attached
```

Each one becomes a base64 part of a multipart/mixed message (content
type guessed from the extension, the rest of the line an optional
description); the send prompt shows the attachment count. PGP signing
and encryption wrap the whole multipart, attachments included — this
also works for forwards with `forward = "attach"`.

## Configuration

`$RMUT_CONFIG` or `~/.config/rmut/config.toml`:

```toml
[identity]
name = "Jane Doe"            # From: Jane Doe <jane@example.com>
email = "jane@example.com"
reverse_name = false         # true: a reply's From becomes whichever
                             # of your addresses the mail was sent to

[[identities]]               # conditional identity (folder-/send-hook):
folder = "*work*"            # glob on the open mailbox, and/or
recipient = "*@work.example.com"   # glob on a draft recipient;
name = "Jane Work"           # matching rules overlay [identity] in
email = "jane@work.example.com"    # order, unset fields fall through

[mail]
mailboxes = ["~/Maildir"]    # default mailbox + folder browser entries;
                             # local ones are watched for new mail
                             # ("new mail in ..." in the status line)
sent = "~/Maildir/.Sent"     # Fcc target (else a nearby Sent is used)
postponed = "~/Maildir/.Drafts"
sendmail = "/usr/sbin/sendmail"
editor = "vim"
poll_seconds = 5             # new-mail check interval
print = "lpr"                # `p` pipes the message here
save = "~/Maildir/.Archive"  # default target for `s`
forward = "inline"           # or "attach" (original as message/rfc822)
query_command = "khard email --parsable %s"   # Tab completion lookup

[index]
format = "%4C %Z %-6d %-20.20F %5c %s"   # %C num %Z flags %d date
                                         # %F from (%L: "To <list>" for
                                         # List-Id mail) %c size %l body
                                         # lines %s subject, and
                                         # %?X?then&else? conditionals
sort = "threads"             # initial sort; reverse-date, size, ...
sort_aux = "last-date-sent"  # threads ordered by their newest message
date_format = "%d.%m.%Y"     # strftime for the date column

[pager]
index_lines = 10             # keep a slice of the index above the pager
context = 3                  # overlapping lines when paging

[filters]                    # auto_view: render a part via a command
"text/html" = "w3m -dump -T text/html -O UTF-8"

[ui]
theme = "default"            # or "mono"

[sidebar]                    # left pane: mail.mailboxes with new-mail
visible = false              # counts (B toggles at runtime; bold =
width = 24                   # has new mail, > marks the open one)

[colors]                     # status_fg status_bg deleted flagged
deleted = "red"              # tagged header

[keys.index]                 # remap: action = "key" (see ? for actions)
sync = "w"
[keys.pager]

[macros.index]               # macro: key = "replayed key sequence" —
L = "l~f jane<enter>"        # literals + <enter>/<esc>/<ctrl+x>/...;
[macros.pager]               # it feeds the input queue, so it can
                             # drive prompts; a macro shadows a
                             # binding on the same key (like mutt)

[[accounts]]                 # remote account: open with `rmut imap:work`
name = "work"
user = "jane@example.com"
password_command = "pass show mail/work"   # first stdout line
# password = "..."                         # alternative; keep the file chmod 600
# auth = "xoauth2"                         # or "oauthbearer": OAuth2 with
# token_command = "oauth2ms"               # a fresh access token per connection
imap_host = "imap.example.com"             # imap_port = 993 (implicit TLS; 143 = STARTTLS)
smtp_host = "smtp.example.com"             # smtp_port = 587 (STARTTLS; 465 = implicit TLS)
sent_folder = "Sent"                       # Fcc target via IMAP APPEND
identity = { name = "Jane W", email = "jane@work.example.com" }  # From
                                           # when composing from this account

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
sendmail/editor/print_command/query_command, binds, status/header
colors, PGP
defaults, IMAP/SMTP URLs into an `[[accounts]]` skeleton — with
`auth`/`token_command` when `*_authenticators` names
oauthbearer/xoauth2 — reverse_name,
folder-hooks/send-hooks that only set from/realname into
`[[identities]]` rules, and macros whose sequence is plain keys and
prompt input) into rmut
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
