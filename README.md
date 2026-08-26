# rmut

A mutt replacement in Rust: terminal mail client with mutt keybindings,
built on ratatui. See [docs/PLAN.md](docs/PLAN.md) for the roadmap.

## Status

**1.68**: everything from the 1.0 roadmap plus R5–R17, hardening
(R19), flow niceties (R20), and display customization (R21):
mutt-style index
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
a header cache for large maildirs, mbox rewrite backups, and **flow
niceties** (R20): attach/review at the send prompt, create-alias,
$trash, a postponed-draft picker, and **display customization** (R21):
pattern→color index rules, mutt's status_format, `%M` collapsed
counts, **distribution** (R22): CI, crates.io packages, a man page
(`docs/rmut.1`), and tagged releases with a prebuilt x86_64-linux
binary, and the **mutt-parity round**: mutt's defaults (index format,
edit_headers, $resolve, $mark_old, first-new positioning, pager
markers and $pager_stop, the ask-yes compose questions, e edits the
raw message) plus the full **compose menu** after the editor, and a
real **prompt line editor** (R23): cursor movement and mid-line
editing (ctrl+a/e/u/k/w, arrows, Del) with per-kind history on
Up/Down, and **pager body search** (R24): `/` searches the displayed
text with `n`/`N` stepping through the hits, wrapping around, and
**triage keys** (R25): Tab/Alt+Tab jump to the next/previous
new-or-unread message, and `D`/`U`/`T`/Ctrl+T apply
delete/undelete/tag/untag to every pattern match, and the
**browser-and-odds round** (R26): the folder browser descends into
directories and creates maildirs, the attachment menu pipes and
prints parts, `rmut -R` opens read-only, `Q` queries addresses into
a compose, and status_format gains `%>` right-alignment and `%P`,
and **pager colors and motion** (R27): quoted lines tinted by depth
(quote_regexp), `[[color_body]]` regex rules, highlighted search
hits, Ctrl+D/Ctrl+U half-page, and `T`/`S` toggle/skip quoted text,
and **header weeding and pager polish** (R28): ignore/unignore +
hdr_order shape the brief header view, mutt's pager_format renders
the bottom line, `$wrap` and `$tilde` round out the pager, and
**notmuch search** (R18): `X` opens `notmuch search` hits as a
read-only virtual mailbox, and **compose round 2** (R29):
fast_reply/autoedit skip the prompts, the compose menu edits
attachment descriptions/types and the Fcc, `forward = "ask"` asks
per forward, and `copy = false` skips the sent copy, and
**big mailboxes** (R30): `~b` searches server-side on IMAP, huge
folders open progressively with a background backfill,
new_mail_command fires on arrivals, and stale header caches sweep
themselves.
A pty-driven e2e suite (including fake IMAP/SMTP servers and a
stub gpg) lives in `tests/e2e/`.

## Install & run

```sh
cargo install rmut-tui                 # installs the `rmut` binary
# or from a checkout:
cargo install --path crates/rmut-tui
# or during development:
cargo run -p rmut-tui -- ~/Maildir     # or: just run ~/Maildir
```

Tagged releases on GitHub carry a prebuilt x86_64-linux binary with
the man page; `man docs/rmut.1` previews the manual from a checkout.

```
usage: rmut [MAILDIR | MBOX | imap:ACCOUNT[/FOLDER]]   (-V version, -h help)
       rmut --import-muttrc [-w] [MUTTRC]
```

Without an argument rmut looks where mutt looks: the first configured
mailbox, then `[mail] folder`, `$MAIL` (a maildir or an mbox file),
`~/Maildir`, `~/Mail`, `~/mail`, and finally the `/var/mail/$USER`
spool. A directory of maildirs answers with its `inbox`. When nothing
turns up it says where it looked, and how to make one:

```sh
mkdir -p ~/Mail/inbox/{cur,new,tmp} && rmut ~/Mail/inbox
```

## The screen

Four regions, as in mutt: the help bar at the top, the mailbox or the
message, the status bar, and the message line under it. The message
line is where prompts, notes and errors go, so the status bar always
says which mailbox you are in and what is in it, whatever else is
happening.

## Keys

Index: `j`/`k` move, `Enter` view, `=`/`*` first/last, PgUp/PgDn or
Ctrl+B/Ctrl+F page, `d`/`u` delete/undelete, `F` flag, `N` toggle
read, `t` tag + `;` apply the next mark, save, copy, pipe, print or
bounce to all tagged, `z` undo
the last of those (or cancel a held send), `s` save
(copy to a mailbox + mark deleted), `$` sync changes to disk (asks
before purging deleted messages, like mutt: Enter takes the yes, and
`[mail] delete = "yes"` skips the question), Space page down,
Ctrl+L repaint, `!` shell command, Ctrl+Z suspend (fg brings it
back), `o` sort
(`d`ate `f`rom t`o` `s`ubject si`z`e `t`hreads `y`label `u`nsorted,
uppercase reverses),
Alt+v/Alt+V fold thread/all, Alt+d/Alt+u/Alt+t delete/undelete/tag a
whole thread, Ctrl+D/Ctrl+U the same for a subthread, Alt+n/Alt+p
type a number then Enter to jump to that message, `@` show the
sender's full address,
step between threads, Ctrl+R/Alt+r mark a thread/subthread read, `P`
jump to the parent, `#` break a thread, `&` link the tagged messages
under the cursor (all of these want thread sort), Alt+s/Alt+C
decode-save/decode-copy (the message as the pager shows it), `Y` edit the
X-Label, `V` show the version, Alt+l show the active limit, `l` limit,
`/` search
+ `n` next (Alt+/ searches backwards, and `n` then keeps going that
way), `c` open mailbox by path (Alt+c opens it read-only; Tab completes
mailboxes,
account folders and nearby maildirs; empty Tab opens the folder
browser), `y` folder browser (with
new/unseen counts; folders holding new mail show bold), `G` check
for new mail now, `B` toggle the sidebar (Ctrl+N/Ctrl+P move its
highlight, Ctrl+O opens the highlighted mailbox), `v` attachments,
`m` compose, `r` reply, `g` group
reply, `L` list-reply, `f` forward, `C` copy to a mailbox (no delete mark), `|` pipe
the raw message to a shell command, `b` bounce (resend as-is to new
recipients, with a Resent-\* block), `e` edit the raw message (mutt's
edit; the changed result replaces the original), Alt+e edit as a new
draft (resend), `a` add the sender to the alias file (nick prompted, local
part prefilled), `p` print (pipes the message to `mail.print`,
default `lpr`), `:` run a config command (see **Enter-command**
below), `q` quit (writes changes; asks before purging
deletions, like mutt), `x` abort without saving. Leaving a mailbox
ages unread new mail to old (`O`), mutt's mark_old.

