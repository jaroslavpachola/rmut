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
      mailparse) + `rmut` (the ratatui binary crate; `rmut-tui` until 2.0)
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
- [x] Publish rmut-core, rmut-session and rmut to crates.io
      (`cargo install rmut`): `just publish` after `cargo login`, in
      dependency order (core, session, tui). 2.0.0 went up on
      2026-08-27. 2.0.1 went up from a checkout that was 14 commits
      behind, so it is missing the 2.0 cut; 2.0.2 supersedes it and
      the stale version can be yanked
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
- [x] Bugfix (1.82, 2026-08-27): the answer came back on a channel
      nothing woke the loop for, and the loop slept its full 1s input
      poll on every fetch: 1013ms per IMAP body, measured, from the
      day R55 shipped. Now the poll is 30ms while a job is in flight.
      A user fetch also blocked (`settle`) behind the poll tick's
      CheckNew + Unseen; it now defers itself and runs from
      `poll_network` when the connection frees up, ahead of the
      tick's own follow-ups. The e2e harness never saw the second:
      its periodic forced redraws (SIGWINCH) woke the loop. Timing
      claims about the TUI need a plain pty read, not `expect`

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

## R56: the reply and forward text (done, 1.56)

Goal: the three strings every mutt user changes. rmut hardcoded all
of them, in compose.rs: "On {date}, {from} wrote:", the `"> "` in
`quote`, and `"[{addr}: {subject}]"`.

