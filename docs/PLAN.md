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

## R8 — compose attachments and message commands (1.7)

The biggest daily-use gap: rmut cannot attach a file to outgoing mail.

- [ ] Attach files: `Attach: <path> [description]` pseudo-headers in
      the draft, collected at send into multipart/mixed (base64,
      content-type guessed from the extension); works for compose,
      reply, and forward; attachment count shown at the send prompt
- [ ] PGP over the assembled multipart, lifting the current "no PGP
      with an attached forward" restriction (RFC 3156 wraps any part)
- [ ] `C` copy to mailbox — like `s` save but without marking the
      original deleted (plumbing exists in `copy_to`)
- [ ] `|` pipe the raw message to a shell command (the `p` print path
      becomes a preset of it)
- [ ] `b` bounce (resend as-is to new recipients) and `e` edit the
      message as a new draft

## R9 — identities and hook basics (1.8)

One global `[identity]` today; mutt users switch From per folder and
per recipient via folder-hook/send-hook.

- [ ] Per-account `name`/`email` on `[[accounts]]`: From follows the
      open mailbox's account when composing
- [ ] mutt's `reverse_name`: replying uses whichever of our addresses
      the original was addressed to
- [ ] Folder-pattern → identity mapping in config for local maildirs
      (the minimal folder-hook)
- [ ] Importer: translate folder-hook/send-hook lines that only set
      from/realname; anything else stays `# not imported`

## R10 — patterns v2 (1.9)

- [ ] Real pattern parser: `!` negation, `|` OR, `()` grouping
      (adjacent terms stay implicit AND)
- [ ] `~d` date ranges (`~d 01/06/2026-30/06/2026`, `~d <1w`, `~d >2d`)
- [ ] `~t` to, `~c` cc, `~e` sender, `~p` addressed-to-me
- [ ] Regex matching where mutt uses regexes, keeping today's
      case-insensitive behavior for plain strings

## R11 — new-mail awareness and IDLE (1.10)

- [ ] Track new mail across all configured mailboxes (mutt's
      `mailboxes` notion), not just the open one: mark folders with
      new mail in the browser `y` and hint in the status line
- [ ] Unread counts in the folder browser (local: scan `new/`; IMAP:
      STATUS UNSEEN)
- [ ] IMAP IDLE on the open folder, falling back to NOOP polling when
      the server lacks it

## R12 — address completion (1.11)

- [ ] Tab at the To/Cc prompt completes against the alias file
- [ ] `query_command` (khard/abook/LDAP): run it on Tab with the
      current word, parse mutt's tab-separated output format

## Candidates (unscoped)

- Macros (key → sequence of actions) — would also let the importer
  translate mutt `macro` lines instead of skipping them
- OAuth2 (XOAUTH2/OAUTHBEARER) for IMAP/SMTP accounts
- mbox read support
- Sidebar, notmuch/xapian search
- Non-goals for now: S/MIME, POP3, scoring