Pager: `j`/`k` scroll, `Space`/`-` page down/up, `J`/`K` next/previous
message, `d` delete and advance, `u`/`F`/`t` undelete/flag/tag without
leaving the message, `h` toggle full headers,
`v` attachments, `m`/`r`/`g`/`L`/`f` compose/reply/list-reply/forward,
`p` print,
`s` save, `C`/`|`/`b` copy/pipe/bounce, Alt+s/Alt+C decode-save/copy,
`e`/Alt+e edit raw/resend,
`:` run a config command, `q`/`i` back. Space past the end opens the next message and wrapped
lines carry a leading `+` marker, like mutt. Replies ask mutt's
ask-yes questions: Reply-To (when the header is set), "No subject,
abort?", and "Include message in reply?"; Enter takes the yes.

Attachments: `Enter` view a text part, `s` save part to a file.

Patterns (limit/search): `~f x` from, `~s x` subject, `~b x` body,
`~t x` to, `~c x` cc, `~C x` to-or-cc, `~e x` sender, `~h x` any
header, `~i x` Message-ID, `~x x` References, `~d spec` date,
`~r spec` received date, `~m spec` index range, `~z spec` size range,
`~=` duplicate (same Message-ID twice), `~(P)`/`~<(P)`/`~>(P)` thread,
parent or child matches P, `~v` folded thread head, `~$` unreferenced,
`~y x` X-Label, `~L x` from-or-to, `~B x` whole message (headers and
body),
`~N` new, `~O` old, `~R` read, `~Q` replied, `~F` flagged, `~D` deleted,
`~U` unread, `~T` tagged,
`~l` addressed to a known mailing list, `~u` to a subscribed list,
`~p` addressed to me,
`~P` sent by me, `~A` every message; a bare word matches subject or
from. `x` is a
case-insensitive regex (`"quotes"` keep spaces; an invalid regex falls
back to plain substring). `~d` takes a day or range
(`24/12/2026`, `1/6/2026-30/6/2026`, `24/12-`, `-1/1/2027`) or an
offset: `<1w` (within), `>2d` (older than), `=3d` (that day); units
y m w d H M, and `~r` takes the same specs against delivery time.
`~m` counts the index as shown (`~m 10-20`, `~m 5-`, `~m -20`), with
`.` for the selected message and `$` for the last (`~m .-$`). `~z`
takes `>100K`, `<2M`, `1K-2M`, or a plain byte count (K/M/G are
powers of 1024). Adjacent terms AND, `!` negates, `|` ORs, `()`
groups: `!~D (~f jane | ~t jane) ~d <1m`.

## IMAP

`rmut imap:work` (or `imap:work/Archive`) opens an account folder;
`c` and the folder browser `y` take the same specs, and `y` lists the
account's folders via LIST. Messages are mirrored into a cache maildir
under `~/.cache/rmut/imap/` (headers up front, full bodies fetched on
first view), so the index is fast and old mail reopens offline. `$`
pushes your changes to the server (flags via UID STORE, deletes via
EXPUNGE). New mail is announced by **IDLE** (RFC 2177, on a second
connection) and shows up within a second; when the server doesn't
support IDLE, the NOOP poll (`poll_seconds`) picks it up as before.
The folder browser asks the server for UNSEEN counts (STATUS). A
connection dropped by laptop sleep or a server timeout is transparently
reopened and the operation retried once; polls fetch only new arrivals
unless the server reported flag changes or expunges. The
password comes from `password_command` (e.g. `pass show mail/work`),
run once per session, or from a stored `password`, if you accept a
secret sitting in the config file (keep it chmod 600).

The connection runs on a thread of its own, so a slow server does not
stop the screen: the index keeps drawing, the keys keep working, the
message line says what is happening ("fetching the message... (Ctrl+G
aborts)"), and **Ctrl+G** gives up on it the way mutt's does, after
which the next operation reconnects. The poll for new mail, `$` sync,
fetching a message body and the sidebar's unread counts all work this
way; opening a mailbox and listing folders still wait for the server.

A server that does not answer costs seconds, not the OS default of
about two minutes with nothing on screen: `[net] connect_timeout` (10s)
bounds the connection and `[net] timeout` (30s) bounds waiting for data
on a live one, and both say which host and what they were doing.
Both take `:set connect_timeout=...` / `:set net_timeout=...` at
runtime, and `--import-muttrc` brings mutt's `$connect_timeout` over.

