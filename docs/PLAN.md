# rmut roadmap

Goal: a daily-drivable mutt replacement: mutt's index/pager workflow,
message manipulation, and compose, with mutt default keybindings
throughout. That goal is met: R1–R15 shipped maildir, IMAP/SMTP with
IDLE and OAuth2, mbox, PGP, the muttrc importer, patterns, identities,
macros, and completion. From here, bugs and paper cuts found in daily
use outrank everything below; the remaining milestones are ranked
nice-to-haves, none committed until picked up.

**1.0 declared** (2026-07) after R1–R5. Remaining milestones below are
post-1.0.

## R1: read-only browser (done, 0.1)

- [x] Workspace: `rmut-core` (maildir scan, message parsing via
      mailparse) + `rmut-tui` (ratatui binary `rmut`)
- [x] Maildir `new/` + `cur/` scan with filename flag parsing (S/R/F/T/D)
- [x] Index: mutt-style line (number, status char, date, from, subject),
      date sort, j/k/PgUp/PgDn/=/* navigation, bold new/unseen
- [x] Pager: decoded headers + first text/plain body, scroll, percent
      indicator, J/K to move between messages
- [x] Unit tests for flag parsing, MIME text extraction, RFC 2047

## R2: a real MUA index (done, 0.2)

- [x] Body line wrapping in the pager (word wrap at terminal width)
- [x] Mark for deletion `d`, undelete `u`, sync `$` (rename/remove in
      maildir; move new/ → cur/ with S flag after reading)
- [x] Flag toggle `F`, mark read/unread `N`
- [x] Sort orders `o` (date, from, subject, size; uppercase = reverse)
- [x] Limit `l` and search `/`+`n` with mutt-ish patterns
      (~f, ~s, ~b, ~N, ~F, ~D, ~U, bare word = subject|from)
- [x] Mailbox switching `c`, folder browser `y` (children + siblings)
- [x] `h` full-header view in pager; attachment list `v`, view text
      parts with Enter, save part `s`

## R3: threading and compose (done, 0.3)

- [x] Threaded index (References/In-Reply-To, JWZ-style in
      `core::thread`; subject grouping not done), sort by threads
      (`o t`), thread folding (Alt+v one thread, Alt+V all)
- [x] Compose `m`, reply `r`, group reply `g`, forward `f`: To/Subject
      prompts (prefilled from the original), $EDITOR on a draft, send
      via sendmail(1) `-t -oi` ($RMUT_SENDMAIL overrides; From/Date/
      Message-ID added when missing; In-Reply-To/References set)
- [x] Postponed messages (send prompt `p`; recalled from `m`, stored in
      a Drafts maildir or .rmut-postponed), Fcc copy to a Sent maildir
      when one exists nearby
- [x] Address aliases file (`alias nick addr` in $RMUT_ALIASES or
      ~/.config/rmut/aliases, expanded at the To prompt)

## R4: config and polish (done, 0.4)

- [x] Config file (TOML, $RMUT_CONFIG or ~/.config/rmut/config.toml):
      identity (From), mailboxes, sent/postponed maildirs, sendmail,
      editor, poll interval, index format string (`%C %Z %d %F %c %s`
      with mutt-ish width/precision), color overrides, key remaps
      ([keys.index]/[keys.pager], action = "key")
- [x] Themes: "default" (mutt's look) and "mono", plus [colors]
      overrides (status_fg/status_bg/deleted/flagged/header)
- [x] New-mail detection while running (mtime polling of new/ and cur/,
      poll_seconds, pending changes survive the rescan)
- [x] Help screen `?` generated from the live keymap (remaps show up);
      pty e2e suite in tests/e2e/run.py (`just e2e`, part of
      `just check`)

## R5: IMAP and SMTP (done, 0.5)

- [x] Account config in config.toml: `[[accounts]]` with imap
      host/port/tls, username, `password_command` (pass(1)-style, never
      a plaintext password), smtp host/port
- [x] IMAP connection over TLS (rustls); LIST folders into the folder
      browser `y`, SELECT + FETCH envelopes/flags to build the index
- [x] Body fetch on view, with a local header/body cache so the index
      stays fast and re-opening a message is free
- [x] Sync `$` maps index ops to IMAP: flags via STORE (Seen/Flagged/
      Answered/Deleted), delete via EXPUNGE
- [x] New-mail polling per account (reuse poll_seconds; IDLE later)
- [x] SMTP send as an alternative to sendmail(1): submission with
      STARTTLS or implicit TLS, AUTH PLAIN/LOGIN; Fcc via IMAP APPEND
      to the Sent folder

## R6: PGP/GPG (done, 1.1)

- [x] Decrypt on view: PGP/MIME (multipart/encrypted) and inline PGP
      via gpg(1), decrypted body shown in the pager
- [x] Verify signatures (multipart/signed + inline), good/bad/unknown
      status line in the pager
- [x] Compose: sign, encrypt, or both from the send prompt (mutt-style
      security menu), recipient key lookup by address, encrypt-to-self
- [x] Config: `[pgp]` section: gpg command, default signing key,
      sign_by_default / encrypt_by_default

## R7: muttrc importer (done, 1.2)

- [x] `rmut --import-muttrc [FILE]`: translate a muttrc into rmut TOML
      on stdout (review-and-save, never writes the config itself)
- [x] set: identity, folder/spoolfile/mailboxes (+/= expansion),
      record/postponed/sendmail/editor/mail_check, index_format,
      pgp_sign_as/crypt_autosign/crypt_autoencrypt
- [x] bind (index/pager function + \C/\e/<name> key translation),
      color (status/header/~D/~F slots), source includes, alias
      passthrough (rmut reads mutt alias files as-is)
- [x] everything untranslatable surfaces as `# not imported:` comments;
      imap_pass imports as the account's stored `password` (1.3),
      secrets are never echoed into comments

## 1.3–1.6: mutt-parity batches (done, no milestone)

Driven by real-world muttrc imports rather than planned up front:
stored `password` on accounts (1.3); IMAP STARTTLS and the first
"implement missing" batch (ssl_starttls/force_tls, smtp_pass,
smtp_authenticators (AUTH LOGIN fallback), print_command) in 1.4; the
second batch was sort/sort_aux, date_format, mime_forward,
pager_index_lines/context, ~T pattern + tagged color, imap_peek,
auto_view filters, extra binds (1.5); index_format `%?X?then&else?`
conditionals, `%L`, 3-char `%Z` (1.6); mutt-style quit/purge prompts.

## R8: compose attachments and message commands (done, 1.7)

The biggest daily-use gap: rmut could not attach a file to outgoing
mail.

- [x] Attach files: `Attach: <path> [description]` pseudo-headers in
      the draft, collected at send into multipart/mixed (base64,
      content-type guessed from the extension); works for compose,
      reply, and forward; attachment count shown at the send prompt
- [x] PGP over the assembled multipart, lifting the old "no PGP with
      an attached forward" restriction (RFC 3156 wraps any entity)
- [x] `C` copy to mailbox, like `s` save but without marking the
      original deleted
- [x] `|` pipe the raw message to a shell command
- [x] `b` bounce (Resent-\* block prepended, recipients on the
      sendmail argv / SMTP envelope) and `e` edit the message as a
      new draft (mutt's resend)

## R9: identities and hook basics (done, 1.8)

One global `[identity]` before; mutt users switch From per folder and
per recipient via folder-hook/send-hook.

- [x] Per-account identity on `[[accounts]]` (`identity = { name,
      email }`): From follows the open mailbox's account
- [x] mutt's `reverse_name`: replying uses whichever of our addresses
      the original was addressed to, display name kept
- [x] `[[identities]]` rules: `folder` and/or `recipient` globs,
      layered over `[identity]` in order (folder-hook + send-hook in
      one mechanism); the draft gets a visible, editable From line
- [x] Importer: `reverse_name` and folder-hook/send-hook lines that
      only set from/realname translate (regex → glob for the easy
      shapes); anything else stays `# not imported`

## R10: patterns v2 (done, 1.9)

- [x] Real pattern parser: `!` negation, `|` OR, `()` grouping
      (adjacent terms stay implicit AND); parse errors reach the
      status line instead of matching nothing
- [x] `~d` date ranges (`~d 01/06/2026-30/06/2026`, open ends, `<1w`,
      `>2d`, `=3d`; units y m w d H M)
- [x] `~t` to, `~c` cc, `~C` to-or-cc, `~e` sender (header read on
      demand), `~p` addressed-to-me
- [x] String arguments are case-insensitive regexes (regex-lite); an
      invalid regex degrades to the old substring match

## R11: new-mail awareness and IDLE (done, 1.10)

- [x] Track new mail across the configured local mailboxes (mutt's
      `mailboxes` notion): the poll watches their new/ and hints
      "new mail in ..." in the status line when one grows
- [x] Counts in the folder browser `y` (local: scan `new/`; IMAP:
      STATUS UNSEEN, the open folder from its cache); folders with
      new mail show bold
- [x] IMAP IDLE on the open folder (RFC 2177, dedicated connection,
      re-issued before the half-hour limit, stops on mailbox switch);
      NOOP polling stays as the fallback when the server lacks it

## R12: address completion (done, 1.11)

- [x] Tab at the To prompt (compose and bounce) completes the word
      under the cursor against alias nicks by prefix; repeated Tab
      cycles multiple matches, with a match counter shown behind the
      input
- [x] `query_command` (khard/abook/LDAP): run on Tab with the current
      word (`%s` or appended, shell-quoted), mutt's tab-separated
      output parsed (first line skipped, `addr<TAB>name` rows);
      imported 1:1 from a muttrc

**1.11 tagged and released** (2026-07) after R12; the whole gap-review
roadmap (R8–R12) is shipped. The milestones below are the former
unscoped candidates, ranked; none is committed until picked up.

**1.13 tagged and released** (2026-07) after R14 (macros + OAuth2).

**1.14 tagged and released** (2026-07) after R15 (mbox).

**1.16 tagged and released** (2026-07) after R17 (index polish +
sidebar). R18–R22 below are the second round of nice-to-haves, ranked;
daily-use findings still outrank them all.

## R13: macros (done, 1.12)

The last thing the importer routinely skipped: `macro` lines.

- [x] Config: `[macros.index]` / `[macros.pager]`, `key = "sequence"`,
      a key that replays a sequence of keys (mutt semantics:
      macros feed the input queue, so they can drive prompts); a
      macro shadows a binding on the same key; `?` lists them
- [x] Replay engine: queued key events run ahead of real input, a
      macro fired mid-replay expands in place; a queue cap breaks
      self-referencing macros
- [x] Key-sequence syntax: literal characters plus the existing key
      names in angle brackets (`<enter>`, `<esc>`, `<ctrl+x>`...)
- [x] Importer: `macro index/pager KEY "sequence"` translates when
      the sequence is plain keys/prompt input (`\n`/`\t`/`\e`/`\Cx`
      escapes converted); mutt function names inside stay
      `# not imported`

## R14: OAuth2 (done, 1.13)

For Gmail/O365 accounts, where app passwords are dying out.

- [x] SASL XOAUTH2 and OAUTHBEARER (RFC 7628) for IMAP AUTHENTICATE
      and SMTP AUTH; no SASL-IR, so any server works; a failure
      challenge is answered with an empty line to surface the NO
- [x] Account config: `auth = "xoauth2"/"oauthbearer"` +
      `token_command` (like password_command, prints an access token;
      refresh is the external tool's business: oauth2ms,
      mutt_oauth2.py). Tokens expire, so it runs per connection,
      never cached
- [x] Importer: `imap_authenticators`/`smtp_authenticators` naming
      oauthbearer/xoauth2 become `auth` + a token_command placeholder;
      a stored imap_pass is then dropped (redacted note) instead of
      emitted alongside

## R15: mbox (done, 1.14)

- [x] Open `/var/mail/$USER` and friends: From_-line splitting with
      mboxrd `>From` unquoting, mirrored into a cache maildir (like
      IMAP) so the index/pager work unchanged; messages keyed by
      content hash, so cache flags survive re-mirrors; new deliveries
      picked up by the poll
- [x] Write support: `$` sync rewrites the file in place under an
      exclusive flock: purged messages dropped, Status:/X-Status:
      rewritten (RO/AF), quoting preserved; refuses when the spool
      changed since the mirror (refresh with G, sync again)

## R16: %l and %L index polish (done, 1.15)

Small, closed the two documented index_format gaps.

- [x] `%l`: body line count, counted on envelope parse and cached;
      header-only IMAP cache files show the `%?l?…&…?` else branch
      until the body is fetched (and update on first view)
- [x] `%L`: list-name detection via List-Id, "To <name>" (the header
      display name, else the id's first label) instead of the author
      for mailing-list traffic; falls back to %F
- [x] Importer: imported index_format strings no longer carry the
      "%l is never set" caveat

## R17: sidebar (done, 1.16)

- [x] Optional left pane listing `mail.mailboxes` with new/unseen
      counts (the open mailbox always included, marked `>`); `B`
      toggles at runtime, `[sidebar]` config (visible, width); bold
      when a mailbox holds new mail
- [x] Ctrl+N/Ctrl+P move the highlight, Ctrl+O opens it without
      leaving the index (all remappable); the toggle state survives
      mailbox switches
- [x] R11's counting reused (local new/ scan, IMAP STATUS for the
      open account's folders), refreshed by the regular poll

## R18: notmuch search (done, 1.28)

Shell out to notmuch(1), mutt-kz style, with no linking and no new deps.

- [x] `X` prompts for a query and runs
      `notmuch search --output=files --limit=1000`; the hits open as
      a virtual read-only mailbox (symlinks into a cache maildir,
      like the mbox mirror); `[mail] notmuch = false` disables the
      key, otherwise notmuch just has to be installed
- [x] Message operations that make sense there work: view, reply,
      copy, pipe; flag changes and deletes refuse (the R26 read-only
      machinery, the real copies live elsewhere), and leaving the
      view restores a writable mailbox
- [x] Setup documented in the man page: notmuch new in a hook or
      cron, rmut for reading; the maildir scan follows symlinks now

## R19: daily-driver hardening (done, 1.17)

Robustness debts that only show up in long sessions and big mailboxes.

- [x] IMAP reconnect: a dead socket no longer poisons the session:
      every operation reconnects + re-SELECTs transparently once and
      retries (all wrapped ops are idempotent); a changed UIDVALIDITY
      refuses with "reopen the mailbox". The IDLE watcher now truly
      respawns dropped sessions (60 s pause; only "no IDLE support"
      gives up)
- [x] Incremental refresh: NOOP responses are classified; arrivals
      alone fetch `UID FETCH <last>:*` (the N:* returns-last quirk
      filtered); flag changes/expunges still reconcile fully
- [x] Header cache: parsed envelopes persist under
      $XDG_CACHE_HOME/rmut/headers keyed by base name + length, so
      reopening a big maildir parses only new files; flag renames
      keep the key, a partial IMAP file gaining its body re-parses
- [x] mbox safety: the spool is copied to `<cache>/.backup` before
      the in-place rewrite, removed on success

## R20: compose and message-flow niceties (done, 1.18)

- [x] Review attachments at the send prompt: `v` lists the draft's
      `Attach:` files (name, size, type, description; the forwarded
      original too) before committing to y
- [x] `a` at the send prompt appends an `Attach:` line without
      re-opening the editor (missing files rejected, spaces quoted)
- [x] Create-alias: `a` on the index/pager appends
      `alias <nick> <From>` to the alias file, nick prompted with the
      address's local part prefilled
- [x] Trash (mutt's $trash): `[mail] trash = "..."` makes the purge
      move messages there first (maildir delivery locally, UID COPY
      on IMAP) and aborts if that fails; purging inside the trash
      deletes for real; imported from `set trash`
- [x] Postponed picker: with several postponed drafts, recalling
      offers a date+subject list (newest first) instead of silently
      taking the newest

## R21: color patterns and status format (done, 1.19)

Patterns v2 made mutt's coloring model implementable.

- [x] `[[color_index]]` rules: `pattern` + `fg`/`bg`, evaluated with
      the R10 engine per index line; first match wins over the slot
      colors, broken patterns/colors warn and drop
- [x] Importer: `color index FG BG PATTERN` lines translate for any
      pattern the engine parses (~D/~F/~T keep their slots, "default"
      colors are dropped)
- [x] `[ui] status_format`: mutt-style bottom line: %f %m %M %n %u
      %d %F %t %s %V %r %v with conditionals, sharing the index
      format's renderer (`render_with`); the default reproduces the
      classic layout exactly; imported from `set status_format`
- [x] `%M` collapsed-thread count in index_format; the "(n hidden)"
      subject suffix steps aside when the format places %M itself

## R22: distribution (done, 1.20)

Make it installable without a checkout.

- [x] CI: GitHub Actions running `just check` (fmt, clippy -D,
      tests, e2e) on push/PR
- [ ] Publish rmut-core, rmut-session and rmut-tui to crates.io
      (`cargo install rmut-tui`): metadata ready and
      `cargo package` verified; `just publish` after `cargo login`,
      in dependency order (core, session, tui)
- [x] A man page (rmut.1, hand-rolled) covering keys, config,
      patterns, formats, and the muttrc importer; `--help` stays the
      short form
- [x] Release automation: tag `vX.Y.Z` → GitHub release with a
      prebuilt x86_64-linux binary + man page tarball

**Mutt-parity round** (2026-07, after 1.20): defaults and flows
aligned with mutt: edit_headers (off), mutt's index_format and
date, $resolve advance, first-new positioning, forward_format,
$mark_old aging, pager + markers and $pager_stop, the ask-yes
compose questions (Reply-To / include / no-subject), e edits the raw
message (Alt+e resends), and the compose menu (with Enter viewing
the selected entry). R23–R26 below are the UX gaps left from that
audit, ranked; daily-use findings still outrank them all.

## R23: prompt line editor (done, 1.22)

Mutt's prompts are a real line editor; rmut's only appended.

- [x] Cursor movement and mid-line editing at every prompt:
      left/right, Home/End (and ctrl+a/ctrl+e), insert and delete at
      the cursor (Del/ctrl+d), ctrl+u to start, ctrl+k to end,
      ctrl+w word delete, the cursor marker drawn where it is
- [x] History per prompt kind (patterns, addresses, mailboxes,
      files, commands, subjects): Up/Down recalls newest first,
      stepping back restores the line being typed; 100 entries,
      in-memory for the session

## R24: pager body search (done, 1.23)

- [x] `/` inside the pager searches the displayed text
      (case-insensitive, regex like patterns), scrolls the hit to the
      top line (even past the usual bottom, like mutt); `n` repeats,
      `N` reverses, both wrap with a status note; remappable pager
      actions (search/search-next/search-prev), the importer maps
      mutt's search/search-next/search-opposite
- [x] The index `/` stays message-level; the `?` help and the man
      page note the difference; the pattern shares the index search's
      prompt history

## R25: triage keys (done, 1.24)

- [x] Tab / Alt+Tab in the index: jump to the next / previous
      new-or-unread message, wrapping with a status note (mutt's
      next-new-then-unread); remappable (next-new / previous-new),
      the importer maps mutt's next-new-then-unread family
- [x] Pattern-wide operations: `D` delete-pattern, `U`
      undelete-pattern, `T` tag-pattern, ctrl+t untag-pattern:
      prompt for a pattern, apply to every match within the active
      limit (folded-away thread members included), report the count

## R26: browser, attachments, and odds (done, 1.25)

- [x] Folder browser: `c` browses any directory, Enter descends into
      plain ones (`..` up) and opens maildirs, `C` creates a maildir
      (relative to the browsed directory)
- [x] Attachment menu: `|` pipes the decoded part to a command, `p`
      prints it via the print command (confirmed)
- [x] Read-only mode (`rmut -R`): nothing written, not even read
      marks; mutating keys refuse with a note, `%r` shows `%`
- [x] Query menu: `Q` prompts, lists query_command results, Enter
      composes to the pick (To prefilled, normal chain after)
- [x] status_format `%>X` right-alignment and `%P` position
      (all/top/bot/NN%), so mutt's exact default status line
      renders; a status message shortens a full-width line instead
      of falling off the edge

**Third round** (2026-07, after 1.25): the parity audit's ranked list
is empty, so R27–R30 below collect what daily use and a fresh look at
mutt still surface: the pager's missing colors, header weeding,
compose paper cuts, and big-mailbox speed. Ranked; R18 (notmuch)
slotted in after R28 and shipped with 1.28; daily-use findings still
outrank everything.

## R27: pager colors and motion (done, 1.26)

The last big *visual* gap: rmut's pager was monochrome where mutt
tints quotes, hits, and URLs.

- [x] Quoted-line coloring: mutt's $quote_regexp default
      (`^([ \t]*[|>:}#])+`) classifies quoted lines, nesting depth
      cycles the `[colors]` quoted/quoted1… palette (default cyan);
      `[pager] quote_regexp` overrides, importer takes
      `color quotedN` and `set quote_regexp`
- [x] `[[color_body]]` rules: regex + fg/bg applied to matching body
      spans in the pager (URLs, diff lines); imported from
      `color body FG BG REGEX`
- [x] Search-hit highlighting: the R24 pager search paints every
      match (search_fg/search_bg, reverse video by default,
      imported from `color search`)
- [x] Motion: ctrl+d/ctrl+u half-page, `T` hides quoted lines,
      `S` skips past the quoted block below (all remappable, the
      importer maps half-down/half-up/toggle-quoted/skip-quoted)

## R28: header weeding and pager polish (done, 1.27)

- [x] ignore/unignore lists drive the brief header view (prefix
      matching, `*` = all; the default keeps Date/From/To/Cc/
      Subject), `hdr_order` sorts it; `[pager]` ignore/unignore/
      hdr_order in TOML, all three imported from muttrc
- [x] `[pager] format`: mutt's $pager_format renders the pager's
      bottom line (%C %m %n %s %Z %P %f; the default keeps the
      classic "---Message n/m"), sharing the status renderer and its
      %>/%P machinery; imported from `set pager_format`
- [x] `$wrap`: wrap at N columns instead of the full width (negative
      N = right margin), rows/search/scroll math all honor it;
      imported
- [x] `~` tilde padding below end-of-message (`[pager] tilde`,
      mutt's $tilde; off by default like mutt); imported

## R29: compose round 2 (done, 1.29)

- [x] `$fast_reply` (replies skip the To/Subject prompts, forwards
      the Subject; the ask-yes questions still run) and `$autoedit`
      (with edit_headers: no prompts or questions at all, straight
      into the editor, compose menu after), both imported
- [x] Compose menu: `d` edits an attachment's description, ctrl+t
      its content-type (mutt's edit-type); both live on the Attach:
      line as `Attach: path [type/subtype] [description]` and reach
      the sent MIME part
- [x] `mime_forward = "ask"` (`[mail] forward = "ask"`): the forward
      flow asks "Forward as attachment?" (yes = attach whole, no =
      inline quote); mutt's ask-yes/ask-no import; $forward_decode
      is inherent (inline quotes are always decoded)
- [x] `f` edits Fcc in the compose menu (a maildir path; empty =
      keep no copy); `$copy = no` (imported as `[mail] copy =
      false`) skips the sent copy by default, an explicit Fcc wins

## R30: big mailboxes (done, 1.30)

- [x] IMAP server-side search: plain-substring `~b` terms in
      limit/search/pattern-ops run as UID SEARCH on the open folder
      instead of fetching every body; regex/non-ASCII terms and any
      server failure fall back to local matching
- [x] Progressive open: a folder with more than 500 unmirrored
      messages fetches the newest 500 synchronously and backfills
      the tail on a background connection; the poll rescan
      integrates the files as they land (the count climbs), and the
      server check pauses meanwhile so nothing double-fetches
- [x] `new_mail_command`: shell hook on arrivals in the open or
      watched mailboxes (`%f` mailbox, `%n` count), fire-and-forget;
      imported from neomutt's option
- [x] Header-cache hygiene: entry pruning and compaction were
      already inherent in the rewrite; what lingered were cache
      files of vanished maildirs, which a weekly sweep (marker-limited)
      drops, keyed by the mailbox path now stored in each file

## Proposed rounds (2026-07, from the mutt-parity gap analysis)

Ranked by how soon a mutt veteran trips over the gap; none committed
until picked up, and daily-use paper cuts still outrank all of them.

## R31: patterns v3 (done, 1.32)

- [x] `~h` any header (the decoded header block, read from disk like
      `~e`), `~i` Message-ID, and `~x` References. The plan said `~x`
      already existed; it did not, so it shipped here
- [x] `~r` received date, sharing the `~d` spec vocabulary (ranges and
      `<`/`>`/`=` offsets) but reading the file's delivery time, with
      the Date header standing in when the file cannot be stat'ed
- [x] `~m` index ranges (`~m 10-20`, `~m 5-`, `~m -20`, `~m 7`) over
      the numbering on screen, with mutt's `.` (selected) and `$`
      (last) usable at either end
- [x] `~z` size ranges (`~z >100K`, `~z <2M`, `~z 1K-2M`, `~z 500`),
      K/M/G suffixes in mutt's powers of 1024
- [x] `~=` duplicate messages: the Message-ID occurs more than once
      in the mailbox
- [x] Everywhere patterns go: limit, search, pattern-ops, and
      color_index rules. `~m`/`~=` need the list around a message, so
      matching takes a `Position` (number, selected, last, duplicate);
      matching one message without a list leaves both false.
      Identities match on folder/recipient globs, not patterns, so
      there was nothing to extend there

## R32: enter-command (done, 1.31)

- [x] `:` in the index and the pager opens a command prompt (the R23
      line editor, with its own history bucket) taking one config
      line for the session; the config file is never rewritten
- [x] `set`/`unset`/`toggle` over the runtime-changeable options in
      mutt's spelling, including `set nofoo`, `set invfoo`, several
      assignments per line, and `set foo?` reporting the value
- [x] `bind` and `macro` against the live key tables, taking mutt key
      spellings (`\Cd`, `<esc>`) and mutt function names
      (`delete-message`) as well as rmut's own action names;
      `index`, `pager`, `generic`, and comma-joined menu lists
- [x] `color` into the same slots the importer uses (named slots, the
      quoted palette, `color index PATTERN`, `color body REGEX`),
      `ignore`/`unignore`, and `alias` appending to the alias file
- [x] `push` feeds the input queue, `exec` runs one function straight
      away in the menu on screen; every failure reports in the error
      style and stops the line
- [x] Changed config recompiles in place: theme, key tables, color
      rules, quote regexp, header rules, and a resort when the sort
      order moved. `command::parse` + `command::apply` are the halves
      a folder-hook with arbitrary commands would reuse

## R33: batch and CLI round (done, 1.33)

- [x] `rmut -s subj [-c cc] [-b bcc] [-a file]... [-i file] -- addr`
      sends with no terminal work at all: identity from `[identity]`
      plus any recipient-matching `[[identities]]` rule, transport
      from `mail.sendmail` or the first SMTP account, body from stdin
      (or `-i`), Fcc into a local `mail.sent`, and an exit code a
      script can read. `-s`/`-c`/`-b`/`-a`/`-i` or a bare `--` is
      what puts rmut in send mode
- [x] `mailto:` argument (RFC 6068: to, cc, bcc, subject, body,
      percent-decoding, `+` for space) opens a prefilled draft with
      the mailbox still open behind it, so it postpones as usual; a
      crafted subject cannot inject headers
- [x] `-p` postponed picker, `-y` mailbox list, `-z`/`-Z` exit 1
      instead of starting when the mailbox is empty / has no new mail
- [x] `-e command` runs an enter-command line before the first draw,
      repeatable; in send mode the config half applies and the
      TUI-only commands are ignored. `-f MAILBOX` joins them, since a
      mutt hand reaches for it
- [x] Batch send keeps no IMAP Fcc: an APPEND needs the account
      opened, which is the interactive path's job. A local
      `mail.sent` maildir is used, and a remote one is skipped
      rather than silently dropped

## R34: mailing lists (done, 1.34)

- [x] `[mail] lists` and `[mail] subscribed`: address patterns (the
      pattern engine's case-insensitive regexes) naming known and
      subscribed lists, imported from mutt's `lists`, `subscribe`,
      `unlists` and `unsubscribe`
- [x] `~l`: addressed to a known list. Matching now takes a `Scope`
      (my addresses, the list patterns, the position) instead of a
      bare `me` slice, since `~p`, `~l`, `~m` and `~=` all need
      something the message alone cannot answer
- [x] `L` list-reply in index and pager: the List-Post address when
      the list published one, else the first To/Cc address matching a
      configured list. With no list to reply to it refuses rather
      than quietly mailing the author, which is the mistake
      list-reply exists to prevent
- [x] Group replies honor a sender's Mail-Followup-To: it is the
      recipient set they asked for, so it replaces To and no Cc is
      added
- [x] $followup_to: mail to a known list carries a Mail-Followup-To.
      Subscribed leaves my own address out (the list copy is the one
      I get), not subscribed keeps it in
- [x] Fixed on the way: list replies did not ask mutt's "Include
      message in reply?" question that Reply and GroupReply ask,
      though they quoted the original anyway

## R35: alternates and my_hdr (done, 1.35)

Goal: rmut knows which addresses are "me" and what extra headers to
send.

- [x] `alternates` (config regex list + importer): reply-to-all
      dedup, the `+`/`T` index marks, reverse_name lookups. One
      `pattern::Me` (identity addresses plus the alternates regexes)
      now answers every "is this me?" question, so `~p`, the marks,
      reverse_name, the group-reply dedup and Mail-Followup-To all
      agree; `unalternates` in the importer and at the `:` prompt
      takes patterns back off
- [x] `my_hdr`/`unmy_hdr`: default headers merged into every compose
      (edit_headers shows them). The merge happens where a draft is
      staged, so replies, forwards, mailto: drafts and batch sends
      all carry them; an entry naming a header rmut already wrote
      replaces it (`From:`, `Reply-To:`), while To/Cc/Bcc gain the
      address instead of losing the old one, like mutt. One entry per
      header name: a later `my_hdr` for the same header wins
- [x] Added on the way: `~P` (mail I sent), mutt's `$metoo` (keep my
      address in a group reply), and the rest of mutt's `$to_chars`
      in the third `%Z` slot: `F` for mail I sent and `L` for mail to
      a subscribed list
- [x] Fixed on the way: a group reply put every original recipient in
      the Cc, my own addresses and the person already in To included,
      so replying to all mailed me a copy and the sender two; `+` now
      means mutt's sole recipient (one To and no Cc) rather than any
      single To address

## R36: hooks round 2 (done, 1.36)

Goal: the per-context hooks daily mutt configs actually use.

All four are message patterns (or a folder glob) plus a payload, and
the three that carry a command line reuse R32's `command::parse` +
`command::apply`, so a hook can say anything `:` can say.

- [x] fcc-hook / fcc-save-hook (pattern → Fcc mailbox):
      `[[fcc_hooks]]`, matched against the draft as it stands after
      the editor, so the compose menu's Fcc line already shows where
      the copy is going; an Fcc picked by hand still wins. Batch
      sends honour it too. `compose::draft_envelope` is what lets a
      pattern match outgoing mail, with Bcc folded into the Cc
      addresses so a hook on `~c` sees a blind recipient
- [x] message-hook (pattern → display-time settings) and reply-hook:
      `[[message_hooks]]` are in force while their message is the
      selected one and are taken back off the moment the match set
      changes, so a display setting really is per-message;
      `[[reply_hooks]]` apply while a reply's draft is built, which
      covers `set from`, edit_headers and my_hdr
- [x] crypt-hook: per-recipient PGP key selection, `[[crypt_hooks]]`
      mapping a recipient address regex to the key id gpg encrypts to
- [x] folder-hook running arbitrary commands (on top of R32):
      `[[folder_hooks]]` run their line when a matching mailbox
      opens, at startup as well as on a switch. Like mutt, nothing is
      undone on the way out, so a catch-all entry is how a setting
      goes back; a from/realname folder-hook still imports to an
      `[[identities]]` rule, where it layers properly
- [x] Added on the way: `~A` (every message), which mutt's
      `$default_hook` expansion needs, and that expansion itself, so
      a bare `fcc-hook boss@example.com` imports to what mutt means
      by it: `(~f "boss@example.com" !~P) | (~P ~C "boss@example.com")`

## R37: format=flowed (done, 1.37)

Goal: f=f both ways. RFC 3676 lives in `core::flowed`: `unflow` for
what comes in, `space_stuff` for what goes out.

- [x] display: reflow flowed paragraphs to the wrap width, keeping
      quote depth. A flowed part goes back to one logical line per
      paragraph and the pager's own wrapper lays it out, so `$wrap`
      and the window width govern as they do everywhere else. Quote
      depth bounds a paragraph and survives as `>` marks, so quoted
      text still colours and folds; space-stuffing is undone, DelSp
      is honoured, and `-- ` stays a fixed line (RFC 3676 4.3).
      mutt's `$reflow_text` turns it off
- [x] compose: optionally send text/plain; format=flowed
      ($text_flowed), space-stuffing included. One `compose::
      text_entity` now builds the text part everywhere it appears
      (plain, multipart/mixed, and inside the PGP layers), so the
      declaration and the stuffing cannot disagree; a draft going out
      with no MIME wrapper at all gets its Content-Type from
      `compose::flow_plain`. The paragraphs are the editor's doing,
      like mutt: rmut adds no trailing spaces of its own

## R38: MIME polish (done, 1.38)

Goal: the remaining display-selection knobs. What the pager needs to
turn a message into text now travels as one `message::Display`
(filters, header rules, $reflow_text, $alternative_order) instead of
a lengthening argument list.

- [x] alternative_order / unalternative_order (importer too),
      slotting into pick_alternative. MIME types most wanted first,
      `text/*` wildcards included, consulted before the auto_view
      filters and before the built-in text ranking, which still has
      the last word when a message carries none of the listed types.
      Both work at the `:` prompt
- [x] unauto_view; read ~/.mailcap for filters when [filters] has no
      entry (copiousoutput entries only). `core::mailcap` reads
      RFC 1524: continuation lines, escaped semicolons, `type/*`
      wildcards, `test=` (run, must succeed) and `needsterminal`
      (passed over, it cannot render into the pager), from $MAILCAPS
      or mutt's default list. A `%s` command gets a temporary file
      holding the part, since only stdin filters can be piped.
      Resolution happens where the config is compiled, so a type
      whose mailcap entry is missing simply does not autoview.
      `auto_view` and `unauto_view` work at the `:` prompt, and the
      importer no longer guesses a command for text/html: an
      auto_view type imports as an empty `[filters]` entry, which is
      exactly mutt's "the command is in my mailcap"
- [x] decrypted PGP/MIME entities render through the full tree
      (attachments inside encrypted mail), not just their first text
      part. `pgp::View` hands back either `Body::Text` (inline PGP,
      plain text) or `Body::Entity` (PGP/MIME, a MIME tree), and
      `message::render_entity` renders the tree the way the pager
      renders any message, so an attachment inside encrypted mail is
      announced like any other

## R39: undo (done, 1.39)

Goal: a way back from the last change to the messages, the first
"Beyond mutt" item and the one it ranked highest on value-per-effort.
Asked for by name; the rest of that list stays frozen.

- [x] `z` in the index walks back the last step: delete, undelete,
      flag, read toggle, tag, a `D`/`U`/`T`/Ctrl+T pattern sweep, a
      save or a copy, and `d` from the pager. One action is one step
      however many messages it touched, so a pattern delete over the
      whole mailbox comes back in one keystroke
- [x] A step keeps a snapshot per message it touched (flags, is_new,
      tagged, dirty) keyed by path, plus the files it created and
      where the cursor was, also by path so a resort cannot move it.
      Undoing restores the snapshot, removes a save's delivered copy,
      and puts the cursor back. A copy that went to an IMAP folder
      says so rather than pretending: APPEND is not taken back
- [x] Writing the mailbox ends what can be undone: `sync` drops the
      stack, since the marks are on disk and the paths the stack keyed
      on have been renamed away. Bounded at 32 steps and 100k
      snapshots, oldest out first, so a sweep over a huge mailbox
      cannot pin memory
- [x] No redo: the stack goes one way, which is what a "that was
      wrong" key is for; e2e scenario_undo

## R40: undo send (done, 1.40)

Goal: the second "Beyond mutt" item, asked for by name. Mail waits a
few seconds before it goes anywhere, and the same `z` takes it back.

- [x] `[mail] undo_send` (seconds, 0 = off, settable at the `:`
      prompt): a sent message is finalized as usual and then parked
      in an outbox instead of handed to sendmail/SMTP. The main
      loop's one-second tick delivers what is due and counts the rest
      down on the status line
- [x] Cancelling is not just "not sending": the held entry keeps the
      whole `Compose`, so `z` puts the compose menu back with the
      draft as it was, ready to edit and send again. A resend
      re-finalizes, so the Message-ID and Date are the new ones
- [x] `z` reaches a held send before the mark history, in both index
      and pager: the send is the most recent thing done, which is
      what a single undo key should take back first
- [x] The timer running out sends it, and so does leaving rmut:
      quitting is not cancelling. Trouble during that last send has
      no status line left to appear on, so it is printed to stderr
      once the terminal is back. Batch sends never hold, having no
      terminal to press `z` at; e2e scenario_undo_send

## Proposed rounds (2026-08, from the ergonomics review against mutt)

Where the daily feel still differs from mutt, worst first. The first
round leads because those are the two places where a mutt hand's keys
do something other than what they mean, rather than nothing at all.

## R41: tagged and pager parity (done, 1.42)

Goal: `;` and the pager stop lying about what they did.

- [x] `;` reaches every function that can take a set: save, copy,
      pipe, print, bounce. The prefix is remembered across the prompt
      in `tag_op`, cleared at the top of every index action so an
      abandoned prompt cannot leak into the next one, and `op_targets`
      is what the operations iterate. Save and copy do one prompt, one
      undo step (all the marks and all the delivered files in it), and
      keep going when one message fails rather than stranding the
      copies already made; pipe and print concatenate into a single
      run of the command, which is mutt with $pipe_split unset; bounce
      sends each to the same addresses, and the y/n counts what it is
      about to do. Not resend or edit: those open a draft or an
      editor, of which rmut has one at a time
- [x] An action that cannot take the prefix says so ("resend takes
      one message, not the tagged set"), rather than quietly acting on
      the current message
- [x] The pager gains `u`, `F` and `t` (undelete, flag, tag), which
      mutt's pager has and rmut's lacked: it could delete and nothing
      else, though the pager is where triage happens. toggle-new is
      there as a function but unbound, since `N` is rmut's backwards
      pager search; `:bind pager KEY toggle-new` gives it a key
- [x] Both go on R39's undo stack, one step per keystroke, like their
      index twins; e2e scenario_tagged_and_pager

## R42: thread operations (done, 1.43, minus the postponed box)

Goal: act on the thread, not the message. `thread_root` and
`thread_depth` are already maintained and folding sits on Alt+v /
Alt+V (mutt's `Esc v`), so half the map is drawn.

- [x] Alt+d / Alt+u delete and undelete the whole thread, Alt+t tags
      it (mutt's `Esc d` / `Esc u` / `Esc t`: a terminal sends
      `Esc x` as Alt+x, so the keys match by construction). Tagging
      follows the cursor's own tag, like mutt, so a second Alt+t
      untags; deleting advances to the next undeleted message
      ($resolve), which is what makes clearing thread after thread
      one repeated key, while a rescue stays where it is
- [x] Ctrl+D / Ctrl+U for the subthread, the selected message and its
      descendants (mutt's delete-subthread / undelete-subthread).
      Thread sort lays the messages out depth-first, so the replies
      are the run after the message that stays deeper than it
- [x] Alt+n / Alt+p jump to the next / previous thread root, over the
      same `wrap_order` walk as every other motion
- [x] One undo step per thread operation, however many messages it
      touched, which is what makes "delete this thread" safe to try.
      All of them refuse without thread sort, as mutt does;
      e2e scenario_thread_ops, which also needed a reply-to-the-reply
      fixture so a subthread is not the whole thread
- [ ] Postponed from R45, only if the macros do not carry it: promote
      next/previous-marked to real motions with a key of their own,
      rather than `/~D` and Alt+/ driven from `[macros.index]`. What
      a key would buy over the macro is not typing the pattern and
      not clobbering `last_search`; what it costs is deciding what
      "marked" means, and it should mean deleted only, not
      `Msg::pending()`, or the motion lands on messages nobody wants
      to revisit

## R43: the = and + folder shorthand (done, 1.44)

Goal: mutt's $folder-relative mailbox names, everywhere a mailbox is
named.

- [x] `[mail] folder`, imported from `set folder`: the importer read
      it already, but only to expand +/= at import time, and now
      carries it over so the running rmut knows it too. An account
      spec works as the folder ("imap:work"), so `=Archive` is that
      account's folder
- [x] `config::expand_folder` is the one expansion, reached two ways:
      `Config::expand_folders` rewrites every mailbox the config
      names (mailboxes, sent, postponed, trash, save, fcc-hook
      targets) once at load, after the `-e` commands and after any
      `:set`, and `run_line_prompt` expands what was typed at the
      mailbox prompts before anything reads it. Everything
      downstream keeps seeing plain paths and imap: specs, so
      `expand_tilde` and `parse_spec` did not have to learn anything
- [x] It also unbreaks imported macros: a macro's keys go through the
      same prompt, so `macro index S "<save-message>=archive<enter>"`
      now files where it means to instead of into a maildir named,
      literally, `=archive`
- [x] Known limit, matching where the work stops: Tab completion at a
      mailbox prompt still completes real paths, not `=` names;
      e2e scenario_folder_shorthand

## R44: paper cuts (done, 1.45)

Goal: small mutt keys rmut does not have. Any of these can ride along
with an earlier round instead of waiting for its own.

- [x] Space pages down in the index (it already does in the pager;
      in mutt it does in every menu)
- [x] Ctrl+L redraws, for when a filter or an editor scribbles on the
      terminal and nothing repaints it. It is an action in the index
      and pager keymaps, so it shows in `?` and can be rebound, plus
      a fallback in `handle_key` for the menus that have no keymap of
      their own (compose, attachments, browser, help)
- [x] `!` runs a shell command with the TUI stood down, the same
      terminal handoff the editor uses, and waits for Enter before
      painting over whatever it printed
- [x] `Alt+c` opens a mailbox read-only (mutt's `Esc c`; `-R` stays
      the session-wide version). Fixed on the way: `-R` was lost on
      any mailbox switch, since `open_mailbox_spec` replaces the App
      wholesale, so a read-only session quietly became writable. The
      session flag now rides across the switch and the per-mailbox
      one does not
- [x] Not a one-liner, kept here for company: Ctrl+Z suspends (mutt's
      $suspend, on by default), which needs SIGTSTP with the terminal
      saved and restored around it. The stop itself cannot be
      exercised by the e2e suite: `pty.fork` makes rmut a session
      leader, so its process group is orphaned and the kernel
      discards stop signals. The scenario covers the handoff around
      it, which is the part that can wreck a session;
      e2e scenario_paper_cuts

## R45: search direction (done, 1.41)

Goal: taken out of R44 early, because backwards was the missing half
of stepping through the messages marked for deletion.

- [x] `Alt+/` searches backwards (mutt's `Esc /`, search-reverse) and
      `n` then repeats backwards too: the direction is remembered
      with the pattern, so one key covers both ways
- [x] `search_next` now walks `wrap_order`, the same helper
      `jump_new` uses, instead of its own forward-only loop: one
      tested walk under every motion
- [x] No new motion key for "next marked message": `/~D` then `n`
      already is one, and `[macros.index]` turns it into `.` and `,`
      (`"." = "/~D<enter>"`, `"," = "<alt+/>~D<enter>"`). Documented
      in the README and the man page, next to the rest of the answer
      to "rescue one message out of a run of deletions": deleted
      messages stay in the index so `j`/`k` reach them, `U PATTERN`
      undeletes a set, `l ~D` limits to the marked ones
- [x] e2e scenario_search_direction, with three of four messages
      marked so forward and backward land on different ones and the
      test can actually tell the directions apart

## R46: the attachment reminder (done, 1.46)

Goal: the third "Beyond mutt" item, asked for by name. A draft that
talks about an attachment and carries none gets questioned before it
goes. neomutt has this one, so it takes neomutt's names and imports
from a muttrc.

- [x] `[mail] abort_noattach`: "no" (default) never looks, "ask"
      questions the send, "yes" refuses it. neomutt's quadoption
      ask-yes and ask-no both mean "ask" here, since rmut's y/n
      prompts have one shape. Answering no returns to the compose
      menu, where `a` attaches the file that was forgotten; the
      answer is remembered for that draft, so a second y sends
- [x] `[mail] attach_keyword` ($abort_noattach_regex), compiled with
      the other Derived regexes and warning like them when it does
      not compile. mutt's `\<` / `\>` word edges are translated to
      `\b` on import, so the common imported value works instead of
      warning at every start
- [x] Quoted lines (the same $quote_regexp the pager uses) and
      anything below a `-- ` signature do not count: a reply to
      someone else's "see attached" and a signature advertising an
      attachment opener are the two false alarms worth ruling out
- [x] The check sits at the top of `send_draft`, before anything is
      finalized, so it also covers a draft going out with $undo_send
      holding it. Batch sends never ask, having no terminal to answer
      at; e2e scenario_attach_reminder

## R47: getting started (done, 1.47)

Goal: two rough edges hit on a fresh machine, reported from real use.
rmut could not find a mailbox that mutt would have found, and
`--import-muttrc` printed a config to the terminal and wrote nothing,
which reads as "it did not work".

- [x] The startup search looks where mutt looks: configured
      mailboxes, `[mail] folder`, `$MAIL` (a maildir or, as it
      classically is, an mbox file), `~/Maildir`, `~/Mail`, `~/mail`,
      then the `/var/mail/$USER` and `/var/spool/mail/$USER` spools.
      A directory of maildirs answers with its inbox, since that is
      what a mutt-style `~/Mail` usually is
- [x] Finding nothing says what to do (give a path, set
      `[mail] mailboxes`, point `$MAIL`), lists the places it tried,
      and shows the mkdir that makes a maildir. `find_default_mailbox`
      takes the environment as an argument, so a unit test can point
      it at a directory of its own
- [x] `--import-muttrc -w` writes the config instead of printing it,
      creating `~/.config/rmut/` and refusing to overwrite what is
      there. Printing at a terminal now says so too, since the output
      scrolls past and nothing was saved
- [x] Running without a terminal says so instead of panicking inside
      ratatui's init
- [x] Fixed on the way: the e2e `repaint()` helper raced the keys it
      followed, so a resize could repaint the screen as it was before
      they were read. Two scenarios were failing at random; it settles
      first now. e2e scenario_getting_started

## R48: the message line (done, 1.48)

Goal: mutt's four regions, not three. rmut had the help bar, the
content and one bottom line doing two jobs, which is why a note
truncated the status bar to fit, R40's send countdown competed with
ordinary notes, and `Tag-` had to be appended to the mailbox line
instead of sitting under it. Reported from use, and right.

- [x] The layout gains a message line under the status bar, always
      there, empty when there is nothing to say. `draw_bottom_line`
      splits into `draw_status_bar` (the format line, unconditional)
      and `draw_message_line` (the prompt, or the note, or the error
      in its colour). The status bar now always says which mailbox
      you are in, whatever else is happening
- [x] A page is three rows shorter than the window, not two
- [x] Fixed on the way: the e2e suite scrapes the screen, and a
      message on a line of its own is a worse case for ratatui's
      cell-diff than one appended to a changing bar, so a dozen
      scenarios started reading text with holes in it. `expect` now
      forces a redraw (an ioctl toggling the width) when a needle has
      not turned up after 0.2s, and again every 0.3s while it waits,
      which fixes the whole class instead of scattering `repaint()`
      calls. No redraw before that: one sent immediately would
      overtake the keys just typed and repaint the screen as it was
      before they were read

## R49: credentials (done, 1.49)

Goal: stop the import from leaving a password where anyone can read
it, and say plainly where a secret should live instead. Found in use:
an imported config sat at mode 664 in ~/.config with an IMAP password
in it.

- [x] `--import-muttrc -w` writes the config and the alias file with
      `create_new` at mode 600, rather than letting the umask decide
      what an imported `set imap_pass` is worth
- [x] A config that holds a plaintext `password` and is readable by
      group or other says so at startup, with the imperative first
      ("chmod 600 <path> ...") because the message line clips at the
      window edge and the path is the long part. Quiet at 600, and
      quiet when the account uses `password_command`
- [x] README and man page grew a Credentials section: what
      `password_command` is for, that it runs once per session and
      fails closed, and four one-liners that cover the usual stores
      (pass, a gpg file, libsecret, an encrypted volume);
      e2e scenario_password_permissions

## The session split (proposed, 2026-08)

Goal: rmut's logic in a library, with the TUI as one front end over
it. The point is testability first: a GUI becomes possible, but it is
not what pays for the work.

Where the line sits today, counted rather than guessed.
`rmut-core` (14k lines) is already UI-free: maildir, mbox, IMAP,
SMTP, compose, PGP, patterns, threading, the importer, mailcap,
flowed. `rmut-tui` is 9.5k, 6.7k of it `app.rs`, whose methods split:

    really UI (modes, prompts, keys, drawing)   59 methods  2572 lines
    model, plus a status message                46 methods  1371 lines
    pure model                                  77 methods  1072 lines

So about 2.4k lines are application logic wearing a TUI coat. The
middle row is the lever: `sync`, `copy_message`, `undo_last`,
`thread_mark`, `open_mailbox_spec`, `deliver` touch the UI only
through `self.status = Some(...)`, which is a notification, not
rendering.

Why it is worth doing on its own: the e2e suite drives a pty and
scrapes the screen, which has cost real time in cell-diff holes,
resize races, and assertions that could not tell two outcomes apart
(R42, R48). Logic tested against a session returns values instead,
leaving the pty suite for what it is good at, the keymap and the
drawing.

The risk, recorded so the decision is made with open eyes: two front
ends double every round. Each feature either lands twice or the GUI
lags, and a lagging front end is worse than none. Nothing here
commits to a GUI; it makes one possible.

Each round below is shippable on its own and must leave behaviour and
the suite unchanged.

## R50: notices instead of a status field (done, 1.50)

Goal: the enabling step, and mechanical.

- [x] `notice::Notice` in rmut-core, emitted through a `NoticeSink`
      the front end installs. Every `self.status = Some(...)` (52) and
      every `error_status` call (110) now goes through `note`, `error`
      or `notify`, so an operation says what happened instead of
      writing into a screen field
- [x] The TUI installs `MessageLine`, a sink keeping the last notice,
      and draws its `text()` in its `is_error()` colour, so what the
      user sees does not change. The bell stays with the front end,
      since `set beep` can change mid-session
- [x] `Synced { deleted, updated }` is the first typed variant, for
      the one outcome a test would otherwise have to parse a sentence
      for; the rest stay `Info`/`Error` until something needs
      otherwise. `notice::Log` is the sink a test will install
- [x] No crate moves. Behaviour and the suite are unchanged: 47/47
      scenarios, no new one, because nothing new is visible

## R51: the session crate (done, 1.51)

Goal: the model half moves out.

- [x] `rmut-session` holds the open mailbox (msgs, visible, sel,
      sort, limit, threads, collapsed), the operations (marks, tagged
      and thread ops, sync, save/copy/pipe/print/bounce), the undo
      stack, the outbox, the hooks and the derived config (the
      matchers, the hook tables, `message::Display`, $quote_regexp,
      $abort_noattach_regex). The theme, the key tables and the
      colour rules stay derived in the TUI, being about a screen
- [x] `rmut-tui` keeps `Mode`, prompts, the keymap, the theme and
      `ui.rs`, and drives the session. app.rs went from 6716 lines to
      4204; the session crate is 2843
- [x] One area at a time, five commits, each with the suite green:
      the state and the marks and sync, then the operations on
      messages, then the hooks, then the mailbox switch, then compose
- [x] Where an operation ended by putting something on the screen it
      hands the front end a value instead: a failed send returns its
      draft, a cancelled one the same, `check_new_mail` takes a
      callback for the sidebar refresh, `op_targets` takes the tag
      prefix's answer. Switching mailboxes stopped rebuilding the
      whole app, which also stopped it dropping the prompt history
      and any held send
- [x] Left for R52, because they end in a question rather than a
      value: `send_draft` (the attachment reminder, the PGP and Fcc
      questions), `run_command_line` and the message-hook sync it
      drives, and the compose menu's own editing. R52 took all of
      them

## R52: asks, not prompts (done, 1.52)

Goal: the part that actually makes a second front end possible.

- [x] `Ask` and `AskKind`: the session says what it needs ("Save to
      mailbox: ", "Purge 3 deleted message(s)? (y/n): ") and the
      front end answers with the `AskKind` it came with, which can
      lead to the next question. The TUI draws them on its message
      line; a test answers straight away. The index side went first
      (limit, search, the pattern marks, save/copy, pipe, print,
      bounce, create-alias, sort, purge), then the config commands
      and the hooks that run them, then the compose flow, then the
      draft and the send
- [x] `Request`, the other way: `Quit`, `ConfigChanged`, `Command`
      (a `bind` the session cannot do), `ShowDraft`, `Editor`,
      `Shell`, `Suspend`. mutt's `!` is an ask ending in a request,
      and a front end that cannot suspend simply does not honour that
      one
- [x] `Mode` stays in the TUI: which menu is on screen is not the
      session's business, and neither is which row the cursor is on
- [x] What is left in app.rs is the front end: keys, menus, drawing,
      the pager, the attachment menu, the folder browser. 3021 lines,
      from 6716 when R50 started; the session crate is 4501 across
      four files

## R53: tests where the logic is (done, 1.53)

Goal: collect the winnings.

- [x] 35 session tests in `rmut-session/src/tests.rs`, over a
      fixture that is a maildir in a tempdir and a session with a
      `notice::Log` installed: undo (delete, pattern, tagged, a save's
      copy, and out of reach after a sync), tagged operations, thread
      and subthread ops with folding, limits and the pattern terms
      that need a whole mailbox (~h, ~i, ~x, ~=, ~z, ~m with . and
      $), sort, sync's numbers, the purge question and $delete,
      save/copy, read-only refusals, folder- and message-hooks,
      fcc-hooks, a command the session hands back, the compose flow
      (with $fast_reply and $abort_nosubject), the attachment
      reminder, and bounce. They run in 0.01s
- [x] The pty suite keeps what it is for: keys, drawing, the
      importer, the batch CLI, and the end-to-end paths through
      sendmail, IMAP and gpg
- [x] Two scenarios lost the assertions the session tests now make
      better (the pattern terms, and half of undo's steps), keeping
      their key paths. The other named ones stayed as they were:
      thread ops is a keymap test (Alt+d, Ctrl+D, Alt+t, Alt+n/p) as
      much as a logic one, and contorting it would lose that
- [x] Recorded honestly: the pty suite went from about 75s to 72s,
      because its cost is per-scenario startup and settling, not
      assertions. The winnings are precision and 0.01s feedback on
      the logic, not a faster suite

## R54: network timeouts (done, 1.54)

Goal: nothing freezes for minutes. Small, independent of the split,
and shippable whenever.

- [x] `connect_timeout` (10s by default) against each address the
      name resolves to, in place of `TcpStream::connect` waiting out
      the kernel's SYN retries. `[net] connect_timeout = 0` waits as
      long as the OS does, as mutt's does
- [x] Read and write timeouts are `[net] timeout`, 30s by default
      rather than 60, never off and never under five: IMAP IDLE ticks
      on it to notice a stop request, and now works out its own ~25
      minute window from it instead of assuming 60s ticks
- [x] A timeout says which host and what it was doing
      ("203.0.113.1:993 timed out after 2s", "imap.example.com:993
      timed out after 30s while reading"), and the io error stays in
      the chain so the retry and IDLE paths still recognise it
- [x] Both settable at runtime (`:set connect_timeout`,
      `:set net_timeout`), and `--import-muttrc` brings mutt's
      $connect_timeout over; e2e scenario_network_timeouts points at
      TEST-NET-3, which black-holes

## R55: network off the main thread (done, 1.55)

Goal: the TUI keeps drawing while the network is slow, and you can
give up. This is the round that was mis-filed as "only if a GUI is
wanted": the TUI is what pays for it today.

- [x] The connection lives on a thread of its own behind a channel:
      the session sends a `Job`, the thread runs it against the
      `Remote`, and the answer comes back as a `Done`. What the
      session needs cheaply (account, folder, cache) is `Facts`,
      beside the handle rather than down the channel
- [x] An operation that needs the network parks itself as a
      `Pending` and is carried on by `poll_network`, which the front
      end calls every time round its loop. Done for the poll tick,
      `$` sync, fetching bodies, and the sidebar's unread counts
- [x] Fetching a body needed no continuation per caller: `have_bodies`
      sends one fetch, parks the operation as `Again(...)`, and runs
      it from the top when the bodies land. Copy, pipe, print and
      bounce grew one guard each; opening a message became
      `Request::ShowMessage`
- [x] mutt's Ctrl+G: `net::Cutoff` shuts the socket from the front
      end's thread, and `Remote::retry` knows an abort from a dropped
      connection, so it does not helpfully re-run what the user gave
      up on. e2e scenario_network_abort, against a server told to
      dawdle four seconds
- [x] Progress is a notice: the message line says what is running
      from the moment it starts ("fetching the message... (Ctrl+G
      aborts)"), and the connection's own lines replace it
- [ ] Still waits for the server: opening a mailbox (the first
      connect and a switch), the folder browser's listing, a
      server-side `~b` search, and an append (saving to an IMAP
      folder, an Fcc). Each is one user-initiated action, bounded by
      R54's connect timeout; the machinery to move them is the same
      `Pending`, if they prove worth it

## Parity, second pass (proposed, 2026-08)

R1-R55 took rmut from nothing to a mutt replacement this machine runs
all day, and the roadmap is out of rounds. What is left is not guessed:
`--import-muttrc` was pointed at a muttrc of 90 widely used settings
(the ones that turn up in every dotfiles repo) and asked what it could
not carry. Sixty lines came back as "no rmut equivalent". Sorted by
what a mutt user would actually miss:

    the reply and forward text     attribution, indent_string,
                                   forward_format, include, askcc,
                                   askbcc
    settings rmut already has      sidebar_visible/width/format,
                                   alias_file
    settings rmut satisfies        imap_idle, header_cache,
                                   message_cachedir, ssl_force_tls,
                                   crypt_use_gpgme, mailcap_path
    reading habits                 pager_stop, markers, smart_wrap,
                                   collapse_unread, uncollapse_jump
    leaving and filing habits      quit, confirmappend, move + mbox,
                                   save_name, force_name, wait_key
    threading knobs                strict_threads, duplicate_threads,
                                   hide_missing, narrow_tree

Scoring stays a non-goal, and so do the rest of the Non-goals below.
Each round is small, shippable on its own, and testable off the pty
now that R53 gave us somewhere to test.

## R56: the reply and forward text

Goal: the three strings every mutt user changes. rmut hardcodes all
of them, in compose.rs: "On {date}, {from} wrote:", the `"> "` in
`quote`, and `"[{addr}: {subject}]"`.

- [ ] `[mail] attribution`, with mutt's specifiers (%a address, %n
      name, %s subject, %d date, %i message-id, %{strftime})
- [ ] `[mail] indent_string`, the quote prefix, default `"> "`
- [ ] `[mail] forward_format`, mutt's `[%a: %s]` by default
- [ ] `[mail] include`: yes / no / ask-yes / ask-no. Today the
      question is always asked, which is mutt's ask-yes; the other
      three are the point of the setting
- [ ] `[mail] ask_cc` / `ask_bcc`: mutt asks for them after To when
      set, and they are asks, which R52 made cheap
- [ ] `--import-muttrc` carries all six over; e2e for the three
      strings, session tests for the specifier expansion

## R57: the importer stops saying "no rmut equivalent" when there is one

Goal: the importer's promise is that nothing is dropped silently and
nothing is claimed falsely. Two dozen lines break it in both
directions.

- [ ] Map what rmut has and the importer forgot: `sidebar_visible`,
      `sidebar_width`, `sidebar_format` (as far as the format
      translates), `alias_file`
- [ ] Say "satisfied" rather than "skipped" for what rmut does by
      design: `imap_idle` (always on), `header_cache` and
      `message_cachedir` (rmut caches under ~/.cache/rmut),
      `ssl_force_tls` and `ssl_starttls` (rmut always upgrades),
      `crypt_use_gpgme`, `mailcap_path` (rmut reads MAILCAPS),
      `implicit_autoview`
- [ ] The report grows a third bucket if it needs one: satisfied,
      skipped, and "rmut does this differently, here is how"

## R58: reading habits

Goal: the pager and index options that change how it feels to read,
all of them one flag and a branch.

- [ ] `pager_stop`: Space at the end of a message stops rather than
      going to the next one
- [ ] `markers`: the `+` on wrapped continuation lines, on by default
      in mutt and not drawn by rmut at all
- [ ] `smart_wrap`: break at word boundaries (rmut already does this;
      the flag is to turn it off)
- [ ] `collapse_unread` / `uncollapse_jump`: whether a thread with
      unread mail folds, and where the cursor lands when one unfolds

## R59: leaving and filing habits

Goal: the questions mutt asks on the way out, and where mail lands.

- [ ] `quit`: yes / no / ask-yes. rmut's `q` leaves without asking
- [ ] `confirmappend`: ask before appending to an existing mailbox
- [ ] `move` + `mbox`: read mail moves out of the spool into $mbox
      when leaving, mutt's oldest habit
- [ ] `save_name` / `force_name`: the default save target follows the
      sender's address instead of $record

## R60: threading knobs

Goal: the four that change what a thread is.

- [ ] `strict_threads`: References/In-Reply-To only, no subject
      grouping (rmut's JWZ pass groups by subject as mutt does)
- [ ] `duplicate_threads`: whether same-Message-ID copies collapse
- [ ] `hide_missing` / `narrow_tree`: how the tree draws around
      messages that are not in the mailbox

## Beyond mutt (frozen: only on explicit request)

Ideas that exploit what mutt structurally can't do. Unlike the
proposed rounds above, these are NOT part of normal work: never pick
one up on your own, not even as a paper cut or "while I'm in there";
each starts only when explicitly asked for by name. Ranked by
value-per-effort:

- Undo: shipped as R39 above (asked for by name, 2026-08)
- Undo send: shipped as R40 above (asked for by name, 2026-08)
- text/calendar: render meeting invites (when/where/who) in the
  pager instead of a base64 blob; maybe accept/decline replies
- Attachment reminder: shipped as R46 above (asked for by name,
  2026-08)
- Clickable/yankable URLs: OSC 8 hyperlinks in the body, OSC 52
  clipboard yank via a URL picker key
- Inline image preview: kitty/sixel graphics for image parts in the
  pager
- Auto-harvested address completion: rank by who you actually mail,
  learned from the mail itself
- Markdown compose (opt-in): text/plain + generated text/html
  multipart/alternative
- Patch-series view: recognize a git series thread, show in order,
  pipe to git am
- Built-in full-text search: an incremental tantivy index making ~b
  instant without notmuch
- Unified inbox: several accounts' inboxes merged into one live
  virtual mailbox (flagship-sized; after the parity rounds)
- A GUI over the session library: only after R50-R53 (and R55, which
  it would need too), and only worth starting if the doubling of
  every round is accepted going in

Explicitly rejected even here: embedded scripting languages, HTML
rendering engines, notmuch-tag write-back, the fat that sank other
mutt successors.

## Non-goals

S/MIME, POP3, scoring: still out; revisit only if daily use proves
otherwise. MH/MMDF folders and compressed-folder hooks join them:
maildir, mbox, and IMAP cover this machine.
