# rmut 1.0 roadmap

Goal: a daily-drivable mutt replacement for local maildirs — mutt's
index/pager workflow, message manipulation, and compose, with mutt
default keybindings throughout. IMAP/SMTP and muttrc compatibility were
originally post-1.0; IMAP/SMTP (R5) made it in anyway.

**1.0 declared** (2026-07) after R1–R5. Remaining milestones below are
post-1.0.

## R1 — read-only browser (done, 0.1)

- [x] Workspace: `rmut-core` (maildir scan, message parsing via
      mailparse) + `rmut-tui` (ratatui binary `rmut`)
- [x] Maildir `new/` + `cur/` scan with filename flag parsing (S/R/F/T/D)
- [x] Index: mutt-style line (number, status char, date, from, subject),
      date sort, j/k/PgUp/PgDn/=/* navigation, bold new/unseen
- [x] Pager: decoded headers + first text/plain body, scroll, percent
      indicator, J/K to move between messages
- [x] Unit tests for flag parsing, MIME text extraction, RFC 2047

## R2 — a real MUA index (done, 0.2)

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

## R3 — threading and compose (done, 0.3)

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

## R4 — config and polish (done, 0.4)

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

## R5 — IMAP and SMTP (done, 0.5)

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

## R6 — PGP/GPG (done, 1.1)

- [x] Decrypt on view: PGP/MIME (multipart/encrypted) and inline PGP
      via gpg(1), decrypted body shown in the pager
- [x] Verify signatures (multipart/signed + inline), good/bad/unknown
      status line in the pager
- [x] Compose: sign, encrypt, or both from the send prompt (mutt-style
      security menu), recipient key lookup by address, encrypt-to-self
- [x] Config: `[pgp]` section — gpg command, default signing key,
      sign_by_default / encrypt_by_default

## R7 — muttrc importer (done, 1.2)

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

## 1.3–1.6 — mutt-parity batches (done, no milestone)

Driven by real-world muttrc imports rather than planned up front:
stored `password` on accounts (1.3); IMAP STARTTLS and the first
"implement missing" batch — ssl_starttls/force_tls, smtp_pass,
smtp_authenticators (AUTH LOGIN fallback), print_command (1.4); the
second batch — sort/sort_aux, date_format, mime_forward,
pager_index_lines/context, ~T pattern + tagged color, imap_peek,
auto_view filters, extra binds (1.5); index_format `%?X?then&else?`
conditionals, `%L`, 3-char `%Z` (1.6); mutt-style quit/purge prompts.

## R8 — compose attachments and message commands (done, 1.7)

The biggest daily-use gap: rmut could not attach a file to outgoing
mail.

- [x] Attach files: `Attach: <path> [description]` pseudo-headers in
      the draft, collected at send into multipart/mixed (base64,
      content-type guessed from the extension); works for compose,
      reply, and forward; attachment count shown at the send prompt
- [x] PGP over the assembled multipart, lifting the old "no PGP with
      an attached forward" restriction (RFC 3156 wraps any entity)
- [x] `C` copy to mailbox — like `s` save but without marking the
      original deleted
- [x] `|` pipe the raw message to a shell command
- [x] `b` bounce (Resent-\* block prepended, recipients on the
      sendmail argv / SMTP envelope) and `e` edit the message as a
      new draft (mutt's resend)

## R9 — identities and hook basics (done, 1.8)

One global `[identity]` before; mutt users switch From per folder and
per recipient via folder-hook/send-hook.

- [x] Per-account identity on `[[accounts]]` (`identity = { name,
      email }`): From follows the open mailbox's account
- [x] mutt's `reverse_name`: replying uses whichever of our addresses
      the original was addressed to, display name kept
- [x] `[[identities]]` rules — `folder` and/or `recipient` globs,
      layered over `[identity]` in order (folder-hook + send-hook in
      one mechanism); the draft gets a visible, editable From line
- [x] Importer: `reverse_name` and folder-hook/send-hook lines that
      only set from/realname translate (regex → glob for the easy
      shapes); anything else stays `# not imported`

## R10 — patterns v2 (done, 1.9)

- [x] Real pattern parser: `!` negation, `|` OR, `()` grouping
      (adjacent terms stay implicit AND); parse errors reach the
      status line instead of matching nothing
- [x] `~d` date ranges (`~d 01/06/2026-30/06/2026`, open ends, `<1w`,
      `>2d`, `=3d`; units y m w d H M)
- [x] `~t` to, `~c` cc, `~C` to-or-cc, `~e` sender (header read on
      demand), `~p` addressed-to-me
- [x] String arguments are case-insensitive regexes (regex-lite); an
      invalid regex degrades to the old substring match

## R11 — new-mail awareness and IDLE (done, 1.10)

- [x] Track new mail across the configured local mailboxes (mutt's
      `mailboxes` notion): the poll watches their new/ and hints
      "new mail in ..." in the status line when one grows
- [x] Counts in the folder browser `y` (local: scan `new/`; IMAP:
      STATUS UNSEEN, the open folder from its cache); folders with
      new mail show bold
- [x] IMAP IDLE on the open folder (RFC 2177, dedicated connection,
      re-issued before the half-hour limit, stops on mailbox switch);
      NOOP polling stays as the fallback when the server lacks it

## R12 — address completion (done, 1.11)

- [x] Tab at the To prompt (compose and bounce) completes the word
      under the cursor against alias nicks by prefix; repeated Tab
      cycles multiple matches, with a match counter shown behind the
      input
- [x] `query_command` (khard/abook/LDAP): run on Tab with the current
      word (`%s` or appended, shell-quoted), mutt's tab-separated
      output parsed (first line skipped, `addr<TAB>name` rows);
      imported 1:1 from a muttrc

**1.11 tagged and released** (2026-07) after R12 — the whole gap-review
roadmap (R8–R12) is shipped. The milestones below are the former
unscoped candidates, ranked; none is committed until picked up.

**1.13 tagged and released** (2026-07) after R14 (macros + OAuth2);
R15 remains the last scoped milestone.

## R13 — macros (done, 1.12)

The last thing the importer routinely skipped: `macro` lines.

- [x] Config: `[macros.index]` / `[macros.pager]`, `key = "sequence"`
      — a key that replays a sequence of keys (mutt semantics:
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

## R14 — OAuth2 (done, 1.13)

For Gmail/O365 accounts, where app passwords are dying out.

- [x] SASL XOAUTH2 and OAUTHBEARER (RFC 7628) for IMAP AUTHENTICATE
      and SMTP AUTH — no SASL-IR, so any server works; a failure
      challenge is answered with an empty line to surface the NO
- [x] Account config: `auth = "xoauth2"/"oauthbearer"` +
      `token_command` (like password_command, prints an access token;
      refresh is the external tool's business — oauth2ms,
      mutt_oauth2.py). Tokens expire, so it runs per connection,
      never cached
- [x] Importer: `imap_authenticators`/`smtp_authenticators` naming
      oauthbearer/xoauth2 become `auth` + a token_command placeholder;
      a stored imap_pass is then dropped (redacted note) instead of
      emitted alongside

## R15 — mbox (done, 1.14)

- [x] Open `/var/mail/$USER` and friends: From_-line splitting with
      mboxrd `>From` unquoting, mirrored into a cache maildir (like
      IMAP) so the index/pager work unchanged; messages keyed by
      content hash, so cache flags survive re-mirrors; new deliveries
      picked up by the poll
- [x] Write support: `$` sync rewrites the file in place under an
      exclusive flock — purged messages dropped, Status:/X-Status:
      rewritten (RO/AF), quoting preserved; refuses when the spool
      changed since the mirror (refresh with G, sync again)

## Candidates (unscoped)

- Sidebar, notmuch/xapian search
- `%l` line counts and `%L` list-name detection in index_format
- Non-goals for now: S/MIME, POP3, scoring