TLS trusts the built-in Mozilla roots and, by default, the operating
system's certificate store on top (`[net] system_cas`, mutt's
`$ssl_usesystemcerts`); `[net] certificate_file` names a PEM of extra
roots for a private CA or a self-signed server. These only ever add
trust anchors, never replace the defaults. Imported from mutt's
`$certificate_file` / `$ssl_ca_certificates_file`; rmut has no
interactive accept-once, client certificates or `$tunnel` yet, so
those import as skipped.

For Gmail/O365-style **OAuth2**, set `auth = "xoauth2"` (or
`"oauthbearer"`, RFC 7628) and a `token_command` whose first output
line is a fresh access token; acquiring and refreshing tokens is the
external tool's business (oauth2ms, mutt_oauth2.py, ...). The command
runs for every connection, since tokens expire; both IMAP
(AUTHENTICATE) and SMTP (AUTH) then use the token instead of a
password.

## mbox

`rmut /var/mail/$USER` opens an mbox file: it is mirrored into a cache
maildir (like IMAP folders), so the index, pager, flags, and patterns
all work unchanged, and messages are keyed by content so your flags
survive when the spool grows. `$` sync writes changes back into the
file: deleted messages dropped, `Status:`/`X-Status:` headers
rewritten (`RO`/`AF`), mboxrd `>From` quoting preserved, all under an
exclusive flock, with a crash backup kept in the cache until the
rewrite lands, and refuses (rather than clobbers) when the spool
changed since the last look; check for new mail (`G`) and sync again.
New deliveries are picked up by the regular poll.

## PGP

PGP messages are handled on view by shelling out to gpg(1):
PGP/MIME (RFC 3156) and inline/clearsigned messages are decrypted
and/or verified, with a `[-- PGP: ... --]` verdict line at the top of
the pager (good/BAD/unverified signature). Outgoing mail is treated
per message: in the compose menu, `p` opens the security menu with
(e)ncrypt, (s)ign, (b)oth, (c)lear, and the chosen state shows in the
menu's Security line. Signing uses `sign_key` (or gpg's default key); encryption
looks keys up by recipient address and always encrypts to the sender
too, so the Fcc copy stays readable. Passphrases are gpg-agent's
business; rmut never sees them.

## Sending mail

Drafts open in `$VISUAL`/`$EDITOR` (default `vi`). If an account with
`smtp_host` applies (the open mailbox's account, or the first one
configured), the message goes out via SMTP submission (STARTTLS on
587, implicit TLS on 465, AUTH PLAIN/LOGIN, Bcc stripped from the wire
copy) and the Fcc lands in the account's `sent_folder` by IMAP
APPEND. Otherwise it is handed to `sendmail -t -oi`; setting
`$RMUT_SENDMAIL` or `mail.sendmail` forces the sendmail path. `From:`
defaults to the identity in effect (`[identity]` overlaid by the open
account's `identity` and any matching `[[identities]]` rules, with
`reverse_name` picking the address a replied-to message came to), or
falls back to `$EMAIL` / `user@hostname`; the draft's own From line
always wins, and rmut prefills it whenever an identity applies. In
the compose menu, `P` postpones the draft into a nearby Drafts maildir
(or `.rmut-postponed`); the next `m` offers to recall it. Sent mail is
copied to a nearby Sent maildir when one exists (local mailboxes).
Aliases are read from `[mail] alias_file` (mutt's own setting),
`$RMUT_ALIASES`, or `~/.config/rmut/aliases`, one mutt-style
`alias nick address...` per line; `a` in the index appends to the same
file. At the To prompt (compose
and bounce), **Tab** completes the word under the cursor: alias nicks
by prefix, plus hits from `query_command` when one is configured
(mutt's protocol: `%s` is the search word, the first output line is a
message, then `address<TAB>name` lines). Repeated Tab cycles through
multiple matches.

A reply quotes the original under mutt's `$attribution` line, one
`$indent_string` per line, and a forward takes its subject from
`$forward_format`. All three are format strings over the message being
answered (`%a` address, `%n` name, `%f` the whole From header, `%s`
subject, `%i` message-id, `%d` date, `%{...}` strftime), and the
defaults are mutt's:

```toml
[mail]
attribution = "On %d, %n wrote:"   # the quoted reply's opening line
indent_string = "> "               # what each quoted line starts with
reply_regexp = "^(re)(\\[[0-9]+\\])*:[ \\t]*"  # what a reply subject
                                   # may already start with
forward_format = "[%a: %s]"        # the subject a forward carries
include = "ask-yes"                # quote the original: yes/no/ask-*
ask_cc = false                     # mutt's $askcc, between To and
ask_bcc = false                    # Subject; $askbcc follows it
forward_quote = false              # true indents the forwarded text
```

Every draft rmut starts can end with a signature (mutt's `$signature`
and `$sig_dashes`), and the two questions mutt asks on the way in are
settable (`$abort_nosubject`, `$abort_unmodified`):

```toml
[mail]
signature = "~/.signature"    # a file, or a command when it ends in |
sig_dashes = true             # the "-- " line above it (the default)
sig_on_top = false            # true puts the signature above the quote
hostname = "mail.example.net" # the Message-ID host (mutt's $hostname)
user_agent = false            # true adds a User-Agent: rmut/... header
abort_nosubject = "ask-yes"   # empty subject: yes/no/ask-yes/ask-no
abort_unmodified = true       # a first edit that changed nothing is
                              # not a message: the draft is dropped
```

The signature is read afresh for every draft, so `signature =
"fortune |"` says something new each time; one that cannot be read is
simply left off. A recalled postponed message keeps the signature it
was postponed with rather than gaining a second one.

By default (like mutt) the editor gets only the message body;
headers come from the prompts, and attachments are added with `a` at
the compose menu. With `edit_headers = true` the draft's header block
is part of the editor buffer, where you can adjust To/Cc/Subject
directly and attach files with `Attach:` pseudo-headers:

```
To: jane@example.com
Subject: the report
Attach: ~/report.pdf the Q2 numbers
Attach: "/tmp/two words.png"

see attached
```

Each one becomes a base64 part of a multipart/mixed message (content
type guessed from the extension, the rest of the line an optional
description). After the editor you land in mutt's **compose menu**:
the draft's From/To/Cc/Bcc/Subject/Fcc/Security above the attachment
table (body, forwarded original, every `Attach:` file with size and
type). `y` sends, `e` reopens the editor, Enter views the selected
entry (text directly, other types via `[filters]`), `t`/`c`/`b`/`s`
edit the headers, `a` attaches without a trip through the editor, `D` detaches
the selected file, `p` opens the security menu, `P` postpones, and
`q` asks "Postpone this message?" (no discards). PGP signing and encryption wrap the whole multipart, attachments
included; this also works for forwards with `forward = "attach"`.

With several postponed drafts, recalling (`m`, then `r`) opens a
picker instead of silently taking the newest.

## Enter-command

`:` opens mutt's command prompt (the same line editor as the other
prompts, with its own history) and applies one config line to the
running session. Nothing is written back to the config file, so it is
a place to try a setting before keeping it.

