# rmut 1.0 roadmap

Goal: a daily-drivable mutt replacement for local maildirs — mutt's
index/pager workflow, message manipulation, and compose, with mutt
default keybindings throughout. IMAP/SMTP and muttrc compatibility are
explicitly post-1.0.

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

## Post-1.0 candidates

- IMAP and SMTP accounts, OAuth2
- mbox format support
- PGP/GPG signing and encryption
- HTML part rendering (w3m-style dump)
- Sidebar, notmuch/xapian search
