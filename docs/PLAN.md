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
- [ ] Publish rmut-core + rmut-tui to crates.io
      (`cargo install rmut-tui`): metadata ready and
      `cargo package` verified; `just publish` after `cargo login`
      (core first, the tui depends on it)
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

## R31: patterns v3

Goal: close the pattern-operator gaps a mutt hand reaches for first.

- [ ] `~h` (any header, read from disk like `~e`), `~i` (Message-ID),
      `~x` already exists; check `~r` (received date) against Date
      handling
- [ ] `~m` message ranges (`~m 10-20`, `.`, `$`), `~z` size ranges
      (`~z >100K`)
- [ ] `~=` duplicate messages (same Message-ID seen twice)
- [ ] everywhere patterns go: limit, search, pattern-ops, color_index
      rules, identities

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

## R33: batch and CLI round

Goal: rmut as a drop-in mutt for scripts and one-shot sends.

- [ ] `rmut -s subj -a file [-c cc] -- addr < body` sends without
      the TUI (identity/SMTP from config)
- [ ] `mailto:` argument opens a prefilled compose (also covers
      being the system mailto handler)
- [ ] `-p` recalls the postponed picker, `-y` opens the mailbox
      list, `-z`/`-Z` exit codes for new mail
- [ ] `-e command` runs a config command at startup (needs R32's
      parser)

## R34: mailing lists

Goal: mutt's list machinery around the existing `%L`.

- [ ] `subscribe`/`lists` (config + importer), `~l` pattern
- [ ] `L` list-reply in index and pager
- [ ] honor Mail-Followup-To on group replies; set it on mail to
      subscribed lists ($followup_to)

## R35: alternates and my_hdr

Goal: rmut knows which addresses are "me" and what extra headers to
send.

- [ ] `alternates` (config regex list + importer): reply-to-all
      dedup, the `+`/`T` index marks, reverse_name lookups
- [ ] `my_hdr`/`unmy_hdr`: default headers merged into every compose
      (edit_headers shows them)

## R36: hooks round 2

Goal: the per-context hooks daily mutt configs actually use.

- [ ] fcc-hook / fcc-save-hook (pattern → Fcc mailbox)
- [ ] message-hook (pattern → display-time settings) and reply-hook
- [ ] crypt-hook: per-recipient PGP key selection
- [ ] folder-hook running arbitrary commands (on top of R32)

## R37: format=flowed

Goal: f=f both ways.

- [ ] display: reflow flowed paragraphs to the wrap width, keeping
      quote depth
- [ ] compose: optionally send text/plain; format=flowed
      ($text_flowed), space-stuffing included

## R38: MIME polish

Goal: the remaining display-selection knobs.

- [ ] alternative_order / unalternative_order (importer too),
      slotting into pick_alternative
- [ ] unauto_view; read ~/.mailcap for filters when [filters] has no
      entry (copiousoutput entries only)
- [ ] decrypted PGP/MIME entities render through the full tree
      (attachments inside encrypted mail), not just their first text
      part

## Beyond mutt (frozen: only on explicit request)

Ideas that exploit what mutt structurally can't do. Unlike the
proposed rounds above, these are NOT part of normal work: never pick
one up on your own, not even as a paper cut or "while I'm in there";
each starts only when explicitly asked for by name. Ranked by
value-per-effort:

- Undo: a stack over delete/flag/tag/move and limit-scoped bulk ops
  (the deferred-sync dirty model makes this cheap; mutt has nothing)
- Undo send: outgoing mail waits N seconds with a cancel key before
  sendmail/SMTP fires
- text/calendar: render meeting invites (when/where/who) in the
  pager instead of a base64 blob; maybe accept/decline replies
- Attachment reminder: body mentions an attachment but none attached,
  so ask at the send prompt
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

Explicitly rejected even here: embedded scripting languages, HTML
rendering engines, notmuch-tag write-back, the fat that sank other
mutt successors.

## Non-goals

S/MIME, POP3, scoring: still out; revisit only if daily use proves
otherwise. MH/MMDF folders and compressed-folder hooks join them:
maildir, mbox, and IMAP cover this machine.