```
:set index_format="%4C %Z %{%b %d} %-15.15L (%?l?%4l&%4c?) %s"
:set nobeep                 # also: set beep, unset beep, toggle beep
:set invtilde               # mutt's inv prefix toggles
:set pager_index_lines=6 pager_context=2
:set sort?                  # report a value instead of setting it
:bind index \Cd delete-message      # mutt keys and function names
:macro pager S "s=archive<enter>"
:color index brightyellow default ~F
:ignore x-spam-score        # and unignore, to bring one back
:alternates 'jane@old\.example\.com'   # and unalternates (* clears)
:my_hdr Organization: Acme  # and unmy_hdr Organization (* clears)
:alias jane Jane Doe <jane@example.com>
:push "<enter>"             # keys into the input queue
:exec sync                  # run one function now
```

Settable at runtime: `index_format`, `date_format`, `sort`,
`sort_aux`, `pager_format`, `pager_index_lines`, `pager_context`,
`quote_regexp`, `wrap`, `tilde`, `status_format`, `theme`, `beep`,
`from`, `realname`, `reverse_name`, `edit_headers`, `fast_reply`,
`autoedit`, `copy`, `forward`/`mime_forward`, `sendmail`, `editor`,
`print_command`, `query_command`, `trash`, `record`, `postponed`,
`new_mail_command`, `mail_check`, `undo_send`, `metoo`, `text_flowed`,
`attribution`, `indent_string`, `forward_format`, `include`, `askcc`,
`askbcc`, `connect_timeout`, `net_timeout`, `pager_stop`, `markers`,
`smart_wrap`, `collapse_unread`, `uncollapse_jump`, `quit`,
`confirmappend`, `save_name`, `force_name`, `forward_quote`,
`signature`, `sig_dashes`, `abort_nosubject`, `abort_unmodified`,
`mark_old`, `print`, `beep_new`, `wait_key`, `reverse_realname`,
`reflow_text`, `notmuch`, `sidebar_visible`,
`sidebar_width`, `pgp_sign_as`, `crypt_autosign`, `crypt_autoencrypt`.
An unknown option, a bad number, an unbindable key, or an unknown
function reports on the bottom line in the error color and stops the
rest of the line. Changed settings recompile in place: colors, key
tables, the quote regexp, header rules, and a resort when the sort
order moved.

## Command line

```
rmut [-R] [-e CMD]... [-p|-y] [-z|-Z] [-f MAILBOX | MAILBOX | mailto:URL]
rmut -s SUBJECT [-c CC] [-b BCC] [-a FILE]... [-i FILE] -- ADDRESS...
rmut --import-muttrc [-w] [MUTTRC]
```

`-R` read-only, `-f` the mailbox to open, `-p` the postponed picker,
`-y` the mailbox list, `-z`/`-Z` exit 1 instead of starting when the
mailbox is empty or has no new mail (for a prompt or a cron job), `-e`
runs an enter-command line before the first draw (repeatable).

A `mailto:` URL opens a prefilled draft (to, cc, bcc, subject, body,
percent-decoded) with the mailbox still open behind it, which is what
a desktop mail handler passes:

```
rmut 'mailto:jane@example.com?subject=Lunch&body=Friday%3F'
```

**Sending without the TUI**, for scripts and one-shot mail: `-s`,
`-c`, `-b`, `-a` (repeatable), or `-i`, or a bare `--`, puts rmut in
send mode. The body is stdin (or `-i FILE`), the recipients are the
remaining arguments, the identity and transport come from the config,
and the exit code says whether the message went out:

```
rmut -s "nightly build" -a build.log -- ops@example.com < report.txt
```

The sent copy goes to a local `mail.sent` maildir; a remote Sent
folder is left to the interactive send, since an IMAP APPEND needs the
account opened. A configured `signature` ends a batch message too, as
in mutt.

## Mailing lists

Tell rmut which addresses are lists and it stops guessing:

```toml
[mail]
lists      = ["announce@lists.example.com"]   # lists you read
subscribed = ["rmut-dev@lists.example.com"]   # lists you are on
```

Entries are case-insensitive regexes matched against addresses, and
`subscribed` counts as a known list too. mutt's `lists`, `subscribe`,
`unlists` and `unsubscribe` are imported.

With that, `L` replies to the list alone: the List-Post address when
the list published one, otherwise the known list address from To/Cc.
On a message from no known list it refuses rather than quietly mailing
the author. `~l` limits to list mail, and `%L` in the index format
already shows "To <list>".