- [x] `compose::render_quoted`, mutt's specifiers over the message
      being answered: %a address, %n name (falling back to the
      address, as mutt's does), %f the From header as written, %s
      subject, %i message-id, %d date, %{...} strftime, and the
      padding and conditionals the index format already had
- [x] `[mail] attribution`, `indent_string` and `forward_format`,
      defaulting to mutt's
- [x] `[mail] include`: yes / no / ask-yes / ask-no. The question was
      always asked, which is mutt's ask-yes; ask-no now takes the no
      on Enter, and the other two decide without asking
- [x] `[mail] ask_cc` / `ask_bcc`: asked between To and Subject on
      every compose, as mutt's are, with the group reply's own Cc as
      the prefill. R52's asks made it about forty lines
- [x] All six settable at runtime and carried over by
      `--import-muttrc`; e2e scenario_reply_text, five session tests,
      and four in core for the specifiers

## R57: the importer stops saying "no rmut equivalent" when there is one (done, 1.57)

Goal: the importer's promise is that nothing is dropped silently and
nothing is claimed falsely. Two dozen lines broke it in both
directions.

- [x] What rmut has and the importer forgot: `sidebar_visible`,
      `sidebar_width`, and `alias_file`, which is a setting now
      ([mail] alias_file, mutt's own): rmut reads and appends to the
      file the config names, so an import of a muttrc that keeps its
      own alias file neither copies nor rewrites it, and says where
      the inline aliases belong instead
- [x] A third bucket, "rmut does these its own way", for the ones
      that are neither settings nor holes: `header_cache` and
      `message_cachedir` (rmut caches under ~/.cache/rmut),
      `imap_keepalive`, `crypt_use_gpgme` (gpg(1) directly),
      `mailcap_path` ($MAILCAPS), `implicit_autoview` (an empty
      [filters] command), `sidebar_format` and friends
- [x] `imap_idle = yes` is satisfied rather than skipped; `= no` is
      still skipped, since rmut cannot be told not to IDLE
- [x] The same muttrc of 90 settings now leaves 41 lines unclaimed,
      down from 60, and most of what is left is R58 to R60

## R58: reading habits (done, 1.58)

Goal: the pager and index options that change how it feels to read,
all of them one flag and a branch.

- [x] `[pager] pager_stop`: Space on the last page stays there
      instead of opening the next message
- [x] `[pager] markers`: rmut already drew mutt's `+` on wrapped
      continuation lines, so the setting is to turn it off. The plan
      said "not drawn by rmut at all", which was a guess and wrong;
      reading the code first is the whole point of these rounds
- [x] `[pager] smart_wrap`: off breaks at the column rather than at
      the last space before it
- [x] `[index] collapse_unread`: off leaves threads holding unread
      mail open when everything else folds. `[index]
      uncollapse_jump`: unfolding lands the cursor on the first
      unread message of the thread
- [x] All four settable at runtime, carried over by the importer
      (`yes` on the three that are already rmut's way reports as
      satisfied), two session tests, one e2e scenario, one for the
      wrap. The muttrc of 90 is down to 36 unclaimed lines

## R59: leaving and filing habits (done, 1.59)

Goal: the questions mutt asks on the way out, and where mail lands.

- [x] `[mail] quit`: yes (the default, as mutt), no, ask-yes or
      ask-no. Leaving moved into the session as `Session::leave`,
      where the quit question, mark-old-unread, the purge question
      and `Request::Quit` are one flow rather than four steps in the
      key handler
- [x] `[mail] confirmappend`: asks before adding to a mailbox that is
      already there. Off by default, where mutt asks: rmut has never
      asked, and changing that under a daily user is worse than a
      setting they can turn on
- [x] `[mail] save_name` / `force_name`: the save prompt offers
      `=<sender's local part>` when such a mailbox exists, or
      whether or not with force_name
- [ ] `move` + `mbox` were dropped on purpose. Moving read mail out
      of the spool is mutt's oldest habit and its riskiest: it
      deletes from one mailbox and writes to another, after a sync
      has cleared the undo stack. The importer says so rather than
      claiming a hole, and a folder-hook with a macro does it for
      anyone who wants it
- [x] The muttrc of 90 is down to 31 unclaimed lines; three session
      tests and e2e scenario_leaving_habits

## R60: threading knobs (done, 1.60)

Goal: the four that change what a thread is. Reading the code first
turned three of them into the importer's business and left one real
gap.

- [x] `strict_threads`: rmut threads by References and In-Reply-To
      only and never by subject, so mutt's `yes` is satisfied and
      `no` is refused. The plan said rmut groups by subject "as mutt
      does", which was a guess and wrong, like R58's markers
- [x] `duplicate_threads`: rmut gives each copy of a Message-ID its
      own line, which is mutt's `no`. `yes` says so in the third
      bucket rather than reading as a hole; `~=` is how you find them
- [x] `hide_missing`: rmut never draws a message it does not have,
      which is mutt's default; `narrow_tree`: rmut's tree is two
      columns a level already
- [x] The real gap was `sort_aux`, which only understood two
      spellings. `thread::ThreadOrder` takes mutt's vocabulary now:
      `last-` orders a thread by its newest message, `reverse-` turns
      the threads round and leaves each thread's own order alone.
      Two core tests, one session test over a fixture where the two
      orders disagree
- [x] The muttrc of 90 is down to 27 unclaimed lines, from 60 when
      the pass started

## R61: the signature and the questions around the editor (done, 1.61)

Goal: what is left of the muttrc after R60, starting with the one
setting in every dotfiles repo that rmut had no answer for at all.

- [x] `[mail] signature`: a file whose contents end every draft rmut
      starts, under the quoted original, or a command when the name
      ends in `|` (mutt's rule). Read afresh per draft, so a generated
      one can say something new; unreadable is no signature rather
      than an error, since a draft is worth more than the ornament on
      it. `sig_dashes` puts mutt's `-- ` line over it, on unless
      turned off. Batch sends (-s and friends) sign off too
- [x] `[mail] forward_quote`: the forwarded message comes in quoted
      with indent_string. The markers around it stay flush: they are
      rmut's words, not the original's
- [x] `[mail] abort_nosubject`: the empty-subject question was
      hardcoded to mutt's ask-yes; it takes all four quadoption
      values now, and ask-no is the one that makes Enter keep the
      draft
- [x] `[mail] abort_unmodified`: an editor that hands the draft back
      untouched is someone quitting it, so the draft is dropped, as
      mutt does. Only the first pass counts, which is what makes a
      re-edit from the compose menu safe. The session keeps what it
      staged and compares on the way back, so the check is testable
      off the pty like everything else
- [x] Two the importer was calling holes: `reply_to = ask-yes` (rmut
      asks whenever a Reply-To differs from From) and
      `honor_followup_to = yes` (a group reply has honored
      Mail-Followup-To since R34) are satisfied; the other values of
      both are refused, since they are rmut answering for you
- [x] All five settable at runtime, three core tests, five session
      tests, e2e scenario_signature_and_send_questions
- [x] The muttrc the parity passes are measured against lives in the
      repo now (tests/e2e/muttrc-parity.rc, 95 settings), and
      scenario_import_muttrc asserts how many lines it cannot carry:
      14, down from 21 when this round started. The count is a
      ratchet a round has to move, not a number in a plan file

## R62: the small habits (done, 1.62)

Goal: the settings that are one flag each, and the reading of the
code that says which of them rmut already had.

- [x] `[mail] mark_old`: rmut has aged unread mail to old on the way
      out since the parity round; the setting is to stop it. `[ui]
      wait_key`: the "Press Enter to continue" after a shell escape
      was unconditional; off brings the screen straight back. Both
      were doing mutt's default already, so the importer satisfies
      `yes` and carries `no`
- [x] `[mail] print_confirm` (mutt's $print): rmut has always asked
      before printing, with Enter declining, which is mutt's ask-no.
      All four values now: ask-yes moves the Enter, yes never asks,
      no refuses. The command stays `[mail] print`, as mutt's
      $print_command is a separate setting
- [x] `[ui] beep_new`: the one real gap. An arrival is a
      `Notice::NewMail` now rather than prose, which is what
      notice.rs asks of a variant: a front end that rings for mail
      should not have to read the sentence. The bell hangs off that,
      next to $beep's
- [x] `[identity] reverse_realname`: with reverse_name on, rmut took
      the address *and* the name it was addressed under, which is
      mutt's default; off keeps the configured name and moves only
      the address
- [x] `$timeout` is mutt waiting out a keypress before checking for
      mail. rmut's poll runs on its own timer, so it goes in the
      third bucket pointing at poll_seconds, not the hole list
- [x] All five settable at runtime, one core test, four session
      tests, one importer test, e2e scenario_small_habits. The
      parity fixture is down to 8 unclaimed lines from 14: the
      crypto trio, two charset options, folder_format,
      imap_check_subscribed and notmuch's default URI

## Parity, third pass (proposed, 2026-08)

The second pass measured rmut against a muttrc of 95 settings; this
one measured it against mutt 2.2.12 itself: the function tables of
every menu, the pattern table, and all 418 `set` variables in the
manual, diffed against rmut's keymaps, `pattern.rs`, the sort
vocabulary and the IMAP and TLS code. What follows is what the plan
never named: neither shipped, nor proposed, nor put under Non-goals.
309 of the 418 variables are unknown to the importer, most of them
noise (smime_*, pop_*, mixmaster, autocrypt); the rounds below are
the daily-use residue, ranked by how soon a mutt hand trips over
them. Each is small enough to ship on its own, and the parity fixture
should grow the settings a round claims, so the ratchet keeps
measuring. Daily-use findings still outrank all of them.

## R63: thread surgery and reading a thread (done, 1.63)

Goal: the index keys that act on the thread's *shape*, which rmut
could not touch at all.

- [x] `#` break-thread and `&` link-threads: `#` takes the
      In-Reply-To and References off the message under the cursor, so
      it and its replies become a thread of their own; `&` makes the
      tagged messages replies to it, each given an In-Reply-To naming
      it and untagged, as mutt's `mutt_break_thread` / `link_threads`
      do. Both rewrite the message file in place through
      `message::with_thread_headers` (the header block replaced, the
      body and line endings kept), reparse it, and are one undo step
      each, the old bytes kept in the step so `z` writes them back.
      Local maildirs only: on IMAP and mbox the real message lives
      elsewhere, so they refuse. Threading needed one fix to make a
      break stick — a reply's References still names the broken
      message's old ancestors, so `thread::link` now refuses to
      reparent a message that carries no references of its own
- [x] Ctrl+R read-thread and Alt+r read-subthread (mutt's `Esc r`),
      a new `ThreadOp::Read` over the existing thread walk; `P`
      parent-message and root-message (unbound) as motions, from the
      thread tree `index_threads` now derives from the depth-first
      layout
- [x] Alt+t already tagged the thread; tag-subthread is an action
      with no default key, as in mutt (`:bind` gives it one)
- [x] `~(PATTERN)`, `~<(PATTERN)`, `~>(PATTERN)`, `~v` and `~$`:
      the pattern engine takes a `ThreadView` of the message's
      neighbours (members, parent, children, folded) through a new
      `EnvSource`, so a thread term reads the other messages. Outside
      thread sort `~(P)` is P of the message and the other four are
      false, as mutt has it. Everywhere patterns go: limit, search,
      pattern-ops, color_index
- [x] `$reply_regexp`: `reply_subject` takes a compiled regex now,
      strips whatever it matches at the subject's start (RE:, Re[2]:,
      a locale's Aw:/Sv:) and puts one "Re: " on. Case-insensitive
      unless the regex holds an uppercase letter, as mutt compiles
      it; `[mail] reply_regexp`, imported and settable at `:`.
      `$sort_re` is the subject-threading half, which rmut does not
      do, so the importer satisfies `yes` and refuses the rest
- [x] e2e scenario_thread_surgery, five session tests, three core
      tests (the anchored-root rule, the header rewrite, the thread
      patterns), and the parity fixture grew sort_re and reply_regexp

## R64: labels and the pattern table (done, 1.64)

Goal: the rest of mutt's readable pattern table, X-Label, and the
show keys. `w` set-flag / `W` clear-flag moved to R68 with the other
small keys: rmut's `d`/`u`/`F`/`N` already set the flags a message
carries, and the mutt letter menu is a small ergonomic add, not a
gap.

- [x] X-Label, the round's spine: `env.label` from the header
      (cached, absent in pre-1.64 caches), `%y` in index_format, `~y`
      pattern, `Y` edit-label (set/change/clear on the message or the
      tagged set, rewriting the file through a generalised
      `message::with_header`, one undo step, local maildirs only like
      the thread edits), and `o y` sort by label, unlabelled last.
      Imported `sort = label` no longer skips
- [x] `~R` read, `~O` old (unread but not new), `~Q` replied, `~L`
      from-or-to, `~u` subscribed list (the scope gained the
      subscribed matchers), `~B` whole message (headers and body).
      Everywhere patterns go
- [x] `Esc l` show-limit (Alt+l) and `V` show-version, TUI-side
      notices
- [x] The terms rmut does not carry name themselves: `~g` `~G` `~V`
      `~k` (crypto, their own round), `~X` (attachment count, needs
      the `attachments` machinery), `~S` `~E` (no in-memory state),
      and `~n` `~H` (mutt's scoring, a Non-goal) all parse to a
      "~X is not supported" error rather than "unknown pattern"
- [x] Deferred honestly: `~B`/`=B` and `=h` server-side forms stay
      local for now (R30's `~b` UID SEARCH is the only server one);
      `group`/`ungroup` and the pattern group forms; `%Y`
      (label-different-from-parent). None blocks daily use
- [x] e2e scenario_labels_and_flags, three session tests (edit-label
      with sort and undo, the read-only pattern table, the refusals),
      importer sort=label

## R65: decoded save, pipe and print (done, 1.65)

Goal: mutt's decode family. rmut saved and copied raw, piped raw,
printed decoded, and none of it was a setting.

- [x] Alt+s decode-save / Alt+C decode-copy (mutt's Esc s / Esc C):
      the message as the pager shows it (weeded headers, decoded
      body) into a mailbox, one undo step, take the tag prefix. A new
      `displayed_text` builds that form; `copy_one` takes a `decode`
      flag, so save/copy and decode-save/copy are one path
- [x] `$pipe_decode` (off) and `$print_decode` (on) decide raw vs
      decoded for pipe and print; `$pipe_split` / `$print_split` run
      the command once per message; `$pipe_sep` separates the
      concatenated run (mutt's `split = no`, which R41 already was).
      `run_over` is the shared driver, all five settable at `:` and
      imported
- [x] Fixed on the way: `pipe_to` errored on a BrokenPipe when the
      command ignores stdin and exits first (grep -q, printf), which
      aborted the rest of a split run and could drop mail from a
      concatenated one under load. A BrokenPipe on the stdin write is
      no longer a failure; the exit status is the verdict
- [x] Deferred honestly, each its own reason: decrypt-save/copy
      (PGP, with the crypto patterns of R64); `$display_filter` and
      `$prompt_after`; and attachment delete-entry / undelete-entry,
      which rebuilds the MIME tree and re-encodes — a round of its
      own, not a rider here. `$copy_decode_weed` and friends fold
      into decode-save's weeding
- [x] e2e scenario_decode_family, two session tests (decode-save's
      decoded copy and undo, pipe_split's per-message runs)

## R66: the trust store (done, 1.66)

Goal: let a mutt user with a private or self-signed server reach it,
without weakening TLS for anyone else. The IMAP folder-management
half (CREATE/DELETE/RENAME/SUBSCRIBE) and the `$tunnel` transport are
their own rounds below (R69, R70): folder mutation needs the worker
plumbing and the test server extended, and both want more care than a
rider here.

- [x] `net.rs` knew only webpki's Mozilla roots. It now builds the
      root store from those *always* (the baseline is never removed),
      plus, opt-in, the OS trust store (`[net] system_cas`, mutt's
      `$ssl_usesystemcerts`, on by default via rustls-native-certs)
      and a PEM file of extra roots (`[net] certificate_file`, mutt's
      `$certificate_file` / `$ssl_ca_certificates_file`) for a
      private CA or a self-signed server's own cert. The settings can
      only add anchors, never take the defaults away; a bad cert in a
      bundle is skipped, an empty file is an error
- [x] Installed process-wide by `net::set_trust`, beside the
      timeouts, from the config at startup and on `:set` — the TLS
      handshake runs on connection threads that carry an account but
      not a config
- [x] Importer: `certificate_file` and `ssl_ca_certificates_file`
      carry over, `ssl_usesystemcerts = no` turns the OS store off,
      `= yes` is satisfied. `ssl_verify_host` / `ssl_verify_dates`
      are refused for `no` (rmut always verifies and has no
      accept-once), and `tunnel` / `ssl_client_cert` are skipped
      by name, pointing at R69/R70
- [x] Two net unit tests (the baseline roots are always present with
      the OS store off; a non-PEM certificate_file is an error) and
      an importer test; the parity fixture grew certificate_file and
      ssl_usesystemcerts. No e2e: a TLS server with a custom cert is
      more than the pty harness should grow for this
- [ ] Deferred to R69/R70: the interactive accept-once TOFU that mutt
      offers on an unknown cert, `$ssl_client_cert`, `$tunnel` /
      `$preconnect` / `$tunnel_is_secure` (a third transport beside
      plain and TLS on the R55 `Remote` thread), and IMAP folder
      management with `account-hook`, `$imap_passive`, NAMESPACE and
      `$folder_format`

## R67: the outgoing envelope (done, 1.67)

Goal: the three compose settings hardcoded since the start — the
Message-ID host, the User-Agent header, and where the signature
sits. The rest of the old broad R67 (compose-menu editing keys,
reply-crypto, DSN, charset) split into R71 and R72 below: they are a
different theme (interactive compose-menu and crypto), not envelope
headers, and reply-crypto belongs with R64's deferred `~g`/`~G`.

- [x] `$hostname`: `[mail] hostname` overrides the host in a
      generated Message-ID, both the interactive send and the batch
      CLI. `$use_domain` imports to the third bucket, since rmut
      takes the host from the system name (or `$hostname`) already
- [x] `$user_agent`: off by default (neomutt's default), `[mail]
      user_agent = true` adds `User-Agent: rmut/VERSION` when the
      draft has none of its own. `finalize` grew a `finalize_with`
      that both send paths call
- [x] `$sig_on_top`: `[mail] sig_on_top` puts the signature above
      the quoted original. `with_signature` became `with_signature_at`
      with an `on_top`; the interactive and batch drafts share it
- [x] All three settable at `:`, imported (with `use_domain`
      satisfied), a core test (finalize's User-Agent and the two
      signature placements), an importer test, e2e
      scenario_outgoing_envelope, and the parity fixture grew all
      three
- [ ] Split out to R71/R72: `$postpone` / `$recall` quadoptions and
      the compose-menu editing keys (edit-from, attach-message,
      rename-attachment, toggle-unlink, view-text/mailcap…); the
      reply-crypto family (`$crypt_replysign`, `$crypt_replyencrypt`,
      `$pgp_replyinline`, extract-keys/mail-key) with R64's crypto
      patterns; `$use_envelope_from`, `$dsn_notify`/`$dsn_return`,
      `$reply_self`, `$fcc_attach`/`$fcc_clear`, `$forward_edit`; and
      `$send_charset`/`$charset`/`$assumed_charset`

## R68: navigation — jumping, sorting, the sender's address (done, 1.68)

Goal: the mutt navigation keys and sort orders rmut lacked. The rest
of the old R68 grab-bag (screen knobs, the mark options, the `un*`
commands, persistent history) split into R73/R74 below.

- [x] Number entry: a run of digits in the index then Enter jumps to
      that message (mutt's count-then-jump), shown on the message
      line as it is typed; any other key ends the run, Esc cancels
- [x] `@` display-address: the selected message's full From header
      on the message line, in the index and (already) the pager
- [x] Sort keys `to` (by the first To address) and `unsorted` /
      `mailbox-order` (the on-disk file-name order, which an mbox
      user expects); the sort menu grew `o` and `u`, matching mutt's
      own letters (verified against `mutt_select_sort`:
      d/f/r/s/o/t/u/z…). `label` was already there from R64
- [x] Session test for the two sort orders and the menu keys, e2e
      scenario_navigation (number-jump, `@`, sort-to, a past-the-end
      number), importer maps `display-address`
- [ ] Split to R73/R74: `$history_file`/`$save_history` (R23's
      history dies with the session), `$simple_search` (the bare-word
      pattern, which expands at every parse site), the screen knobs
      (`$status_on_top`, `$arrow_cursor`, the terminal title
      `$ts_*`…), the mark options (`$delete_untag`, `$keep_flagged`,
      `$maildir_trash`…), the remaining keys (`%` toggle-write, `\`
      pager search-toggle, H/M/L, purge-message, next-unread-mailbox),
      and the `un*` commands (`uncolor`, `unhook`, `unmailboxes`,
      `unalias`, `reset`)
- [ ] Still owed by the importer: classify every one of mutt's 418
      variables into carried / satisfied / its-own-way /
      refused-by-name / hole, so a "no rmut equivalent" line is a
      decision, not an absence (smime_*, pop_*, mixmaster, autocrypt
      refuse under Non-goals)

## R69: IMAP folder management (done, 1.69)

Goal: the folder mutation a "real" server setup wants, split out of
R66 because it needed the worker plumbing and the test server grown.

- [x] CREATE / DELETE / RENAME / SUBSCRIBE / UNSUBSCRIBE as client
      methods on `imap::Client`, each a single tagged command whose
      NO/BAD surfaces as an error (a folder_management unit test
      scripts the five and a refused DELETE). A `Job::Manage(Manage)`
      variant carries them to the worker thread; `remote::` wraps each
      in the reconnect-once retry
- [x] The browser (`y`): `C` creates a folder (an `imap:account/name`
      spec makes a remote one, a plain path a local maildir), `d`
      deletes the selected mailbox behind a y/n confirm, `r` renames
      it, `s`/`u` subscribe and unsubscribe. `Session::{create,delete,
      rename}_folder` and `set_subscribed` route by spec: the open
      account's verb goes down the connection, a local path to the
      filesystem, and a spec for another account is refused
- [x] Deferred to a follow-up, none blocking: LSUB-backed
      subscribed-only listing (`$imap_list_subscribed` /
      `$imap_check_subscribed`), Tab toggle-mailboxes, enter-mask /
      `$mask`, `$folder_format`, `$imap_passive`, `$imap_delim_chars`,
      NAMESPACE, and `account-hook`. `$confirmcreate` too — rmut
      creates without asking, as it has since R26
- [x] Unit test at the protocol layer, the IMAP e2e scenario grown to
      create/subscribe/unsubscribe/rename/delete against the fake
      server and assert each verb reached it; 59/59 scenarios

## R70: the tunnel and the unknown cert (post-2.0)

Goal: the two trust/transport pieces R66 deferred, both security-
sensitive enough to want their own round. Not a 2.0 blocker: the cert
piece wants a careful design, not a deadline (see "The 2.0 cut").

- [ ] `$tunnel` / `$preconnect`: run a command and speak IMAP over
      its stdin/stdout (ssh to the mail host), `$tunnel_is_secure`
      deciding whether STARTTLS is still demanded. A third transport
      beside plain and TLS on the R55 `Remote` thread
- [ ] The interactive accept-once / accept-always TOFU mutt offers on
      an unknown certificate, and `$ssl_client_cert`. This weakens
      TLS if done wrong, so it wants a careful design and a real
      test, not a rider

## R71: the compose quadoptions and the From/Reply-To keys (done, 1.70)

Goal: the two send/recall quadoptions and the compose-menu header
keys, split out of R67. The heavier compose-menu functions (an
attach-message picker, rename-attachment, toggle-unlink…) and the
envelope odds stay for R75 below: they need a mailbox picker or new
draft-assembly plumbing, not a rider.

- [x] `$postpone`: leaving a draft. `ask-yes` (the default) and
      `ask-no` ask, with the matching Enter default; `yes` postpones
      and `no` discards without asking. `ask_postpone` became `&mut`
      and does the deciding when there is no question to put
- [x] `$recall`: composing when postponed drafts wait. `no` never
      offers (always a new message), `yes` recalls the newest
      outright, the default puts the new/recall choice up. Read in
      the TUI's `start_compose`; `ask-yes`/`ask-no` both mean "ask",
      rmut's one prompt shape
- [x] Compose-menu `F` and `r` edit the From and Reply-To through the
      existing `ask_header`, which already writes any header onto the
      staged draft
- [x] Both quadoptions settable at `:`, imported (with the defaults
      satisfied), a session test over the four `$postpone` answers,
      an importer test, e2e scenario_compose_menu (recall = no goes
      straight to a new message, `F` overrides From), and the parity
      fixture grew both
- [ ] Left for R75: `A` attach-message from the open mailbox (a
      picker), rename-attachment, toggle-unlink, toggle-disposition,
      new-mime, move-up/down, write-fcc, view-text/view-mailcap,
      ispell; and the envelope odds `$use_envelope_from` /
      `$envelope_from_address`, `$dsn_notify` / `$dsn_return`,
      `$reply_self`, `$fcc_attach` / `$fcc_clear`, `$forward_edit`,
      `$mime_forward_rest`

## R72: reply crypto and charset (done, 1.71)

Goal: the crypto defaults and the charset knobs R67 deferred. The
gpg-integration key operations and inline-vs-MIME PGP split into R76
below — they need gpg fixtures and a second PGP encoding path, not a
rider.

- [x] `$crypt_replysign`, `$crypt_replyencrypt`,
      `$crypt_replysignencrypted`: a reply inherits the original's
      protection. A new `pgp::classify` reads the MIME type (and the
      inline PGP markers) *without running gpg* — the defaults must
      never decrypt to decide — and `Session::security_for` layers
      those on the `sign_by_default` / `encrypt_by_default` base for
      replies only, as mutt does. All three `[pgp]` bools, off by
      default, settable at `:` and imported
- [x] `$charset` / `$send_charset` were already handled (satisfied
      for UTF-8, skipped otherwise); `$assumed_charset` joins the
      third bucket, pointing at how mailparse decodes declared
      charsets and rmut reads undeclared 8-bit as UTF-8
- [x] Core test for `classify` (each MIME shape and inline marker), a
      session test for `security_for` over signed / encrypted / plain
      originals and the reply-only rule, an importer test; the parity
      fixture grew `crypt_replysign` / `crypt_replyencrypt`. No e2e:
      the send-through-gpg path is already the PGP scenario's job, and
      the default is pure logic
- [ ] Left for R76: `$crypt_opportunistic_encrypt` (per-recipient key
      lookup), `$pgp_replyinline` / `$pgp_autoinline` (a second,
      inline PGP encoding beside PGP/MIME), `$postpone_encrypt`, and
      the index key ops Ctrl+K extract-keys, Esc k mail-key, Esc P
      check-traditional-pgp

## R73: the terminal title and the write toggle (done, 1.72)

Goal: the terminal title and `%`, the two most-wanted of R68's
screen/key grab-bag. The rest of the keys, the layout knobs, and the
mark options split into R77 and R78 below.

- [x] `$ts_enabled` / `$ts_status_format` (and `$ts_icon_format`, the
      same slot): the terminal title, set while running from a format
      string over the status specifiers (default "rmut: %f"), only
      re-sent when it changes. `index_status`'s field function was
      pulled out to `index_status_field` so the bar and the title
      share it; the title goes out as an OSC via crossterm's SetTitle.
      Off by default, as in mutt
- [x] `%` toggle-write: flip the open mailbox's writable state for the
      session (`Session::toggle_write`); the `-R` session flag cannot
      be turned off this way, as mutt refuses too
- [x] Both settable at `:`, imported (with `ts_enabled = no`
      satisfied), an importer test, e2e scenario_title_and_write (the
      OSC escape carries the format, `%` makes a delete refuse then
      writable again), the parity fixture grew both
- [ ] Left for R77/R78: `\` pager search-toggle, H/M/L, top/middle/
      bottom-page, next-unread-mailbox, purge-message, mark-message
      hotkeys, mark-as-new, error-history, what-key, list-action; the
      layout knobs `$status_on_top`, `$status_chars`, `$arrow_cursor`,
      `$menu_scroll`/`$menu_context`/`$menu_move_off`, the `$help`
      toggle, `$sleep_time`, `$read_inc`/`$write_inc`/`$net_inc`; and
      the mark options `$delete_untag`, `$keep_flagged`, `$flag_safe`,
      `$maildir_trash`, `$uncollapse_new`, `$hide_thread_subject`,
      `$hide_limited`/`$hide_top_limited`, `$thread_received`,
      `$mail_check_recent`, `$check_new`

## R74: simple_search (done, 1.73)

Goal: the bare-word search, the most-used of R74's original grab-bag.
History persistence and the `un*` commands split into R79 below.

- [x] `$simple_search`: `[mail] simple_search` is the template a bare
      single word (no `~`) expands to, `%s` the word — default `~f %s
      | ~s %s`, which is exactly what a bare word already did, so the
      default changes nothing; `~f %s | ~s %s | ~b %s` adds the body.
      `Session::compile_search` does the expansion and the three
      interactive parse sites (search, limit, pattern-ops) route
      through it; the config color rules parse raw, as they must.
      Only a lone word with no `~` expands — a multi-word or
      `~`-carrying input is a pattern already
- [x] Settable at `:`, imported, a session test (the default finds
      the subject only, a `~b` template finds the body, a `~`-input
      is left alone), an importer test, e2e scenario_simple_search,
      the parity fixture grew it
- [ ] Left for R79: `$history_file` / `$save_history` / `$history`
      (R23's prompt history dies with the session), `$search_context`,
      `$wrap_search`, `$sort_browser`, `$sort_alias`, and the config
      teardown commands `uncolor`, `mono`/`unmono`, `unhook`,
      `unmailboxes`, `unalias`, `reset`; `$shell`, `$tmpdir`

## R75: the heavier compose-menu functions (done, 1.86)

Goal: the compose-menu work R71 left, which needs a picker or new
draft plumbing. The compose-menu keys are in the 2.0 cut; the
envelope odds moved to R89 (post-2.0).

- [x] The Attach: line grew mutt's per-part state as `@`-options
      between the type and the description: `@name="..."`
      (rename-attachment, Ctrl+O), `@inline` (toggle-disposition,
      Ctrl+D), `@unlink` (toggle-unlink, `u`; the send removes the
      file). `Attachment::of` and `send_name`; mixed_entity honours
      all three, and sends a `message/rfc822` part whole, unencoded,
      without a filename
- [x] `A` attach-message: mutt opens a mailbox to tag in; rmut takes
      the tagged messages of the open mailbox (the cursor's when none
      is tagged), inline as mutt makes them, and refuses a header-only
      IMAP cache file by name
- [x] `n` new-mime: "New file: ", "Content-Type: " (base/sub
      checked), the file made when missing, attached, and handed to
      the editor through `Request::EditFile`; the menu comes back
- [x] `K` / `J` move-up / move-down swap Attach: lines (mutt leaves
      them unbound; the menu has no key table to bind by name yet)
- [x] `w` write-fcc: `outgoing_text` is the send path's assembly
      without the send (finalize, attachments, original, security),
      delivered to a local maildir or appended to a folder of the
      open account; the draft stays
- [x] `i` ispell: `$ispell -x FILE` on the real terminal, through the
      shell-escape path
- [x] `V` view-mailcap: `mailcap::viewer_for`, terminal-wanting
      entries run on the real terminal, copiousoutput ones into the
      viewer; Esc+v view-text shows the bytes as text
- [x] Two core tests (the line grammar round trip; name, disposition
      and rfc822 in the entity), three session tests, a pty scenario
      that attaches, renames, unlinks, writes and sends. 70/70
      scenarios

## R76: the PGP odds (post-2.0)

Goal: the crypto pieces R72 deferred — each needs gpg fixtures or a
new encoding path. Not a 2.0 blocker.

- [ ] `$crypt_opportunistic_encrypt` (encrypt when every recipient
      has a key), `$pgp_replyinline` / `$pgp_autoinline` (inline PGP
      as an alternative to PGP/MIME), `$postpone_encrypt`
- [ ] The index key ops: Ctrl+K extract-keys (gpg --import from the
      message), Esc k mail-key (mail your public key), Esc P
      check-traditional-pgp

## R77: page motion (done, 1.74)

Goal: the window-motion keys, the cleanest of R73's leftover keys.

- [x] `H` top-page and `M` middle-page move the cursor to the top /
      middle of the visible index page, from `index_offset` (the same
      scroll offset `%P` reads) plus the page height. `bottom-page`
      is an action with no default key, since `L` is list-reply, as
      in mutt — the index overrides the generic `L` there
- [x] Imported under their mutt names, e2e scenario_page_motion (over
      a 20-message mailbox on a short screen: after jumping to the
      last, `H` lands above the bottom and below the top, `M` between
      `H` and the bottom)
- [ ] Left for R80: `\` pager search-toggle, next-unread-mailbox,
      purge-message (delete past `$trash`), mark-message hotkeys,
      mark-as-new, error-history, what-key, list-action over
      List-Unsubscribe / List-Help

## R78: the layout knobs (done, 1.76)

Goal: the two visible display options, the cleanest of R73's layout
set. The rest of the layout knobs and the mark options split into R82.

- [x] `$status_on_top`: the status bar and message line move to just
      under the help bar rather than the bottom — `draw`'s vertical
      layout reorders and re-labels its four regions. Off by default
- [x] `$arrow_cursor`: the selected index row is marked with `->`
      instead of reverse video; `draw_index` prepends the marker and
      skips the REVERSED modifier. Off by default
- [x] Both `[ui]` bools, settable at `:`, imported (with the `no`
      defaults satisfied), an importer test, e2e scenario_layout (the
      arrow and the top-placed status bar), the parity fixture grew
      both
- [ ] Left for R82: the rest of the layout knobs (`$status_chars`,
      `$menu_scroll`/`$menu_context`/`$menu_move_off`, the `$help`
      toggle, `$sleep_time`, `$read_inc`/`$write_inc`/`$net_inc`) and
      the mark/thread options (`$delete_untag`, `$keep_flagged`,
      `$flag_safe`, `$maildir_trash`, `$uncollapse_new`,
      `$hide_thread_subject`, `$hide_limited`/`$hide_top_limited`,
      `$thread_received`, `$mail_check_recent`, `$check_new`)

## R79: persistent prompt history (done, 1.75)

Goal: R23's prompt history persisted, the one piece of R79's set that
survives a restart. The `un*` commands and the remaining search knobs
split into R81 below.

- [x] `$history_file`: `[ui] history_file` names a file that R23's
      per-kind prompt history is loaded from at startup and written to
      on the way out (`bucket\tentry` lines, newest first), so
      patterns, addresses, mailboxes and the rest come back next
      session. `$save_history` caps the entries a bucket keeps in the
      file (default 100, rmut's in-memory cap); `$history` and
      `$save_history` are the "its own way" note. Unset keeps the
      in-memory-only behaviour rmut had
- [x] Importer carries `history_file`, an importer test, e2e
      scenario_history_file (a first session runs a limit search and
      quits, a second recalls it with Up), the parity fixture grew it
- [ ] Left for R81: `$search_context`, `$wrap_search`,
      `$sort_browser`, `$sort_alias`, and the config-teardown commands
      `uncolor`, `mono`/`unmono`, `unhook`, `unmailboxes`, `unalias`,
      `reset`; `$shell`, `$tmpdir`

## R80: pager search-toggle (done, 1.80)

Goal: `\` search-toggle, the cleanest of R80's leftover keys.

- [x] `\` (search-toggle) in the pager hides and restores the
      search-hit highlighting without forgetting the pattern: a
      `pager_search_off` flag the highlight is gated on, cleared when
      a fresh `/` search runs. Imported under its mutt name
- [x] e2e (the pager search scenario grown to toggle the highlight
      off and back on)
- [ ] Left for R86: next-unread-mailbox, purge-message (a per-message
      purge flag), mark-message hotkeys, mark-as-new, error-history,
      what-key, list-action over List-Unsubscribe / List-Help

## R81: $wrap_search (done, 1.77)

Goal: the search-wrap toggle, the cleanest of R81's set. `$search_context`
and the `un*` commands split into R83.

- [x] `$wrap_search`: `[mail] wrap_search = false` stops `n` at the
      last (or first) match instead of looping to the other end;
      `search_next` breaks on the wrap boundary when it is off. True
      by default, as in mutt
- [x] Settable at `:`, imported (`nowrap_search`), a session test
      (wrap on loops to the first match, off stays put and says "not
      found"), an importer test, the parity fixture grew it
- [ ] Left for R83: `$search_context`, `$sort_browser`, `$sort_alias`,
      and the config-teardown commands `uncolor`, `mono`/`unmono`,
      `unhook`, `unmailboxes`, `unalias`, `reset`; `$shell`, `$tmpdir`

## R83: $search_context (done, 1.81)

Goal: the search-context lines, the cleanest of R83's set. The `un*`
commands and the browser/alias sort split into R87.

- [x] `$search_context`: `[pager] search_context` keeps that many
      lines above a pager search hit rather than scrolling it flush to
      the top (`pager_search_step` subtracts it from the hit line).
      Default 0, as in mutt
- [x] Settable at `:`, imported, an importer test, e2e
      scenario_search_context (with 3 lines of context the hit is not
      the top line), the parity fixture grew it
- [ ] Left for R87: `$sort_browser`, `$sort_alias`, and the config-
      teardown commands `uncolor`, `mono`/`unmono`, `unhook`,
      `unmailboxes`, `unalias`, `reset`; `$shell`, `$tmpdir`

## R82: $status_chars (done, 1.78)

Goal: the configurable status marker, the cleanest of R82's set. The
mark/thread options and the remaining layout knobs split into R84.

- [x] `$status_chars`: `[ui] status_chars` sets the characters `%r`
      shows for the mailbox state — [0] unchanged, [1] changed (needs
      sync), [2] read-only. Unset keeps rmut's own (nothing / `*` /
      `%`), so the default look is unchanged; a mutt `set status_chars`
      carries over
- [x] Settable at `:`, imported, an importer test, e2e
      scenario_status_chars (the unchanged and changed markers), the
      parity fixture grew it
- [ ] Left for R84: the mark/thread options (`$delete_untag`,
      `$keep_flagged`, `$flag_safe`, `$maildir_trash`,
      `$uncollapse_new`, `$hide_thread_subject`, `$hide_limited` /
      `$hide_top_limited`, `$thread_received`, `$mail_check_recent`,
      `$check_new`) and the smaller layout knobs (`$menu_scroll` /
      `$menu_context` / `$menu_move_off`, the `$help` toggle,
      `$sleep_time`, `$read_inc` / `$write_inc` / `$net_inc`)

## R84: $hide_thread_subject (done, 1.79)

Goal: the thread-subject display option, the cleanest of R84's set.
The rest of the mark/thread options and the smaller layout knobs
split into R85.

- [x] `$hide_thread_subject`: a thread reply whose subject repeats
      its parent's (after stripping Re:/Fwd:) shows only the tree
      arrow. `Session::subject_hidden` decides it from `thread_parent`
      and `subject_key`, and `draw_index` blanks the subject when it
      returns true. `[index]` bool, off by default here (mutt has it
      on) so rmut's look is unchanged until asked
- [x] Settable at `:`, imported, an importer test, a session test
      (the root and a fresh-subject reply show, the repeating reply
      hides), the parity fixture grew it
- [ ] Left for R85: the mark/thread options that need more than a
      display branch (`$delete_untag`, `$keep_flagged`, `$flag_safe`,
      `$maildir_trash`, `$uncollapse_new`, `$hide_limited` /
      `$hide_top_limited`, `$thread_received`, `$mail_check_recent`,
      `$check_new`) and the smaller layout knobs (`$menu_scroll` /
      `$menu_context` / `$menu_move_off`, the `$help` toggle,
      `$sleep_time`, `$read_inc` / `$write_inc` / `$net_inc`)

## R85: the harder mark options and the last layout knobs (done, 1.83)

Goal: the mark/thread behaviour knobs that touch the delete/sync
paths, and the smaller layout options R84 left. The first of the 2.0
cut.

- [x] `DeleteRules` (mutt's mutt_set_flag(MUTT_DELETE), read once per
      operation): `$flag_safe` keeps a flagged message from every
      way of deleting (d, `;d`, delete-thread, delete-pattern, a
      save, an edit), `$delete_untag` (on, as in mutt) takes the tag
      off with the mark, except under delete-pattern, where mutt
      leaves it. The pty scenario that undeleted "the tagged" after
      `;d` learned to re-tag, as a mutt user has to
- [x] `$maildir_trash`: a purge writes the T flag and keeps the
      message in the index marked D; maildir only (the server purges
      IMAP, the mirror mbox). `$` reports it as an update, not a
      deletion
- [x] `$uncollapse_new` (on): a folded thread that receives a new
      message unfolds; off, it stays folded and its count grows.
      Done in the rescan, after the resort placed the arrival
- [x] `$mail_check_recent` (on): off, a watched mailbox holding new
      mail is announced whether or not it grew, once, until it
      empties (`mailbox_announced`)
- [x] `$check_new` (on): off, the open maildir is not rescanned while
      open; IMAP is unaffected, as in mutt
- [x] `$menu_scroll` / `$menu_context` / `$menu_move_off`: mutt's
      menu_check_recenter, line for line, as `ui::recenter`, with
      unit tests against the C. menu_scroll is on by default here
      (rmut always scrolled a line; mutt turns a page) so the feel is
      unchanged until asked
- [x] `$help` off drops the top line; the page size follows
- [x] Answered in the importer rather than implemented, each with
      its reason: `$keep_flagged` (no `$move`), `$hide_limited` /
      `$hide_top_limited` (rmut's tree never marks limited-out
      messages, which is the set side), `$thread_received` (no
      subject threading to date), `$sleep_time` (rmut never pauses on
      a message), `$read_inc` / `$write_inc` / `$net_inc` / `$time_inc`
      (progress shows as the connection reports it)
- [x] Settable at :, imported (both sides of each), importer tests,
      six session tests, a pty scenario for the help bar and
      menu_context, the parity fixture grew eight lines. 68/68
      scenarios

## R86: the last keys (done, 1.84)

Goal: the keys R80 left, each needing more than a binding.

- [x] `next-unread-mailbox`: the next configured mailbox after the
      open one holding new mail (a local new/ with files, an IMAP
      folder with an unseen count), wrapping; `FrontOp::OpenMailbox`.
      "No mailboxes have new mail" otherwise, as in mutt
- [x] `purge-message`: `Msg::purge`, set with the delete, cleared by
      undelete, and `trash_deleted` skips it (mutt's mx.c). Tagged
      too
- [x] `~` mark-message: "Enter macro stroke: ", then a
      `Request::Command(Macro)` binding the stroke to `/~i <id>` with
      the brackets off and the dots quoted. mutt's $mark_macro_prefix
      is not carried: rmut's macros are one key, so the stroke is the
      key
- [x] `mark-as-new` is mutt's pager name for toggle-new; the pager's
      from_name knows it, so a muttrc `bind pager` line lands
- [x] `error-history`: the session keeps the last $error_history
      (30) complaints; the function hands them to the front end,
      which shows them on the help screen's machinery. 0 disables
- [x] `what-key`: "Enter keys (^G to abort): " and every key is
      named ("Char = a, Octal = 141, Decimal = 97") until Ctrl+G
- [x] Esc+L list-action: `message::list_actions` reads the RFC 2369
      List-* headers (mutt's first-mailto rule, keeping a non-mailto
      URL so the refusal can be specific), a one-key menu of the six,
      a mailto: becomes `Request::Mailto` (the command-line path);
      the rest is refused in mutt's words. Index and pager
- [x] Five session tests, a core test for the header parse, a pty
      scenario over what-key, error-history and `~`. 69/69 scenarios

## R87: the browser sort and the un* commands (done, 1.85)

Goal: what R83 left. The last parity round of the 2.0 cut.

- [x] `$sort_browser` (`sort_browser` in the session: alpha, count /
      unread, date by the maildir's change time, unsorted, reverse-)
      over the browser and mailbox-completion candidates, which now
      dedupe in order instead of by sorting. size reads as alpha
- [x] `$sort_alias` (address, alias, unsorted-as-alias, reverse-)
      over address completion
- [x] `reset NAME`: the setting as a fresh Config has it, where
      `unset` zeroes; `reset all` refuses and says to reload
- [x] `uncolor` / `unmono`: rules by pattern or `*`, the named slots
      outright, undoing exactly what `color` put in; `mono OBJECT
      ATTR [PATTERN]` is a color line carrying bold / underline /
      reverse / standout / none in the fg slot, which the rule
      compiler reads as a modifier
- [x] `unhook TYPE|*`, `unmailboxes SPEC...|*` (a trailing slash is
      no difference; the sidebar is told), `unalias NICK...|*`
      rewrites the alias file without them
- [x] `$shell`: a bare `!` runs it interactively ($SHELL, then sh);
      `$tmpdir` sets $TMPDIR once at startup, which is what every
      temporary file asks
- [x] Settable at :, imported, importer test, four command tests,
      two alias tests, two session tests, the parity fixture grew two
      lines. 69/69 scenarios

## R88: functions where the logic is (done, 1.82)

Goal: the index functions stop being part of the terminal. mutt names
every operation and binds keys to the names, which is what makes a
keymap a configuration rather than a program; rmut had the names, but
they sat in `rmut-tui/src/keymap.rs` next to a 290-line dispatcher, so
the only way to reach `delete` was to press a key on a pty. R53 moved
the tests to where the logic is; this moves the last of the logic to
where the tests are.

- [x] `Function` (83 named index operations) with `name`, `from_name`,
      `all`, `describe` and `takes_tagged` moves from
      `rmut-tui/src/keymap.rs` into `rmut-session/src/function.rs`.
      `describe` goes too: a menu bar wants those strings as badly as
      a help screen does
- [x] `Session::run_function(function, tagged, page)` does the mail
      half and hands back an `Outcome`: `Done`, an `Ask` to put, or a
      `FrontOp`. 57 of the 83 finish inside the session
- [x] `FrontOp` is the honest list of what a session cannot do for
      itself: open a menu, repaint, hand the terminal to an editor,
      move the cursor by where it sits on screen. The session does
      what checking it can first, so `Query` only comes back when
      `$query_command` is set and `ChangeMailbox` only when the open
      mailbox is ready to be left
- [x] The TUI keeps the key tables (only it knows what a key is), the
      mode machine, and a `run_front_op` for the 26 that come back.
      `run_index_action` is 12 lines; `rmut-tui` is 602 lines lighter
      and `rmut-session` 686 heavier
- [x] Nine session tests that could not exist before, naming functions
      the way a muttrc binds them: every name round-trips and
      describes itself, delete marks and advances, `;` hands over the
      tagged set and refuses when nothing is tagged, a question comes
      back as a question, page motion uses the page it is given, a
      read-only mailbox refuses what would write
- [x] The pty suite's version assertion reads the workspace version
      instead of pinning it, so a release bump stops failing a
      scenario about labels
- [ ] Left for a follow-up: the same for `PagerAction` (44 more
      functions, 326 lines), and `$recall` becoming an `Ask` instead
      of the front end's own quadoption branch
## R89: the envelope odds (post-2.0)

Goal: the send-side options split out of R75 so 2.0 does not wait on
them. Real, but nobody's first-week complaint.

- [ ] `$use_envelope_from` / `$envelope_from_address`, `$dsn_notify` /
      `$dsn_return`, `$reply_self`, `$fcc_attach` / `$fcc_clear`,
      `$forward_edit`, `$mime_forward_rest`

## The 2.0 cut (decided 2026-08-27)

2.0 means: mutt parity is finished for the transports and folders this
machine uses (maildir, mbox, IMAP/SMTP over TLS). Every default key a
mutt user presses does something, an existing muttrc loads without
errors, and the crates are installable. No breaking change is pending;
the major bump marks the roadmap finishing, not an incompatibility.

Blocking, in order:

- [x] R85: the mark/thread knobs on the delete/sync path and the last
      layout knobs. These go *before* the tag because they are where
      muscle memory can lose mail (1.83)
- [x] R86: the last keys (1.84)
- [x] R87: the browser sort and the `un*` / `reset` commands (muttrc
      compatibility) (1.85)
- [x] R75: the compose-menu functions (the envelope odds moved to R89)
      (1.86)
- [x] R22 leftover: publish rmut-core, rmut-session and rmut to
      crates.io. Prepared 2026-08-27: the metadata packages clean
      (`cargo publish --dry-run -p rmut-core`), the names are free.
      Names settled the same day, after a look at czkawka
      (czkawka_core / czkawka_cli / czkawka_gui / krokiet, bare
      `czkawka` never claimed) and at crates.io's squatting rule (a
      crate that "exists only to reserve a name" may be removed): the
      binary crate is `rmut` itself, ripgrep's shape, so
      `cargo install rmut` is the real thing and no placeholder holds
      the name; a GUI would be `rmut-egui`, or a name of its own as
      krokiet is. Published the same day at 2.0.0
- [x] Docs: `docs/rmut.1` version line, a README opening that
      describes 2.0 instead of listing rounds, and this entry

**2.0.0 tagged** (2026-08-27) after R75, and published the same day.
2.0.1 went up from a checkout 14 commits behind this line, so it is
missing the 2.0 cut: yank it, and take 2.0.2 (R90) as the version
after 2.0.0.

Explicitly not blocking (2.x): R70 (tunnel, TOFU on unknown certs —
security work is not rushed for a version number), R76 (PGP odds,
needs gpg fixtures), R89 (envelope odds), the R42 next/previous-marked
motion and the R55 remaining blocking calls (quality, not parity), and
the whole "Beyond mutt" list, which stays frozen.

## R90: threading by subject (done, 2.0.2)

Goal: the hole R60 found and named. mutt threads a message that has
no In-Reply-To and no References by its subject, and for some mail
that is the only thing there is: a notification robot mints a fresh
Message-ID every time and repeats one subject, so rmut read six
messages about one merge request as six threads while mutt, two
panes over, showed one. Found in daily use, which is what outranks
everything else here.

- [x] `core::thread::thread_with` takes a `SubjectFallback` and runs
      mutt's `pseudo_threads` after the reference pass: a thread root
      whose subject, with $reply_regexp taken off, repeats one
      already in the mailbox hangs under the message that named it.
      The rules were read off mutt 2.2.12 driven in a pty, not off
      the manual: the parent may be any message whose own subject
      differs from its parent's (mutt's subject_changed), never one
      placed this way itself, never one sent later, never one inside
      the root's own thread. What falls out is the shape mutt draws,
      the oldest message a root with a flat fan under it, and a
      subject child brings its own replies with it
- [x] `$strict_threads` turns the pass off, `$sort_re` narrows it to
      subjects that carry the reply prefix: `[index]` settings, live
      at `:` (the index re-sorts on the spot, as it does for $sort),
      and imported. R60 refused both values of both, having no pass
      to point them at; the importer now satisfies mutt's defaults
      and carries the other two
- [x] The index stars the tree where the subject, not a reference,
      decided the place (mutt's fake_thread mark):
      `ThreadedItem.pseudo` → `Session::subject_threaded` → `└*`
- [x] break-thread sticks, which in mutt it does not. `#` on a
      message the subject grouped has no headers to clear, so it
      writes `X-Rmut-Thread: broken` into the file; the pass skips
      what carries it, undo takes it off, and link-threads clears it.
      The one deliberate deviation in the round: mutt has nowhere to
      record a break, so its own subject pass hangs the message
      straight back and the manual's answer is $strict_threads. `#`
      also stops answering "already a thread of its own" when what
      holds the message is a subject group
- [x] Five core tests (the robot shape, both $sort_re answers, a
      renamed reply as the parent for its own subject, a subject
      child bringing its replies, no loops), two session tests
      (the grouping and both knobs; break-thread out of a group and
      back), an importer test, and e2e scenario_subject_threading

## 2.1.0 (released 2026-09-01)

The first release with the window: `rmut-front` (the toolkit-free
front-end pieces) and `rmut-egui` (egui over the session) join
`rmut-core`, `rmut-session` and `rmut` on crates.io, all at 2.1.0.
Since 2.0.2: G1, G2, G2p, G3 and the G2 leftovers as the sections
above tell it, and on the shared side the save path's $resolve
advance (index and pager), verified against mutt's source.

## After 2.2.1

- [x] The built-in editor, asked for 2026-09-01: `[gui] editor =
      "builtin"` (a Preferences checkbox; external stays the
      default, as the G-plan promised) routes the draft, new-mime
      files and raw-edit into an egui text box under the same file
      contract as $EDITOR - Ctrl+Enter writes and lands on the
      compose menu, Esc abandons with the draft kept, the keymap
      stands down while it is up, and raw bytes that are not clean
      UTF-8 keep the terminal so nothing is mangled

## 2.2.1 (released 2026-09-01)

The terminal hunt fixed hours after 2.2.0: it knew only foot,
alacritty, kitty and xterm, so a stock GNOME desktop could not
compose. It now also finds gnome-terminal (behind --wait, or its
server "finishes" mid-edit), konsole, xfce4-terminal, terminator and
x-terminal-emulator, each called its own way.

## 2.2.0 (released 2026-09-01)

The window grown up, all at 2.2.0 on crates.io: G4 (the compose flow
through a spawned terminal, the compose menu, postponed and query
screens, mailto, resend, -p), G5 (clickable URLs, the proportional
body, the new-mail notification), the window's own colors (#rrggbb
everywhere, [gui.colors], the canvas as the whole window's ground
with the widget theme following its brightness, the zebra derived
from it), the About overlay and the Preferences dialog with its
gui.toml overlay.

## Still open inside rounds marked done

Easy to lose under a (done, x.y) heading:

- R42: a real next/previous-marked motion, if the macros do not carry
  it
- R55: opening and switching a mailbox, the browser's LIST, a
  server-side `~b` search and an APPEND still block the TUI
- R59: `move` + `mbox` dropped on purpose; stays dropped unless asked

## The GUI front end (proposed, 2026-08-27)

Goal: `rmut-egui`, a second front end over `rmut-session`, in a
window. Same keys, same `$index_format`, same muttrc; what it adds is
what a terminal cannot do, and nothing else.

Why now, counted. The session contract is small and has stopped
moving: `Function -> Outcome { Done | Ask | Front(FrontOp) }`, 20
`FrontOp`s, two shapes of `Ask`, notices through `NoticeSink`, IMAP
on its own thread. Since the split, `function.rs` changed in 3
commits while `lib.rs` changed in 41: features land in the session,
not in the contract. The cost is the TUI crate's shape, 6.2k lines
with eight screens in `Mode` and seven process-spawn sites, which a
second front end re-implements once and then follows.

Decisions, made going in so the rounds do not re-argue them:

- **Toolkit: egui/eframe.** `ui.rs::draw(frame, app)` is already
  immediate-mode; an eframe `update` is the same function over
  different primitives, and it stays one static binary. It is the
  biggest dependency the project has taken (winit, glow); that is
  the price, and it is paid by `rmut-egui` only. `cargo install rmut`
  does not change. Checked 2026-08-27: egui / eframe / egui_kittest
  0.36.1 (released together 2026-08-07); eframe's `glow` renderer
  for the smaller binary, `wayland` feature on by default; egui
  repaints only on input or `request_repaint*`, so idling is the
  default, not a discipline; egui_kittest drives the AccessKit tree
  (`Harness`, `get_by_label`) and renders headless with `wgpu`. The
  alternatives at that date: iced 0.14.0 (2025-12, no release since),
  slint 1.17 (a DSL and a runtime of its own), gtk4 0.11 (the C
  stack), gpui 0.2 (Zed's, young as a crate). Qt, asked about the
  same day: cxx-qt 0.10.0 (KDAB, 2026-08-24, alive; qmetaobject and
  ritual are dead) bridges QObjects only, "these projects do not
  provide Rust bindings for QWidgets APIs", so the UI would be QML
  over Rust models: a second language holding the screens and half
  the key handling, a C++ compiler + CMake + Qt dev headers to build
  (this machine has the 6.4.2 runtime only), and tests on the QML
  side. What it buys, Qt's text engine and platform look, the keymap
  interface would barely use. Also looked at: qtbridge-rust, The Qt
  Company's own bridge (github.com/qt/qtbridge-rust, crates.io
  `qtbridge` 0.2.0 of 2026-07, pushed the day it was looked at,
  LGPL-3 / commercial, "pre-release"). Nicer than cxx-qt, plain
  Cargo build with attribute macros, no CMake, but the same shape,
  "Qt Quick user interfaces with Rust backends", QML holding the
  screens, and it "requires Qt 6.10 or higher" with `qmake` on PATH
  and the private headers; Ubuntu 24.04 ships 6.4.2, so on this
  machine it means the Qt installer or a source build before the
  first window. Rejected unless native look becomes a
  requirement; if it does, qtbridge is the bridge to use. This machine: Wayland, Mesa 25.2 on Iris Xe, GL 4.6.
- **The editor stays external.** Compose hands the draft to
  `$EDITOR` inside `gui.terminal` (`$TERMINAL`, then a short list:
  foot, alacritty, kitty, xterm), `-e`, and waits, exactly as the
  TUI waits with the screen stood down. mutt's identity is the
  editor, and a built-in one would be a different program. A
  `TextEdit` compose is allowed later, opt-in, never the default.
- **HTML stays rejected.** The GUI renders the same text the pager
  renders. Images it can show; documents it cannot.
- **Doubling is bounded by a test.** `rmut-front` enumerates every
  `FrontOp` and every `AskKind`; each front end registers what it
  handles, and the unhandled set is asserted, not discovered. A new
  `FrontOp` fails the GUI's build until it is either handled or
  listed as a said-once "not in the GUI" notice.

Each round is shippable alone and leaves the TUI and its 70
scenarios unchanged.

### G1: rmut-front, the part of a front end that is not a toolkit

The prep that pays without a GUI: what the TUI does that has nothing
to do with ratatui, moved under it so both front ends share it.

- [x] `rmut-front` crate. `keymap.rs` moves in over its own
      `key` module (`KeyCode`, `KeyModifiers`, `KeyEvent`, shaped
      like crossterm's so the move was an import swap); the TUI maps
      crossterm's event to it at its one read site (`front_key`).
      `just publish` gained the crate in order. 70/70
- [x] The pure `ui.rs` functions moved: `pager_rows`,
      `wrap_line_with`, `quote_depth`, `recenter`, `humanize_size`,
      `Row` / `RowKind` (`pager`); `index_title`, the
      `$status_format` / `$pager_format` expanders and `pager_wrap`
      (`status`, over `&Session`). `Style` became the toolkit-free
      description (fg, bg, bold, underline, reverse) in `style`,
      `Theme` and the color-rule compiler moved over it, and the
      TUI's `theme.rs` is now the mapping onto ratatui
- [x] The line editor: `editor::LineEdit` (the mutt/readline keys,
      history stepping), `editor::History` (buckets, the history
      file) and `editor::Complete` (token, first match, Tab cycling),
      out of `app.rs`; the prompt's meaning (`LineKind`, what a
      submit does, which candidates) stayed in the front end
- [x] The coverage check, simpler than the registry sketched above
      once counted: `AskKind` needs none (a front end never matches
      on it; it carries the question back to `Session::answer`
      untouched, so every kind is handled by construction), and for
      `FrontOp` the dispatch is an exhaustive match under
      `#[deny(clippy::wildcard_enum_match_arm)]` - a new variant
      fails the build until the front end says what it does with it.
      The GUI's dispatch gets the same lint
- [x] `app.rs` shrank by what left (3339 to 3177 lines); nothing else in
      it changed. 71/71 scenarios, unit tests followed their code

### G2: a window that reads mail

The smallest GUI that is honestly usable: read-only, keyboard-driven,
the same keys.

- [x] `crates/rmut-egui`, workspace member, binary `rmut-egui`
      (egui 0.36: the unified `Panel` API, `App::ui`). Args are its
      own small set (`-R`, `-y`, `-z`/`-Z`, `-e`, `-f`, a mailbox,
      `-V`); `-s`/`--` are refused with a pointer at `rmut`, `-p`
      says its round - one shared `parse_args` is still owed, the
      window also remembers its zoom in the cache between runs
- [x] Index: the shared `rmut-front::index` row builder (text and
      style from one place; the TUI now draws from it too), monospace
      LayoutJob rows, thread depth, collapse, the sidebar
- [x] Pager: `pager_rows` over the same `MessageView`, quote colors,
      body color rules; `$pager_index_lines` above the message,
      `$pager_context` overlap, and the pager search (/, n, N, the
      backslash toggle, `$search_context`, highlighting) through the
      shared `search_lines`
- [x] Status bar and message line from the shared `rmut-front::status`
      expanders; notices through the same one-line `NoticeSink`
- [x] Repaint discipline: the window idles. `request_repaint_after`
      at the poll interval, 30ms while the connection owes an answer;
      `poll_network` / `check_new_mail` run between frames the way
      the TUI runs them between keys
- [x] `Ask::Line` and `Ask::Key` as a bottom line, driven by the
      shared line editor with history; Tab completion ported from the
      TUI (aliases + query_command at address prompts, folder
      candidates at mailbox ones, the empty-c browser fallthrough)
- [x] `FrontOp`s handled: Exit, OpenSelected, Help, Redraw, PageMove,
      Sidebar, ErrorHistory, OpenMailbox, ChangeMailbox, Folders,
      WhatKey, CommandPrompt, TagPrefix; macros replay through the
      same queue; `:` commands run (bind/macro from `:` still owed).
      Everything that writes or spawns is a said-once notice, and
      `read_only_session` is forced
- [x] Tests without a pty: the key logic drives `Gui::handle_keys`
      directly over a maildir fixture (motion, pager, limit, screens,
      the read-only guarantee, the said-once refusals), and
      `egui_kittest` 0.36 runs the layout headless through
      `Gui::frame`, split from the eframe shell for exactly that.
      Snapshot scenarios can come with the wgpu feature if wanted

### G2p: the pointer (still read-only)

The aim, said 2026-08-31, re-ordering the rounds: a clickable
interface - menus, context menus, the mailbox stripe - ahead of
images and typography. All of it fits inside the read-only gate, and
all of it obeys one rule: a click fires the same `Function` or
keystroke path the keyboard uses (mostly by queueing the bound keys),
so the clickable layer cannot drift from the keymap.

- [x] The menu bar: Mailbox / Message / Thread / Sort / Help, every
      item labelled with its bound key (`Button::shortcut_text`),
      firing by queueing those keys at the top of the next frame
- [x] Index rows clickable: click selects, double-click opens,
      right-click selects and is a context menu of the message
      operations, every item the keys it stands for
- [x] The mailbox stripe: the sidebar entries clickable (click
      opens), counts as today, hover highlight; the folder browser
      rows click open too
- [x] Wheel scrolling in the index, pager and help screens; key
      motion keeps mutt's recentering (it now runs only on key
      frames), the wheel moves the view without dragging the cursor
- [x] The look of a list: a translucent hover wash and a faint zebra
      stripe under the index, the selection full-strength reverse
- [ ] Tests where logic allows (the menu items queue the right keys);
      the pointer itself is the user's to judge. Still owed, with the
      round judged in daily use first

### G3: a window that changes mail

- [x] Dropped the forced read-only (-R still means what it means, one
      test guards it); sync, delete/undelete, flags, tags, tag-prefix,
      limit, sort through the same `Function`s, the pager's write keys
      wired the TUI's way (delete advances or falls out, undo takes
      $undo_send first), Alt+c's read-only open carried through
- [ ] The postponed and query lists as selectable lists - moved to
      G4, where Enter on them can actually compose; the folder
      browser has been in since G2
- [x] Attachments: the menu (list, view, save, pipe, print) with the
      TUI's keys plus click/double-click; text and auto_view-filtered
      parts open in a part pager that knows its way back; `image/*`
      parts decode in a view of their own through egui_extras'
      loaders (the first thing the window does that the terminal
      cannot). Inline-in-the-pager rendering still owed; mailcap
      viewer spawning still owed
- [ ] Coverage registry: every `AskKind` handled

### G4: a window that sends mail

- [x] `[gui] terminal` and the spawn wrapper ($TERMINAL, then
      foot/alacritty/kitty/xterm), non-blocking: the child is polled
      each frame, keys wait while it runs, and its close resumes the
      flow. `$EDITOR` for compose, raw-edit and new-mime files; `!`
      and `$shell`; `print_command` and pipes as before
- [x] The compose menu screen (headers, the attachment table,
      clickable rows) with the TUI's keys; send, postpone, recall
      ($recall ask included), the postponed picker and the query
      screen (their G3 deferral repaid), mailto and resend, notmuch,
      `-p`; `$undo_send` flushes on close with trouble on stderr.
      Still owed: the compose menu's mailcap/text views (V, Esc v)
- [x] Every `FrontOp` now acts; the said-once notice survives only
      for suspend (a window minimizes instead) and `:`-bound keys

### G5: what only a window can do

Each one small, each one behind a `[gui]` key, none changing the
TUI.

- [x] Clickable URLs in the pager: body rows carrying a URL lay out
      as segments with real links (trailing sentence punctuation left
      outside), each opening through xdg-open - spawned by hand,
      since eframe's own opener wants a webbrowser release crates.io
      no longer resolves
- [x] Typography, in two sittings. First (asked for): `[gui] size`
      and `[gui] font`, keyboard zoom, Ctrl+wheel / pinch zoom from
      `zoom_delta`. Then `[gui] proportional`: prose rows in a
      proportional face while the index, headers and indented
      (preformatted) lines keep the grid; a Preferences checkbox
      drives it
- [x] Native new-mail notification: the session's NewMail notice
      becomes a notify-send when the window is unfocused - only as
      the fallback with `$new_mail_command` unset, since the session
      fires that command itself for every front end
- [ ] A scrollbar and mouse selection in the pager, mouse click to
      select in the index
- [x] The window's own colors, asked for 2026-09-01: `#rrggbb`
      accepted wherever a color is named (both front ends;
      terminals carry it as truecolor), and the window alone adds
      `[gui] background` / `foreground` for the canvas plus a
      `[gui.colors]` table laid over `[colors]`, so the window can
      wear its own palette while the terminal keeps the shared one
- [ ] Menus, since egui has them (`MenuBar`, `menu_button`,
      `SubMenu`, `Response::context_menu`): a bar of Mailbox /
      Message / Thread / Help and a right-click menu on an index row,
      every item a `Function` labelled with its bound key, so the
      menus are the help screen made clickable. The keymap stays the
      primary interface; the menus are generated from it, never a
      second list of what rmut can do
- [ ] Several windows are not several sessions; one mailbox per
      process, as with `rmut`. Revisit only if daily use asks

Order of proof: G1 lands regardless of the rest. G2 is used daily for
a week before G3 starts; if reading mail in the window is not better
than reading it in the terminal, the plan stops there, with G1 kept
and `rmut-egui` a read-only viewer that costs nothing to keep.

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
- A GUI over the session library: planned above (G1-G5, 2026-08-27),
  with the doubling bounded by the coverage registry rather than
  accepted open-ended

Explicitly rejected even here: embedded scripting languages, HTML
rendering engines, notmuch-tag write-back, the fat that sank other
mutt successors.

## Non-goals

S/MIME, POP3, scoring: still out; revisit only if daily use proves
otherwise. MH/MMDF folders and compressed-folder hooks join them:
maildir, mbox, and IMAP cover this machine.