Mail going to a known list carries a `Mail-Followup-To` so replies
land on the list (mutt's $followup_to). On a list you are subscribed
to, your own address is left out, since the list copy is the one you
will get; on a list you only read, it stays in. A sender's own
`Mail-Followup-To` is honored by a group reply: it replaces the
recipient set rather than adding to it.

## Who counts as me, and my_hdr

`[identity] email`, the accounts, and the `[[identities]]` rules
already name your addresses. `alternates` adds the rest: an old
domain, a role address, whatever forwards to you.

```toml
[mail]
alternates = ['jane@old\.example\.com', '^(jane|jd)@example\.com$']
my_hdr     = ["Organization: Acme", "Bcc: jane@example.com"]
metoo      = false   # true: a group reply copies you too
```

Entries are case-insensitive regexes over the bare address, mutt's
`alternates`, and one answer serves everywhere rmut asks whether an
address is yours: `~p` and `~P`, the third `%Z` character (`+` sole
recipient, `T` one of several, `C` on the Cc, `F` sent by you, `L` to
a subscribed list), `reverse_name` picking the address a message came
to, and the dedup a group reply does. That dedup is mutt's: replying
to all drops your own addresses and the person already in the To, so
you get no copy of your own mail and nobody gets two. `metoo = true`
keeps you on the list, like mutt's $metoo.

`my_hdr` lines ride on every draft the compose menu, a `mailto:` URL,
or a batch send produces; with `edit_headers` they show in the editor
like any other header. One naming a header rmut already wrote replaces
it, so `my_hdr From:` and `my_hdr Reply-To:` win; `To`, `Cc` and `Bcc`
gain the address instead, so a standing `my_hdr Bcc:` cannot erase a
reply's recipients. There is one entry per header name: a later
`my_hdr` for the same header replaces the earlier one, and `unmy_hdr`
takes it off. mutt's `alternates`, `unalternates`, `my_hdr`,
`unmy_hdr` and `set metoo` are imported, and all of them work at the
`:` prompt.

## format=flowed

A `text/plain; format=flowed` part (RFC 3676) arrives split at
whatever width the sender's terminal happened to be. rmut puts it back
into paragraphs and lets the pager wrap it at your width, so `[pager]
wrap` and the window govern as they do for everything else. Quote
depth bounds a paragraph and survives as `>` marks, so quoted text
still colours and folds; space-stuffing is undone, `DelSp=yes` is
honoured, and `-- ` stays a fixed line. mutt's `$reflow_text` turns it
off:

```toml
[pager]
reflow_text = false   # keep the sender's line breaks

[mail]
text_flowed = true    # send text/plain; format=flowed
```

`text_flowed` declares the outgoing text part `format=flowed` and
space-stuffs it, in the plain case, under attachments, and inside a
PGP signature or encryption alike. The paragraphs themselves are your
editor's doing, exactly as in mutt: a line that continues has to end
with a space, and rmut adds none of its own.

## Mailbox names: `=` and `+`

Set `[mail] folder` and a mailbox can be named under it, the way mutt
does it:

```toml
[mail]
folder = "~/Mail"            # or an account: "imap:work"
mailboxes = ["=inbox", "=lists"]
trash = "=trash"
```

`=x` and `+x` mean `$folder/x`, and `=` alone is the folder itself.
It works wherever a mailbox is named: the change-folder prompt, save
and copy, the compose menu's Fcc, the configured mailboxes, trash,
sent, postponed and the fcc-hook targets. It also works in a macro,
which is what lets an imported mutt line like
`macro index S "<save-message>=archive<enter>"` land where it means
to. Tab completion still works on real paths only, so complete first
or type the shorthand whole.

With `folder = "imap:work"`, `=Archive` is that account's Archive
folder.

## Credentials

An account's password can sit in the config as `password = "..."`,
and `--import-muttrc` puts `set imap_pass` there because that is
where mutt had it. It is the weakest of the options: the config is a
plain file in `~/.config`, and anyone who can read it can read your
mail. rmut writes the files it creates at mode 600, and says so at
startup if a config holding a password is readable by anyone else:

```
chmod 600 /home/you/.config/rmut/config.toml (it holds a password and others can read it)
```

Better is to keep the secret somewhere else and name the command that
fetches it. `password_command` runs once per session, and its first
line of output is the password:

```toml
[[accounts]]
name = "work"
user = "jane"
# pick one:
password_command = "pass show mail/work"          # pass(1), GPG-backed
password_command = "gpg -q -d ~/.config/rmut/imap.gpg"   # a file you encrypted
password_command = "secret-tool lookup service imap user jane"   # libsecret
password_command = "cat /media/crypt/mail-pass"   # an encrypted volume
```

Anything that prints the password works. A GPG-backed one prompts
once per session and gpg-agent remembers it for a while; a command
that fails takes the connection down with its message, so a locked
store fails closed rather than silently.

For a provider that wants OAuth2 rather than a password, `auth =
"xoauth2"` (or `"oauthbearer"`) with `token_command` runs the token
helper for every connection, since tokens expire.

## Threads

`o t` sorts by threads, and then the thread is a unit you can act on:
Alt+d, Alt+u and Alt+t (mutt's `Esc d`, `Esc u`, `Esc t`) delete,
undelete and tag the whole thread the cursor sits in, Ctrl+D and
Ctrl+U do the same for the subthread (the message under the cursor
and its replies), and Alt+n / Alt+p step to the next and previous
thread. Alt+v folds one thread, Alt+V all of them.

Each of these is one undo step, however many messages hang off it, so
`z` brings a thread back whole. Deleting advances to the next
undeleted message, mutt's `$resolve`, which makes clearing thread
after thread one repeated key; undeleting and tagging stay put.
Tagging follows the cursor, so a second Alt+t untags the thread.
Without thread sort they refuse, as they do in mutt.

Ctrl+R and Alt+r (mutt's `Ctrl+R`, `Esc r`) mark the thread or the
subthread read, `P` jumps to the parent message (root-message is
there too, unbound), and tag-subthread is an action for `:bind`.

Two keys edit the threading itself, since misconfigured mailers
leave replies dangling or bolt a new discussion onto an old one.
`#` (break-thread) takes the In-Reply-To and References off the
message under the cursor, so it and its replies become a thread of
their own; `&` (link-threads) makes the tagged messages replies to
the one under the cursor, as mutt does, by giving each an In-Reply-To
naming it (and untagging it). Both rewrite the message file in place
and are one undo step each: `z` writes the old headers back. They
work on local maildirs; on IMAP and mbox the real message lives
elsewhere, so they refuse rather than edit a copy.

The patterns know threads too: `~(P)` matches every message in a
thread where some message matches P (`~(~P)`: threads I took part
in), `~<(P)` the messages whose parent matches P (`~<(~P)`: replies
to my mail), `~>(P)` those with a child matching P, `~v` the head of
a folded thread, and `~$` a message with no parent and no children.
Outside thread sort `~(P)` reads as P and the rest are false.

A reply's subject is "Re: " over the original with whatever
`[mail] reply_regexp` matched at its start taken off (mutt's
`$reply_regexp`, default `^(re)(\[[0-9]+\])*:[ \t]*`), so "RE: x" and
"Re[2]: x" both answer as "Re: x"; a locale's prefixes go in as
`^(re|aw|sv):[ \t]*`. It is case-insensitive unless it holds an
uppercase letter, as mutt compiles it.

## Tagged operations

`t` tags a message and `;` hands the tagged set to the next function
(`Tag-` sits on the message line while it waits, as in mutt):
`d`/`u`/`F`/`N`/`t` mark them all, `s`/`C` save or copy them all (one
prompt, one undo step), `|` and `p` pipe or print them concatenated
into a single run of the command (mutt's `$pipe_split` unset, the
separator mutt's `$pipe_sep`; `pipe_split`/`print_split` = true run it
once per message instead), and
`b` bounces them all to the same addresses, with the confirmation
counting what it is about to do.

A function that cannot take a set says so ("resend does not take the
tagged set") rather than quietly acting on the one under the
cursor. `T` and Ctrl+T tag and untag by pattern; `;t` clears the tags,
the way it does in mutt.

## The attachment reminder

Set `[mail] abort_noattach` and a draft whose body mentions an
attachment when none is attached gets a question before it goes:

```toml
[mail]
abort_noattach = "ask"       # "no" (default), "ask", or "yes" (refuse)
attach_keyword = '\b(attach|attached|attachment)\b'   # the default
```

Answering `n` puts you back in the compose menu, where `a` attaches
the file you meant. Quoted lines and anything below a `-- ` signature
do not count, so a reply to someone else's "see attached" and a
signature advertising an attachment opener are not false alarms.

neomutt's `abort_noattach` and `abort_noattach_regex` import
(`ask-yes` and `ask-no` both become `ask`), and mutt's `\<` / `\>`
word edges are translated to `\b` on the way in. Batch sends do not
ask: there is no terminal to answer at.

## Undo

`z` walks back the last change to your messages: a delete or
undelete, a flag or read toggle, a tag, a `D`/`U`/`T`/Ctrl+T pattern
sweep, or a save or copy to another mailbox. One keystroke is one
step however many messages it touched, so a pattern delete over three
hundred messages comes back in one go, and a save's copy in the
target mailbox is removed again along with the original's delete
mark.

A step remembers the messages as they stood before it, and puts that
state back. Writing the mailbox ends what can be undone: `$` (and the
write on quit) drops the stack, because those changes are on disk and
the deleted ones are gone. Up to 32 steps are kept.

This one is rmut's own; mutt has nothing like it. It is cheap here
because rmut already defers every mark to the sync.

### Undo send

`[mail] undo_send` holds a sent message for that many seconds before
anything leaves the machine:

```toml
[mail]
undo_send = 10   # 0 (the default) sends at once, as mutt does
```

The status line counts the seconds down, and `z` takes the message
back: not just cancelled, but returned to its compose menu with the
draft as you left it, ready to edit and send again. A held message is
the most recent thing you did, so `z` reaches it before it reaches
the mark history.

The timer running out sends it, and so does leaving rmut: quitting is
not cancelling. Batch sends (`-s` and friends) never hold, since
there is no terminal to press `z` at.

### Rescuing one message from a run of deletions

Deleted messages stay in the index (they only go on `$`), so `j`/`k`
land on them and `u` puts one back. When there are many, three things
help: `U <pattern>` undeletes a whole set (`U ~f boss`), `l ~D` limits
the view to the marked ones, and a pattern search hops between them:
`/~D` then `n`, with Alt+/ to go the other way. The two macros above
put that hop on `.` and `,`.

In the pager, `j`/`k` step over deleted messages by design (mutt does
the same); `J`/`K` step to any message, deleted ones included, and
`u` puts one back without going out to the index.

## Which part shows, and mailcap

A `multipart/alternative` message carries the same text twice or more.
mutt's `alternative_order` decides which copy you read, most wanted
type first; `text/*` matches a whole main type, and anything not
listed falls back to rmut's ranking (a part with a filter, then
enriched over plain over html):

```toml
[pager]
alternative_order = ["text/plain", "text/html"]
```

`[filters]` is mutt's `auto_view`: a MIME type and the command that
turns it into text on stdout. Leave the command empty and rmut takes
it from your mailcap, exactly where mutt takes it from: the first
`copiousoutput` entry for the type, skipping one whose `test=` fails
or that wants the terminal, with `%s` given a temporary file. The
files are `$MAILCAPS`, or `~/.mailcap`, `/etc/mailcap`,
`/usr/etc/mailcap`, `/usr/local/etc/mailcap`. A type with no such
entry simply does not autoview, and the part stays an attachment.

`auto_view`, `unauto_view`, `alternative_order` and
`unalternative_order` all work at the `:` prompt as well, and import
from a muttrc.

## Hooks

Beyond `[[identities]]` (the from/realname half of folder-hook and
send-hook), four mutt hooks have tables of their own. The three that
carry a command line take exactly what the `:` prompt takes.

```toml
[[folder_hooks]]              # on opening a matching mailbox
folder  = "*work*"            # glob on the mailbox, path or imap: spec
command = 'set index_format="%4C %Z %-6d %-20.20F %s"'

[[message_hooks]]             # while that message is selected
pattern = "~f boss@example.com"
command = "set pager_context=5"

[[reply_hooks]]               # while a reply to it is built
pattern = "~f boss@example.com"
command = "set from=jane@work.example.com"

[[fcc_hooks]]                 # where the sent copy goes
pattern = '~t @work\.example\.com'
mailbox = "~/Maildir/.WorkSent"

[[crypt_hooks]]               # encrypt to this key for this recipient
address = "boss@example.com"
key     = "0xDEADBEEF"
```

A **message-hook** is in force only while its message is the selected
one: the moment the match set changes, every setting it touched goes
back to what it was, so a display setting really is per-message. A
**reply-hook** applies while the reply's draft is built, which covers
`set from`, `edit_headers` and `my_hdr`. A **folder-hook** is not
undone when you leave, exactly like mutt, so a catch-all entry
(`folder = "*"`) is how you put a setting back.

**fcc-hook** patterns match the draft as it stands after the editor,
so the compose menu's Fcc line already shows where the copy is going;
an Fcc chosen by hand with `f` still wins, and batch sends honour the
hook too. Bcc addresses join the Cc ones for matching, so `~c` sees a
blind recipient. **crypt-hook** replaces a recipient's address with a
key id when gpg is asked to encrypt.

## Configuration

`$RMUT_CONFIG` or `~/.config/rmut/config.toml`:

```toml
[identity]
name = "Jane Doe"            # From: Jane Doe <jane@example.com>
email = "jane@example.com"
reverse_name = false         # true: a reply's From becomes whichever
                             # of your addresses the mail was sent to
reverse_realname = true      # false: only the address comes over, the
                             # name above stays (mutt's setting)

[[identities]]               # conditional identity (folder-/send-hook):
folder = "*work*"            # glob on the open mailbox, and/or
recipient = "*@work.example.com"   # glob on a draft recipient;
name = "Jane Work"           # matching rules overlay [identity] in
email = "jane@work.example.com"    # order, unset fields fall through

[mail]
folder = "~/Maildir"         # mutt's $folder: "=x" and "+x" name a
                             # mailbox under it, at a prompt or in a
                             # macro ("imap:work" works too)
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
trash = "~/Maildir/.Trash"   # purged mail moves here (mutt's $trash;
                             # imap:acct/Trash for IMAP mailboxes);
                             # purging inside it deletes for real
edit_headers = false         # true: the header block is part of the
                             # editor buffer (To/Cc/Subject, Attach:)
alternates = ['jane@old\.example\.com']   # my other addresses
my_hdr = ["Organization: Acme"]           # on every draft
metoo = false                # true: a group reply copies me too
text_flowed = false          # true: send text/plain; format=flowed
undo_send = 0                # seconds a sent message waits, so z can
                             # take it back (0 sends at once)
delete = "ask"               # mutt's $delete: "yes" purges without
                             # asking, "no" keeps the marks
abort_noattach = "no"        # "ask"/"yes": a body that mentions an
                             # attachment with none attached is
                             # questioned before it goes
signature = "~/.signature"   # ends every draft; a name ending in |
                             # is a command whose output it is
sig_dashes = true            # the "-- " line above the signature
forward_quote = false        # true: the forwarded text comes in
                             # quoted with indent_string
abort_nosubject = "ask-yes"  # empty subject: "yes" aborts without
                             # asking, "no" never asks
abort_unmodified = true      # false: keep a draft the first editor
                             # pass left untouched
mark_old = true              # unread mail ages to old (O) when you
                             # leave the mailbox, as in mutt
print_confirm = "ask-no"     # mutt's $print: "ask-yes" makes Enter
                             # print, "yes" never asks, "no" refuses

[index]
format = "%4C %Z %-6d %-15.15L (%?l?%4l&%4c?) %s"   # mutt's default
                                         # %Z status/flag/mark, where
                                         # the mark is mutt's to_chars:
                                         # + sole recipient, T one of
                                         # several, C on the Cc, F sent
                                         # by me, L to a subscribed list
                                         # %F from (%L: "To <list>" for
                                         # List-Id mail) %c size %l body
                                         # lines %M collapsed count
                                         # %s subject, and
                                         # %?X?then&else? conditionals
sort = "threads"             # initial sort; reverse-date, size, ...
sort_aux = "last-date-sent"  # which thread comes first: last- by its
                             # newest message, reverse- newest thread
                             # first (mutt's spellings)
date_format = "%d.%m.%Y"     # strftime for the date column

[pager]
index_lines = 10             # keep a slice of the index above the pager
context = 3                  # overlapping lines when paging
reflow_text = true           # false: keep a format=flowed part's own
                             # line breaks instead of rewrapping it
alternative_order = ["text/plain", "text/html"]
                             # which part of a multipart/alternative
                             # shows, most wanted first ("text/*" ok)

[filters]                    # auto_view: render a part via a command
"text/html" = "w3m -dump -T text/html -O UTF-8"
"text/calendar" = ""         # empty: take the command from mailcap

[net]                        # how long the network gets before rmut
connect_timeout = 10         # says so; 0 waits as long as the OS
timeout = 30                 # does (about two minutes). timeout is
                             # data on a live connection, never off:
                             # IMAP IDLE ticks on it, minimum 5
system_cas = true            # trust the OS cert store too (mutt's
                             # $ssl_usesystemcerts); adds to the roots
certificate_file = "~/.mutt/certs.pem"  # a PEM of extra roots to
                             # trust (a private/self-signed CA)

[ui]
theme = "default"            # or "mono"
status_format = "---rmut: %f [Msgs:%?M?%M/?%m New:%n%?d? Del:%d?] (sort:%s)%?V? (limit:%V)?"
                             # bottom line: %f mailbox %m msgs
                             # %M shown-when-limited %n new %u unread
                             # %d deleted %F flagged %t tagged %s sort
                             # %V limit %r pending-mark %v version,
                             # with %?X?then&else? conditionals
beep = true                  # ring the bell on an error (mutt's $beep)
beep_new = false             # true: ring when mail arrives, too
wait_key = true              # a shell escape (!) ends with "Press
                             # Enter", so its output can be read

[[color_index]]              # mutt's `color index FG BG PATTERN`:
pattern = "~f boss@example.com"   # any limit/search pattern; first
fg = "yellow"                # matching rule colors the index line,
# bg = "blue"                # over the [colors] slots below

[sidebar]                    # left pane: mail.mailboxes with new-mail
visible = false              # counts (B toggles at runtime; bold =
width = 24                   # has new mail, > marks the open one)

[colors]                     # status_fg status_bg deleted flagged
deleted = "red"              # tagged header. One colour each: for
                             # mutt's `color index black magenta ~D`
                             # (a painted bar) use a [[color_index]]
                             # rule, which takes fg and bg both

[keys.index]                 # remap: action = "key" (see ? for actions)
sync = "w"
[keys.pager]

[macros.index]               # macro: key = "replayed key sequence",
L = "l~f jane<enter>"        # literals + <enter>/<esc>/<ctrl+x>/...;
"." = "/~D<enter>"           # hop to the next message marked deleted,
"," = "<alt+/>~D<enter>"     # and back again
[macros.pager]               # it feeds the input queue, so it can
                             # drive prompts; a macro shadows a
                             # binding on the same key (like mutt)

[[accounts]]                 # remote account: open with `rmut imap:work`
name = "work"
user = "jane@example.com"
password_command = "pass show mail/work"   # first stdout line
# password = "..."                         # alternative, but see Credentials below
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
rmut --import-muttrc -w            # reads ~/.muttrc, writes the config
rmut --import-muttrc               # or print it, to look first
```

`-w` saves it to `~/.config/rmut/config.toml`, creating the directory
and refusing to overwrite a config that is already there. Any `alias`
lines it found, including ones in a `source`d file, are written to
`~/.config/rmut/aliases` at the same time: rmut keeps aliases in a
mutt-format file of their own rather than in the TOML, so your mutt
alias file works as it stands (point `$RMUT_ALIASES` at it if you
would rather keep it where it is), and `a` in the index appends to
it. Without `-w` the translation goes to stdout for review, aliases
as a comment block, so redirect it yourself if that is what you
want.

The translation covers identity, folder/mailboxes, record/postponed,
sendmail/editor/print_command/query_command/status_format, binds,
status/header colors and `color index FG BG PATTERN` rules, PGP
defaults, IMAP/SMTP URLs into an `[[accounts]]` skeleton, with
`auth`/`token_command` when `*_authenticators` names
oauthbearer/xoauth2, reverse_name, alternates/unalternates,
my_hdr/unmy_hdr, text_flowed/reflow_text,
auto_view/unauto_view (the command left to mailcap),
alternative_order/unalternative_order,
folder-hooks/send-hooks that only set from/realname into
`[[identities]]` rules, every other folder-hook plus message-hook,
reply-hook, fcc-hook/fcc-save-hook and crypt-hook into their own hook
tables, and macros whose sequence is plain keys and
prompt input) into rmut
TOML on stdout for review; it never writes any file itself.
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

Tests sit where what they test does. `rmut-core` tests parsing and
protocols, `rmut-session` tests the operations against a maildir in a
tempdir and asserts on values (`crates/rmut-session/src/tests.rs`),
and the pty suite in `tests/e2e` drives a real terminal for the keys,
the drawing, and the paths that go all the way out through sendmail,
IMAP and gpg.

Three crates. `rmut-core` is the mail itself: maildir, mbox, IMAP,
SMTP, compose, PGP, patterns, threading, the importer. `rmut-session`
is an open mailbox and everything that can be done to it, with no
screen attached: what is in it, what is selected, marks, sync, save
and copy, the undo stack, the outbox, the hooks. `rmut-tui` owns the
menus, the keys, the theme and the drawing, and drives a session.

An operation reports what it did as a `notice::Notice` rather than
writing into a status field, and the front end installs the sink that
receives it: the terminal keeps the last notice for its message line,
a test reads the values. A session with no sink installed is silent.

When an operation needs an answer it hands back an `Ask` ("Save to
mailbox: ", "Purge 3 deleted message(s)? (y/n): ") rather than opening
a prompt, and the front end answers with the `AskKind` it came with;
answering can produce the next question. What only a front end can do
comes back as a `Request`: quit, run an editor or a shell command,
suspend, show the draft again. A front end that cannot do one of them
simply does not honour it. See [docs/PLAN.md](docs/PLAN.md) for where
that line is headed.
