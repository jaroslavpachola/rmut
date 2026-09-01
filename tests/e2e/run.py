#!/usr/bin/env python3
"""End-to-end tests: drive the rmut binary in a pty and assert on
screens and on-disk maildir state.

Screen matching note: ratatui redraws only changed cells, so captured
output loses spaces between unchanged regions. All assertions therefore
match against whitespace-squashed, ANSI-stripped text.
"""

import base64
import os
import pty
import re
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import fcntl
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
RMUT = os.path.join(REPO, "target", "debug", "rmut")
# What V prints, read from the workspace rather than pinned here, so a
# release bump does not fail a scenario about labels.
VERSION = re.search(
    r'^version = "(.*)"', open(os.path.join(REPO, "Cargo.toml")).read(), re.M
).group(1)

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Z0-9]|\x1b[78=>]")


def squash(raw: str) -> str:
    return re.sub(r"\s+", "", ANSI.sub("", raw))


def make_maildir(root, name=""):
    d = os.path.join(root, name) if name else root
    for sub in ("cur", "new", "tmp"):
        os.makedirs(os.path.join(d, sub), exist_ok=True)
    return d


MSGS = {
    "jane": (
        "cur/1751790000.1.host:2,S",
        "From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
        "Subject: Lunch on Friday?\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
        "Message-ID: <msg1@example.com>\r\n\r\n"
        "Hi Jarda,\r\n\r\nAre you free for lunch on Friday?\r\n\r\nJane\r\n",
    ),
    "petr": (
        "new/1751866200.2.host",
        "From: =?utf-8?q?Petr_Nov=C3=A1k?= <petr@example.com>\r\nTo: jarda@example.com\r\n"
        "Subject: =?utf-8?q?Sch=C5=AFzka_z=C3=ADtra?=\r\nDate: Tue, 7 Jul 2026 08:30:00 +0200\r\n"
        "Message-ID: <msg2@example.com>\r\nMIME-Version: 1.0\r\n"
        'Content-Type: multipart/alternative; boundary="b1"\r\n\r\n'
        "--b1\r\nContent-Type: text/plain; charset=utf-8\r\n"
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n"
        "Ahoj, sejdeme se z=C3=ADtra v 9:00?\r\n\r\nPetr\r\n"
        "--b1\r\nContent-Type: text/html\r\n\r\n<p>html</p>\r\n--b1--\r\n",
    ),
    "ci": (
        "cur/1751750100.3.host:2,S",
        "From: build-bot@example.com\r\nSubject: CI failed on main\r\n"
        "Date: Sun, 5 Jul 2026 23:15:00 +0200\r\nMessage-ID: <msg3@example.com>\r\n\r\n"
        "Job 4812 failed.\r\n",
    ),
    "alice": (
        "cur/1751952000.4.host:2,S",
        "From: Alice <alice@example.com>\r\nTo: jarda@example.com\r\n"
        "Subject: Re: Lunch on Friday?\r\nDate: Wed, 8 Jul 2026 09:00:00 +0200\r\n"
        "Message-ID: <msg4@example.com>\r\nIn-Reply-To: <msg1@example.com>\r\n"
        "References: <msg1@example.com>\r\n\r\nCount me in too!\r\nAlice\r\n",
    ),
    "bob": (
        "cur/1752038400.5.host:2,S",
        "From: Bob <bob@example.com>\r\nTo: jarda@example.com\r\n"
        "Subject: Re: Lunch on Friday?\r\nDate: Thu, 9 Jul 2026 09:00:00 +0200\r\n"
        "Message-ID: <msg5@example.com>\r\nIn-Reply-To: <msg4@example.com>\r\n"
        "References: <msg1@example.com> <msg4@example.com>\r\n\r\nSo am I.\r\nBob\r\n",
    ),
}


def write_msgs(maildir, names):
    for name in names:
        rel, content = MSGS[name]
        with open(os.path.join(maildir, rel), "w") as f:
            f.write(content)


class Rmut:
    def __init__(self, maildir, env=None, rows=30, cols=160, args=()):
        self.buf = ""
        self.rows, self.cols = rows, cols
        # maildir=None: no positional, so rmut has to find one itself.
        cmd = [RMUT, *args] + ([maildir] if maildir else [])
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ["TERM"] = "xterm-256color"
            os.environ.update(env or {})
            os.execvp(cmd[0], cmd)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def keys(self, data: bytes):
        os.write(self.fd, data)

    def _drain(self, wait=0.1):
        r, _, _ = select.select([self.fd], [], [], wait)
        if r:
            try:
                self.buf += os.read(self.fd, 65536).decode("utf-8", "replace")
            except OSError:
                pass

    def expect(self, *needles, timeout=5.0, absent=()):
        # ratatui writes only the cells that changed, so text that
        # replaces text sharing its characters reaches the pty with
        # holes in it. When a needle does not turn up quickly, force a
        # full repaint and keep looking: what is on screen then lands
        # in the buffer whole.
        deadline = time.time() + timeout
        # No redraw up front: it would overtake the keys just sent and
        # repaint the screen as it was before they were read. Once
        # they have had a moment to land, redraw every so often while
        # the needle is missing, an ioctl each.
        next_redraw = time.time() + 0.2
        while time.time() < deadline:
            self._drain()
            text = squash(self.buf)
            if all(squash(n) in text for n in needles):
                for a in absent:
                    assert squash(a) not in text, f"unexpected {a!r} on screen"
                return
            if time.time() >= next_redraw:
                self._force_redraw()
                next_redraw = time.time() + 0.3
        raise AssertionError(
            f"timed out waiting for {needles!r}; tail: {squash(self.buf)[-400:]!r}"
        )

    def _force_redraw(self):
        """Toggle the window width so ratatui repaints every cell."""
        self.cols = 159 if self.cols == 160 else 160
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ,
                    struct.pack("HHHH", self.rows, self.cols, 0, 0))

    def repaint(self):
        """Settle, then force a full redraw before reading the screen.

        ratatui writes only the cells that changed, so a status line
        sharing characters with the one it replaced arrives in the pty
        stream with holes in it, and expect() cannot see it. A window
        resize makes the whole screen paint again. The status itself
        survives: only a key press clears it, and a resize is not one.
        """
        # Let the keys just sent land first: a resize that overtakes
        # them repaints the screen as it was before they were read.
        self.settle()
        self._force_redraw()
        self.settle()

    def settle(self, wait=0.5):
        end = time.time() + wait
        while time.time() < end:
            self._drain()

    def close(self):
        try:
            os.close(self.fd)
        except OSError:
            pass
        try:
            os.waitpid(self.pid, os.WNOHANG)
        except ChildProcessError:
            pass


def wait_for(cond, timeout=5.0, desc="condition"):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if cond():
            return
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {desc}")


def base_env(tmp, extra=None):
    env = {
        "RMUT_CONFIG": os.path.join(tmp, "no-config.toml"),
        "RMUT_ALIASES": os.path.join(tmp, "no-aliases"),
        "EMAIL": "jarda@example.com",
        # Keep header/mirror caches inside the sandbox, away from the
        # user's real ~/.cache.
        "XDG_CACHE_HOME": os.path.join(tmp, "cache"),
    }
    env.update(extra or {})
    return env


def scenario_view_and_pager(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3", "New:1", "Lunch on Friday?", "Schůzka zítra", "CI failed on main")
    r.keys(b"y")  # the folder browser counts Petr's message in new/
    r.expect("(1 new)")
    r.keys(b"q")
    r.keys(b"\r")  # newest (Petr, new) opens in the pager
    r.expect("sejdeme se zítra v 9:00", "Message 3/3")
    r.keys(b"K")  # previous message
    r.expect("Are you free for lunch")
    r.keys(b"i")
    r.keys(b"q")  # read-marks are written silently on quit, like mutt
    r.close()


def scenario_pager_save_advances(tmp):
    # mutt's $resolve in the pager (pager.c OP_SAVE): a successful
    # save opens the next undeleted message instead of staying on the
    # saved one.
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")
    r.keys(b"\r")  # newest (Petr) opens in the pager
    r.expect("sejdeme se zítra v 9:00")
    r.keys(b"K")  # back to Jane's lunch question
    r.expect("Are you free for lunch")
    r.keys(b"s")
    r.expect("Save to")
    r.keys(os.path.join(tmp, "archive").encode() + b"\r")
    # The save advanced, and the pager followed to the next message.
    r.expect("saved to", "sejdeme se zítra v 9:00")
    r.keys(b"i")
    r.keys(b"x")
    r.close()


def scenario_pager_save_last_exits(tmp):
    # mutt's pager on a last-message save: next-undeleted finds
    # nothing and falls out to the index (curs_main.c in_pager break)
    # instead of keeping the saved message on screen.
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")
    r.keys(b"\r")  # newest (Petr) is the last message; open it
    r.expect("sejdeme se zítra v 9:00")
    r.keys(b"s")
    r.expect("Save to")
    r.keys(os.path.join(tmp, "archive").encode() + b"\r")
    r.expect("saved to")
    # Back on the index, not in the pager: judge the repainted screen
    # alone, not the accumulated pty history.
    r.buf = ""
    r.expect("Msgs:3", "CI failed on main", absent=("Message 3/3",))
    r.keys(b"x")
    r.close()


def scenario_sync_delete_flag_limit(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")
    r.keys(b"\r")  # read Petr's (new/) message
    r.keys(b"i")
    r.keys(b"$")
    r.expect("synced: 0 deleted, 1 updated")
    assert os.listdir(os.path.join(md, "new")) == []
    assert os.path.exists(os.path.join(md, "cur", "1751866200.2.host:2,S"))
    r.keys(b"=")  # first (CI)
    r.keys(b"d")
    r.expect("Del:1")
    r.keys(b"$")  # deletions pending -> purge prompt
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"y")
    r.expect("synced: 1 deleted")
    assert not any("1751750100" in f for f in os.listdir(os.path.join(md, "cur")))
    r.keys(b"*F$")  # flag newest, sync
    flagged = os.path.join(md, "cur", "1751866200.2.host:2,FS")
    wait_for(lambda: os.path.exists(flagged), desc="flagged rename on disk")
    r.keys(b"l~f jane\r")
    r.expect("Msgs:1/2", "limit:~f jane")
    r.keys(b"l")
    r.keys(b"\x15\r")  # ctrl+u clears the prefilled limit
    r.expect("Msgs:2")
    # patterns v2: OR, negation, and error reporting
    r.keys(b"l~f jane|~f petr\r")
    r.expect("limit:~f jane|~f petr")
    r.keys(b"l\x15!~f jane\r")
    r.expect("limit:!~f jane")
    r.keys(b"l\x15~J\r")
    r.expect("bad pattern: unknown pattern ~J")
    r.keys(b"l\x15\r")  # back to all
    r.keys(b"q")
    r.close()


def scenario_threads_and_fold(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci", "alice"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:4")
    r.keys(b"ot")
    r.expect("sorted by threads", "└>")
    # The cursor starts on Petr (first new, like mutt); move to Alice
    # in Jane's thread before folding.
    r.keys(b"*k")
    r.keys(b"\x1bv")  # Alt+v: fold the thread under the cursor (Alice -> Jane's thread)
    r.expect("(1 hidden)")
    r.keys(b"\x1bv")
    r.expect("Re: Lunch on Friday?")
    r.keys(b"q")
    r.close()


def scenario_compose_send_postpone(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent.eml")
    editor = os.path.join(tmp, "editor.sh")
    with open(editor, "w") as f:
        f.write(f'#!/bin/sh\ncp "$1" {tmp}/draft-copy-$$.txt\n'
                f'printf "Hello from e2e\\n" >> "$1"\n')
    sendmail = os.path.join(tmp, "sendmail.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(editor, 0o755)
    os.chmod(sendmail, 0o755)
    aliases = os.path.join(tmp, "aliases")
    with open(aliases, "w") as f:
        f.write("alias petr Petr Novak <petr@example.com>\n"
                "alias pete Pete Example <pete@example.org>\n")
    query = os.path.join(tmp, "query.sh")
    with open(query, "w") as f:
        f.write("#!/bin/sh\nprintf 'Searching...\\nzdenka@example.com\\tZdenka Q\\n'\n")
    os.chmod(query, 0o755)
    cfg = os.path.join(tmp, "compose-config.toml")
    with open(cfg, "w") as f:
        # edit_headers: this scenario asserts on the header block the
        # editor sees (mutt's edit_headers style).
        f.write(f'[mail]\nquery_command = "{query} %s"\nedit_headers = true\n')
    env = base_env(tmp, {
        "EDITOR": editor,
        "RMUT_SENDMAIL": sendmail,
        "RMUT_ALIASES": aliases,
        "RMUT_CONFIG": cfg,
    })
    r = Rmut(md, env)
    r.expect("Msgs:1")
    # Prompt redraws are cell-diffed, so mid-flow screen asserts are
    # unreliable; keys are processed strictly in order, so type blindly
    # and assert on unique status strings and disk state.
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"petr\re2e test\ry")  # To (alias), Subject, editor runs, send
    wait_for(lambda: os.path.exists(sent_file), desc="sendmail invoked")
    r.expect("message sent")
    sent = open(sent_file).read()
    assert "To: Petr Novak <petr@example.com>" in sent
    assert "Subject: e2e test" in sent
    assert "From: jarda@example.com" in sent
    assert "Message-ID:" in sent and "Date:" in sent
    assert "Hello from e2e" in sent
    # reply: accept prefilled To/Subject, include the original
    # (Enter = yes at mutt's $include question), discard at the send
    # prompt
    r.keys(b"r\r\r\rqn")  # q asks to postpone; n discards
    r.expect("message discarded")

    def reply_draft():
        for p in os.listdir(tmp):
            if p.startswith("draft-copy-"):
                body = open(os.path.join(tmp, p)).read()
                if "In-Reply-To: <msg1@example.com>" in body:
                    return body
        return None

    wait_for(lambda: reply_draft() is not None, desc="reply draft captured")
    body = reply_draft()
    assert "To: Jane Doe <jane@example.com>" in body
    assert "Subject: Re: Lunch on Friday?" in body
    assert "References: <msg1@example.com>" in body
    assert "> Are you free for lunch on Friday?" in body
    # postpone, recall, send
    postponed_cur = os.path.join(md, ".rmut-postponed", "cur")
    r.keys(b"mx@y\rposty\rP")  # P postpones from the compose menu
    r.expect("postponed to")
    assert os.listdir(postponed_cur)
    r.keys(b"mry")  # recall prompt -> recall -> editor -> send
    wait_for(lambda: "Subject: posty" in open(sent_file).read(), desc="recalled mail sent")
    wait_for(lambda: os.listdir(postponed_cur) == [], desc="postponed original removed")
    # Tab completion at the To prompt: first alias match by prefix...
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"pe\t\rcomp1\ry")
    wait_for(lambda: "Subject: comp1" in open(sent_file).read(), desc="completed send")
    assert "To: Pete Example <pete@example.org>" in open(sent_file).read()
    # ...a second Tab cycles to the next match (2 aliases + the
    # query script's unconditional hit = 3 candidates)...
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"pe\t\t")
    r.expect("match 2/3")
    r.keys(b"\rcomp2\ry")
    wait_for(lambda: "Subject: comp2" in open(sent_file).read(), desc="cycled send")
    assert open(sent_file).read().count("To: Petr Novak <petr@example.com>") == 2
    # ...and query_command results complete too.
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"zd\t\rcomp3\ry")
    wait_for(lambda: "Subject: comp3" in open(sent_file).read(), desc="query send")
    assert "To: Zdenka Q <zdenka@example.com>" in open(sent_file).read()
    # R20: attach from the send prompt, review, send
    notes = os.path.join(tmp, "notes.txt")
    with open(notes, "w") as f:
        f.write("some notes\n")
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"x@y.example.com\rattprompt\r")
    r.keys(b"a")
    r.expect("Attach file:")
    r.keys(notes.encode() + b"\r")
    # the compose menu lists the new attachment
    r.expect("notes.txt", "text/plain")
    r.keys(b"y")
    wait_for(lambda: "Subject: attprompt" in open(sent_file).read(),
             desc="attach-prompt send")
    assert 'filename="notes.txt"' in open(sent_file).read()
    # R20: two postponed drafts -> the recall picker
    r.keys(b"mx@y\rdraft-one\rP")
    r.expect("postponed to")
    r.keys(b"mn")  # postponed exist: answer (n)ew first
    r.keys(b"x@y\rdraft-two\rP")
    wait_for(lambda: len(os.listdir(postponed_cur)) == 2, desc="two drafts")
    r.keys(b"mr")  # recall -> the picker (newest first)
    r.expect("postponed drafts [Found:2]", "draft-one", "draft-two")
    r.keys(b"j\r")  # pick the older draft-one; editor runs, then send
    r.keys(b"y")
    wait_for(lambda: "Subject: draft-one" in open(sent_file).read(),
             desc="picked draft sent")
    assert len(os.listdir(postponed_cur)) == 1
    r.keys(b"q")
    r.close()


def scenario_config(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "ci"])
    # A mailing-list message, for %L and %l.
    with open(os.path.join(md, "cur", "1751960000.9.host:2,S"), "w") as f:
        f.write("From: announce-bot@example.com\r\nTo: dev@lists.example.com\r\n"
                "List-Id: <dev.lists.example.com>\r\nSubject: list ping\r\n"
                "Date: Wed, 8 Jul 2026 09:00:00 +0200\r\n"
                "Message-ID: <lp@example.com>\r\n\r\none\r\ntwo\r\n")
    make_maildir(tmp, "Sent")
    cfg = os.path.join(tmp, "config.toml")
    with open(cfg, "w") as f:
        f.write(
            f"""
[identity]
name = "Jarda"
email = "jarda@example.com"
[mail]
mailboxes = ["{md}"]
sent = "{os.path.join(tmp, 'Sent')}"
poll_seconds = 1
new_mail_command = "echo %f:%n >> {os.path.join(tmp, 'hook.out')}"
[index]
format = "%C|%-4.4F|%l|%L|%s"
[keys.index]
sync = "w"
[macros.index]
L = "l~f jane<enter>"
"""
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("1|buil|1|build-bot@example.com|CI failed on main",
             "2|Jane|5|Jane Doe|Lunch on Friday?",
             "3|anno|2|To dev|list ping")
    r.keys(b"=d")  # first message (CI), mark deleted
    r.keys(b"w")  # remapped sync; confirm the purge
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"y")
    r.expect("synced: 1 deleted")
    wait_for(
        lambda: not any("1751750100" in f for f in os.listdir(os.path.join(md, "cur"))),
        desc="deleted file removed",
    )
    # help screen shows the remapped key and, after paging, the patterns
    r.keys(b"?")
    r.expect("write changes to the maildir")
    r.keys(b"\x1b[F")  # End: the patterns block sits at the bottom
    r.expect("~p addressed to me")
    r.keys(b"q")
    # the macro replays its sequence through the limit prompt
    r.keys(b"L")
    r.expect("limit:~f jane")
    r.keys(b"l\x15\r")  # clear the limit again
    # new-mail detection: drop a message into new/ and wait for the poll
    write_msgs(md, ["petr"])
    r.expect("new mail in", "+1", timeout=8)
    # ... which also fires new_mail_command with %f/%n expanded
    hook = os.path.join(tmp, "hook.out")
    wait_for(
        lambda: os.path.exists(hook) and f"{md}:1" in open(hook).read(),
        desc="new_mail_command ran",
    )
    r.keys(b"q")
    r.close()


def scenario_sidebar(tmp):
    """R17: the sidebar lists mailboxes with counts; move, open, toggle."""
    md = make_maildir(tmp, "md")
    md2 = make_maildir(tmp, "md2")
    write_msgs(md, ["jane", "ci"])
    write_msgs(md2, ["petr"])  # lands in new/: md2 counts 1
    cfg = os.path.join(tmp, "sidebar-config.toml")
    with open(cfg, "w") as f:
        f.write(
            f"""
[mail]
mailboxes = ["{md}", "{md2}"]
poll_seconds = 1
[sidebar]
visible = true
width = 20
"""
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect(">md", "md2 (1)", "Lunch on Friday?")
    r.keys(b"\x0e\x0f")  # ctrl+n highlights md2, ctrl+o opens it
    r.expect("Schůzka zítra", "[Msgs:1 New:1]")
    # Let a poll set the watch baseline in md2, then mail lands in md.
    r.settle(1.5)
    write_msgs(md, ["petr"])
    r.expect("new mail in", timeout=8)
    r.keys(b"B")     # hide the sidebar...
    r.keys(b"\x0e")  # ...then sidebar keys explain themselves
    r.expect("sidebar is hidden")
    r.keys(b"B")     # showing again repaints the whole pane fresh:
    r.expect(">md2", "md (1)")
    r.keys(b"q")
    r.close()


def scenario_send_via_config_sendmail(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-config.eml")
    sendmail = os.path.join(tmp, "sendmail-cfg.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "true-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "config2.toml")
    with open(cfg, "w") as f:
        f.write(
            f"""
[identity]
name = "Jarda"
email = "jarda@example.com"
[mail]
sendmail = "{sendmail}"
editor = "{editor}"
"""
        )
    env = base_env(tmp, {"RMUT_CONFIG": cfg})
    env.pop("RMUT_SENDMAIL", None)
    r = Rmut(md, env)
    r.expect("Msgs:1")
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"a@b\rhello\r")
    r.expect("y:Send")  # the compose menu
    r.keys(b"y")
    r.expect("message sent")
    r.settle()
    sent = open(sent_file).read()
    assert "From: Jarda <jarda@example.com>" in sent
    r.keys(b"q")
    r.close()


class FakeImap(threading.Thread):
    """Stateful IMAP server: enough of RFC 3501 (+ IDLE) for rmut's
    client. Connections are served concurrently; rmut keeps a second
    one open for IDLE."""

    def __init__(self):
        super().__init__(daemon=True)
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(4)
        self.port = self.sock.getsockname()[1]
        self.msgs = {}  # uid -> [set(flags), bytes]
        self.commands = []
        self.appended = []
        self.announce = False
        self.idle_push = False  # make the idling connection see EXISTS
        self.auth_payloads = []  # SASL responses from AUTHENTICATE
        self.body_delay = 0  # seconds a full-body fetch dawdles
        self.lock = threading.Lock()

    def add(self, uid, flags, content):
        with self.lock:
            self.msgs[uid] = [set(flags), content.encode()]

    def run(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self.serve_one, args=(conn,), daemon=True).start()

    def serve_one(self, conn):
        try:
            self.serve(conn)
        except (ConnectionError, OSError):
            pass
        finally:
            conn.close()

    def serve(self, conn):
        rfile = conn.makefile("rb")
        conn.sendall(b"* OK fake ready\r\n")
        while True:
            raw = rfile.readline()
            if not raw:
                return
            line = raw.decode().rstrip("\r\n")
            m = re.search(r"\{(\d+)\}$", line)
            if m:  # client literal (APPEND)
                conn.sendall(b"+ go\r\n")
                lit = rfile.read(int(m.group(1)))
                rfile.readline()  # trailing CRLF
                self.appended.append(lit.decode())
            self.commands.append(line)
            tag, _, cmd = line.partition(" ")
            up = cmd.upper()
            if up.startswith("IDLE"):
                # Wait for DONE, pushing an EXISTS when the test set
                # idle_push meanwhile. select() instead of a socket
                # timeout: a timed-out makefile object breaks on 3.9.
                conn.sendall(b"+ idling\r\n")
                while True:
                    with self.lock:
                        if self.idle_push:
                            self.idle_push = False
                            conn.sendall(f"* {len(self.msgs)} EXISTS\r\n".encode())
                    ready, _, _ = select.select([conn], [], [], 0.1)
                    if not ready:
                        continue
                    raw2 = rfile.readline()
                    if not raw2 or raw2.decode().strip().upper() == "DONE":
                        break
                conn.sendall(f"{tag} OK done\r\n".encode())
                continue
            if up.startswith("AUTHENTICATE"):
                # SASL with one client response (XOAUTH2/OAUTHBEARER).
                conn.sendall(b"+ \r\n")
                payload = rfile.readline().decode().strip()
                self.auth_payloads.append(payload)
                conn.sendall(f"{tag} OK done\r\n".encode())
                continue
            with self.lock:
                if up.startswith("LOGIN"):
                    pass
                elif up.startswith("CAPABILITY"):
                    conn.sendall(b"* CAPABILITY IMAP4rev1 IDLE\r\n")
                elif up.startswith("STATUS"):
                    m = re.match(r'STATUS "([^"]+)"', cmd, re.I)
                    name = m.group(1) if m else "?"
                    unseen = (
                        sum(1 for f, _ in self.msgs.values() if "\\Seen" not in f)
                        if name == "INBOX"
                        else 0
                    )
                    conn.sendall(f'* STATUS "{name}" (UNSEEN {unseen})\r\n'.encode())
                elif up.startswith("SELECT"):
                    conn.sendall(
                        f"* {len(self.msgs)} EXISTS\r\n"
                        f"* OK [UIDVALIDITY 7] ok\r\n".encode()
                    )
                elif up.startswith("LIST"):
                    conn.sendall(b'* LIST () "/" "INBOX"\r\n* LIST () "/" "Sent"\r\n')
                elif up.startswith("UID FETCH"):
                    m = re.match(r"UID FETCH ([\d,:*]+) \((.*)\)", cmd, re.I)
                    spec = m.group(1)
                    if spec == "1:*":
                        uids = sorted(self.msgs)
                    elif spec.endswith(":*"):
                        start = int(spec[:-2])
                        uids = [u for u in sorted(self.msgs) if u >= start]
                        if not uids and self.msgs:
                            # the IMAP quirk: N:* returns at least the
                            # last message
                            uids = [max(self.msgs)]
                    else:
                        uids = [int(u) for u in spec.split(",")]
                    for seq, uid in enumerate(uids, 1):
                        if uid not in self.msgs:
                            continue
                        flags, content = self.msgs[uid]
                        fl = " ".join(sorted(flags))
                        attrs = f"UID {uid} FLAGS ({fl})"
                        body = None
                        if "RFC822.SIZE" in m.group(2).upper():
                            attrs += f" RFC822.SIZE {len(content)}"
                        if "BODY.PEEK[HEADER]" in m.group(2).upper():
                            body = content.split(b"\r\n\r\n", 1)[0] + b"\r\n\r\n"
                            attrs += " BODY[HEADER]"
                        elif "BODY.PEEK[]" in m.group(2).upper():
                            if self.body_delay:
                                time.sleep(self.body_delay)
                            body = content
                            attrs += " BODY[]"
                        if body is None:
                            conn.sendall(f"* {seq} FETCH ({attrs})\r\n".encode())
                        else:
                            conn.sendall(
                                f"* {seq} FETCH ({attrs} {{{len(body)}}}\r\n".encode()
                                + body
                                + b")\r\n"
                            )
                elif up.startswith("UID STORE"):
                    m = re.match(r"UID STORE ([\d,]+) (\+?)FLAGS\.SILENT \((.*)\)", cmd, re.I)
                    flags = set(m.group(3).split())
                    for uid in (int(u) for u in m.group(1).split(",")):
                        if uid in self.msgs:
                            if m.group(2) == "+":
                                self.msgs[uid][0] |= flags
                            else:
                                self.msgs[uid][0] = set(flags)
                elif up.startswith("EXPUNGE"):
                    for uid in [u for u, v in self.msgs.items() if "\\Deleted" in v[0]]:
                        del self.msgs[uid]
                elif up.startswith("NOOP"):
                    if self.announce:
                        self.announce = False
                        conn.sendall(f"* {len(self.msgs)} EXISTS\r\n".encode())
                elif up.startswith("LOGOUT"):
                    conn.sendall(f"* BYE\r\n{tag} OK bye\r\n".encode())
                    return
            conn.sendall(f"{tag} OK done\r\n".encode())


class FakeSmtp(threading.Thread):
    """One-shot SMTP submission server; records the DATA payload."""

    def __init__(self):
        super().__init__(daemon=True)
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]
        self.message = None
        self.commands = []

    def run(self):
        conn, _ = self.sock.accept()
        rfile = conn.makefile("rb")
        conn.sendall(b"220 fake smtp\r\n")
        while True:
            raw = rfile.readline()
            if not raw:
                return
            line = raw.decode().rstrip("\r\n")
            self.commands.append(line)
            up = line.upper()
            if up.startswith("EHLO"):
                conn.sendall(b"250-fake\r\n250 AUTH PLAIN XOAUTH2\r\n")
            elif up.startswith("AUTH XOAUTH2"):
                conn.sendall(b"334 \r\n")
                self.commands.append(rfile.readline().decode().rstrip("\r\n"))
                conn.sendall(b"235 ok\r\n")
            elif up.startswith("AUTH"):
                conn.sendall(b"235 ok\r\n")
            elif up.startswith("MAIL") or up.startswith("RCPT"):
                conn.sendall(b"250 ok\r\n")
            elif up.startswith("DATA"):
                conn.sendall(b"354 go\r\n")
                payload = []
                while True:
                    data_line = rfile.readline().decode()
                    if data_line.rstrip("\r\n") == ".":
                        break
                    payload.append(data_line)
                self.message = "".join(payload)
                conn.sendall(b"250 accepted\r\n")
            elif up.startswith("QUIT"):
                conn.sendall(b"221 bye\r\n")
                return


IMAP_MSG = (
    "From: {sender}\r\nTo: jarda@example.com\r\nSubject: {subject}\r\n"
    "Date: {date}\r\nMessage-ID: <{mid}@remote>\r\n\r\n{body}\r\n"
)


def scenario_imap(tmp):
    imap = FakeImap()
    imap.add(1, {"\\Seen"}, IMAP_MSG.format(
        sender="one@remote.example", subject="remote one",
        date="Mon, 6 Jul 2026 10:00:00 +0200", mid="r1", body="body one"))
    imap.add(2, set(), IMAP_MSG.format(
        sender="two@remote.example", subject="remote two",
        date="Tue, 7 Jul 2026 10:00:00 +0200", mid="r2", body="full body two"))
    imap.start()
    smtp = FakeSmtp()
    smtp.start()
    editor = os.path.join(tmp, "editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "smtp body line\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "imap-config.toml")
    with open(cfg, "w") as f:
        f.write(
            f"""
[identity]
name = "Jarda"
email = "jarda@example.com"
[mail]
poll_seconds = 30
editor = "{editor}"
[[accounts]]
name = "test"
user = "jane"
auth = "xoauth2"
token_command = "echo test-token"
imap_host = "127.0.0.1"
imap_port = {imap.port}
imap_tls = false
smtp_host = "127.0.0.1"
smtp_port = {smtp.port}
smtp_tls = false
"""
        )
    env = base_env(tmp, {
        "RMUT_CONFIG": cfg,
        "XDG_CACHE_HOME": os.path.join(tmp, "cache"),
    })
    r = Rmut("imap:test", env)
    # The pre-TUI open narrates its progress on stderr.
    r.expect("connecting to 127.0.0.1...")
    r.expect("fetching message headers... 0/2")
    # Index built from header-only cache files; the login went through
    # SASL XOAUTH2 with the token_command's output.
    r.expect("imap:test/INBOX", "Msgs:2", "New:1", "remote one", "remote two")
    assert imap.auth_payloads, "no AUTHENTICATE payload seen"
    decoded = base64.b64decode(imap.auth_payloads[0]).decode()
    assert "user=jane" in decoded and "auth=Bearer test-token" in decoded
    r.keys(b"\r")  # newest = uid 2: body fetched from the server on view
    r.expect("full body two")
    r.keys(b"i$")  # back, sync: the read-mark goes to the server
    r.expect("synced: 0 deleted, 1 updated")
    wait_for(
        lambda: any("UID STORE 2 FLAGS.SILENT (\\Seen)" in c for c in imap.commands),
        desc="read-mark pushed via UID STORE",
    )
    assert "\\Seen" in imap.msgs[2][0]
    r.keys(b"=d$")  # first = uid 1: delete, sync, confirm the purge
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"y")
    r.expect("synced: 1 deleted")
    wait_for(lambda: 1 not in imap.msgs, desc="message expunged on the server")
    # New mail arrives server-side; IDLE (not the 30 s poll) must be
    # what makes rmut notice it this fast.
    imap.add(3, set(), IMAP_MSG.format(
        sender="three@remote.example", subject="remote three",
        date="Wed, 8 Jul 2026 10:00:00 +0200", mid="r3", body="body three"))
    imap.announce = True
    imap.idle_push = True
    r.expect("remote three", "Msgs:2", timeout=8)
    assert any("IDLE" in c for c in imap.commands), "client never idled"
    # Compose: SMTP submission plus Fcc via APPEND to Sent.
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"bob@example.org\rimap send\ry")
    r.expect("message sent, copy in Sent")
    wait_for(lambda: smtp.message is not None, desc="message on the SMTP server")
    assert "Subject: imap send" in smtp.message
    assert "smtp body line" in smtp.message
    assert any("MAIL FROM:<jarda@example.com>" in c for c in smtp.commands)
    assert any("RCPT TO:<bob@example.org>" in c for c in smtp.commands)
    wait_for(lambda: imap.appended, desc="Fcc APPEND on the IMAP server")
    assert "Subject: imap send" in imap.appended[0]
    # Folder browser lists the account's folders.
    r.keys(b"y")
    r.expect("imap:test/Sent")

    # R69: folder management. Create a remote folder (typed as a
    # spec), subscribe to the selected one, rename it, delete it.
    r.keys(b"Cimap:test/Archive\r")
    r.expect("created imap:test/Archive")
    wait_for(lambda: any('CREATE "Archive"' in c for c in imap.commands),
             desc="CREATE reached the server")
    r.settle()
    # The selection sits on a folder; subscribe/unsubscribe it.
    r.keys(b"s")
    r.expect("subscribed to")
    wait_for(lambda: any("SUBSCRIBE " in c for c in imap.commands),
             desc="SUBSCRIBE reached the server")
    r.settle()
    r.keys(b"u")
    r.expect("unsubscribed from")
    r.settle()
    # Rename the selected folder.
    r.keys(b"rRenamed\r")
    r.expect("renamed to Renamed")
    wait_for(lambda: any("RENAME " in c for c in imap.commands),
             desc="RENAME reached the server")
    r.settle()
    # Delete it, with the confirm.
    r.keys(b"d")
    r.expect("Delete mailbox")
    r.keys(b"y")
    r.expect("deleted imap:test")
    wait_for(lambda: any("DELETE " in c for c in imap.commands),
             desc="DELETE reached the server")
    r.keys(b"q")
    r.keys(b"q")
    r.close()


def scenario_mbox(tmp):
    """R15: open an mbox spool, flag/delete, write-back, new mail."""
    spool = os.path.join(tmp, "spool")
    with open(spool, "w") as f:
        f.write("From jane@example.com Mon Jul  6 10:00:00 2026\n"
                "From: Jane Doe <jane@example.com>\n"
                "Subject: spool one\nDate: Mon, 6 Jul 2026 10:00:00 +0200\n"
                "Message-ID: <s1@example.com>\nStatus: RO\n\nread already\n\n"
                "From petr@example.com Tue Jul  7 10:00:00 2026\n"
                "From: Petr Novak <petr@example.com>\n"
                "Subject: spool two\nDate: Tue, 7 Jul 2026 10:00:00 +0200\n"
                "Message-ID: <s2@example.com>\n\nfresh in the spool\n\n")
    cfg = os.path.join(tmp, "mbox-config.toml")
    with open(cfg, "w") as f:
        f.write("[mail]\npoll_seconds = 1\n")
    env = base_env(tmp, {
        "RMUT_CONFIG": cfg,
        "XDG_CACHE_HOME": os.path.join(tmp, "cache"),
    })
    r = Rmut(spool, env)
    r.expect("Msgs:2", "New:1", "spool one", "spool two")
    r.keys(b"\r")  # newest (petr, new) opens in the pager
    r.expect("fresh in the spool")
    r.keys(b"i")
    r.keys(b"F")   # flag it: the ! mark shows in its index line
    r.expect("! Jul  7 Petr")
    r.keys(b"=d")  # first (jane), mark deleted
    r.keys(b"$")
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"y")
    r.expect("synced: 1 deleted")
    # The spool was rewritten: jane gone, petr read + flagged.
    def spool_synced():
        text = open(spool).read()
        return ("spool one" not in text and "Status: RO" in text
                and "X-Status: F" in text)
    wait_for(spool_synced, desc="mbox write-back")
    text = open(spool).read()
    assert text.startswith("From petr@example.com"), text[:60]
    # Delivery appends a message; the poll re-mirrors and announces it.
    with open(spool, "a") as f:
        f.write("From ci@example.com Wed Jul  8 10:00:00 2026\n"
                "From: build-bot@example.com\nSubject: spool three\n"
                "Date: Wed, 8 Jul 2026 10:00:00 +0200\n"
                "Message-ID: <s3@example.com>\n\njob finished\n\n")
    r.expect("new mail in", "+1", timeout=8)
    r.expect("spool three")
    r.keys(b"q")
    r.close()


def scenario_trash_and_alias(tmp):
    """R20: $trash moves purged mail; create-alias appends to the file."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "ci"])
    trash = os.path.join(tmp, "trash")
    cfg = os.path.join(tmp, "trash-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[mail]\ntrash = "{trash}"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:2")
    r.keys(b"=d$")  # delete CI, sync
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"y")
    r.expect("synced: 1 deleted")
    trashed = [os.path.join(trash, sub, p)
               for sub in ("cur", "new")
               for p in os.listdir(os.path.join(trash, sub))]
    assert len(trashed) == 1, trashed
    assert "CI failed on main" in open(trashed[0]).read()
    assert not any("1751750100" in f
                   for f in os.listdir(os.path.join(md, "cur")))
    # create-alias on the remaining (jane) message
    r.keys(b"a")
    r.expect("Alias as (nick): jane")  # nick prefilled from the address
    r.keys(b"\r")
    r.expect("added: alias jane")
    aliases = open(os.path.join(tmp, "no-aliases")).read()
    assert "alias jane Jane Doe <jane@example.com>" in aliases
    r.keys(b"q")
    r.close()


def scenario_pgp(tmp):
    """Decrypt on view and sign on send, against a stub gpg."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    with open(os.path.join(md, "cur", "1751900000.7.host:2,S"), "w") as f:
        f.write(
            "From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: sealed orders\r\nDate: Tue, 7 Jul 2026 12:00:00 +0200\r\n"
            "Message-ID: <sealed@example.com>\r\nMIME-Version: 1.0\r\n"
            'Content-Type: multipart/encrypted; boundary="b";\r\n'
            '\tprotocol="application/pgp-encrypted"\r\n\r\n'
            "--b\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n"
            "--b\r\nContent-Type: application/octet-stream\r\n\r\n"
            "-----BEGIN PGP MESSAGE-----\r\nZZZ\r\n-----END PGP MESSAGE-----\r\n"
            "--b--\r\n"
        )
    gpg = os.path.join(tmp, "gpg.sh")
    recip_log = os.path.join(tmp, "gpg-recipients.txt")
    with open(gpg, "w") as f:
        f.write(
            '#!/bin/sh\ncase "$*" in\n'
            "*--decrypt*)\n"
            "  cat >/dev/null\n"
            '  echo "[GNUPG:] BEGIN_DECRYPTION" >&2\n'
            '  echo "[GNUPG:] DECRYPTION_OKAY" >&2\n'
            '  echo "[GNUPG:] GOODSIG AAA Jane <jane@example.com>" >&2\n'
            # The plaintext is a MIME tree of its own: text plus an
            # attachment, which the pager announces like any other.
            "  printf 'Content-Type: multipart/mixed; boundary=\"m\"\\r\\n\\r\\n"
            "--m\\r\\nContent-Type: text/plain\\r\\n\\r\\nthe secret plan\\r\\n"
            "--m\\r\\nContent-Type: application/pdf\\r\\n"
            "Content-Disposition: attachment; filename=\"plan.pdf\"\\r\\n\\r\\n"
            "PDFBYTES\\r\\n--m--\\r\\n' ;;\n"
            "*--detach-sign*)\n"
            "  cat >/dev/null\n"
            '  echo "[GNUPG:] SIG_CREATED D 1 8 00 12 FPR" >&2\n'
            "  printf -- '-----BEGIN PGP SIGNATURE-----\\nAAAA\\n"
            "-----END PGP SIGNATURE-----\\n' ;;\n"
            "*--encrypt*)\n"
            "  cat >/dev/null\n"
            f'  echo "$*" >> {recip_log}\n'
            "  printf -- '-----BEGIN PGP MESSAGE-----\\nBBBB\\n"
            "-----END PGP MESSAGE-----\\n' ;;\n"
            "esac\nexit 0\n"
        )
    os.chmod(gpg, 0o755)
    config = os.path.join(tmp, "config.toml")
    with open(config, "w") as f:
        f.write(f'[pgp]\ncommand = "{gpg}"\n'
                '[[crypt_hooks]]\naddress = "boss@example.com"\n'
                'key = "0xDEADBEEF"\n')
    editor = os.path.join(tmp, "editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "signed body line\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    sent_file = os.path.join(tmp, "sent.eml")
    sendmail = os.path.join(tmp, "sendmail.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    env = base_env(tmp, {
        "RMUT_CONFIG": config,
        "EDITOR": editor,
        "RMUT_SENDMAIL": sendmail,
    })
    r = Rmut(md, env)
    r.expect("Msgs:2", "sealed orders")
    r.keys(b"\r")  # newest = the encrypted message
    r.expect("the secret plan", "decrypted", "good signature from Jane",
             "[-- Attachment #2: plan.pdf --]")
    r.keys(b"i")
    # Compose, pick sign from the security menu, send.
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"bob@example.org\rsigned subject\r")  # editor appends the body
    r.expect("y:Send")  # the compose menu
    r.keys(b"ps")  # p opens the security menu -> sign; the signed
    # output is asserted on the sent file below (cell-diff redraws
    # make the menu's Security value unreliable to screen-scrape)
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="sendmail invoked")
    r.expect("message sent")
    sent = open(sent_file).read()
    assert "Content-Type: multipart/signed" in sent
    assert "micalg=pgp-sha256" in sent
    assert "BEGIN PGP SIGNATURE" in sent
    assert "signed body line" in sent

    # crypt-hook: the boss is encrypted to their key id, not to their
    # address; everyone else (me, on the Fcc copy) stays an address.
    os.truncate(sent_file, 0)
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"boss@example.com\rsealed\r")
    r.expect("y:Send")
    r.keys(b"pe")  # security menu -> encrypt
    r.keys(b"y")
    wait_for(lambda: os.path.exists(recip_log)
             and "--encrypt" in open(recip_log).read(),
             desc="gpg asked to encrypt")
    args = open(recip_log).read()
    assert "--recipient 0xDEADBEEF" in args, args
    assert "--recipient boss@example.com" not in args, args
    r.keys(b"q")  # both messages were already seen, so it quits directly
    r.close()


def scenario_print(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    out = os.path.join(tmp, "printed.txt")
    config = os.path.join(tmp, "config.toml")
    with open(config, "w") as f:
        f.write(f'[mail]\nprint = "cat >> {out}"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": config}))
    r.expect("Msgs:1")
    r.keys(b"p")
    r.expect("Print message?")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(out), desc="print command ran")
    r.expect("printed via")
    printed = open(out).read()
    assert "Subject: Lunch on Friday?" in printed
    assert "Are you free for lunch" in printed
    r.keys(b"q")
    r.close()


def scenario_identities(tmp):
    """R9: recipient/folder identity rules and reverse_name."""
    md = make_maildir(tmp, "md")
    md2 = make_maildir(tmp, "md2")
    # A message addressed to "me" under a display name, for reverse_name.
    with open(os.path.join(md, "cur", "1751790000.9.host:2,S"), "w") as f:
        f.write("From: Jane Doe <jane@example.com>\r\n"
                "To: Boss Me <jarda@example.com>\r\n"
                "Subject: status?\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
                "Message-ID: <rev1@example.com>\r\n\r\nAny update?\r\n")
    write_msgs(md2, ["ci"])
    # A third sibling for Tab completion: md/md2/md3 share a prefix.
    md3 = make_maildir(tmp, "md3")
    with open(os.path.join(md3, "cur", "1751790500.7.host:2,S"), "w") as f:
        f.write("From: Tab Test <tab@example.com>\r\n"
                "To: jarda@example.com\r\n"
                "Subject: Tab landed here\r\nDate: Mon, 6 Jul 2026 11:00:00 +0200\r\n"
                "Message-ID: <tab1@example.com>\r\n\r\nvia completion\r\n")
    sent_file = os.path.join(tmp, "sent-ids.eml")
    sendmail = os.path.join(tmp, "sendmail-ids.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "body-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "ids-config.toml")
    with open(cfg, "w") as f:
        f.write(
            f"""
[identity]
name = "Jarda"
email = "jarda@example.com"
reverse_name = true
[mail]
sendmail = "{sendmail}"
editor = "{editor}"
[[identities]]
recipient = "*@work.example.com"
name = "Jarda Work"
email = "jarda-work@example.com"
[[identities]]
folder = "*md2*"
email = "second@example.com"
"""
        )
    env = base_env(tmp, {"RMUT_CONFIG": cfg})
    r = Rmut(md, env)
    r.expect("Msgs:1")
    # recipient rule: composing to *@work.example.com switches From
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"petr@work.example.com\rreport\ry")
    wait_for(lambda: os.path.exists(sent_file)
             and "From: Jarda Work <jarda-work@example.com>" in open(sent_file).read(),
             desc="recipient identity applied")
    # reverse_name: the reply From is the address the mail came to,
    # with the display name the sender used
    r.keys(b"r\r\r\ry")  # the extra Enter answers the include question
    wait_for(lambda: "From: Boss Me <jarda@example.com>" in open(sent_file).read(),
             desc="reverse_name applied")
    # folder rule: the same compose from md2 uses its identity
    r.keys(b"c")
    r.expect("Open mailbox (Tab completes):")
    r.keys(f"{md2}\r".encode())
    r.expect("CI failed on main")
    r.keys(b"mx@y.example.com\rhello\ry")
    wait_for(lambda: "From: Jarda <second@example.com>" in open(sent_file).read(),
             desc="folder identity applied")
    # bugfix: a pending flag change (reading new mail, N, F) must not
    # block c/y/ctrl+o; it syncs silently on the way out, like q
    r.keys(b"N")
    r.keys(b"c")
    r.expect("Open mailbox (Tab completes):", absent=("pending changes",))
    r.keys(f"{md}\r".encode())
    r.expect("status?")
    wait_for(lambda: all("S" not in f.split(":2,")[-1]
                         for f in os.listdir(os.path.join(md2, "cur"))),
             desc="flag change written when switching away")
    # Tab at the mailbox prompt: empty opens the folder browser, a
    # prefix completes and repeated Tab cycles the candidates
    r.keys(b"c")
    r.expect("Open mailbox (Tab completes):")
    r.keys(b"\t")
    r.expect("j/k:Move Enter:Open")
    r.keys(b"q")
    r.keys(b"c")
    r.keys(os.path.join(tmp, "md").encode() + b"\t")
    r.expect("match 1/3 (Tab cycles)")
    r.keys(b"\t\t\r")  # cycle md -> md2 -> md3, open it
    r.expect("Tab landed here")
    r.keys(b"q")
    r.close()


def scenario_edit_headers(tmp):
    """The default (edit_headers off, like mutt): the editor sees only
    the body; headers come from the prompts, attachments via the send
    prompt."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-eh.eml")
    sendmail = os.path.join(tmp, "sendmail-eh.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    seen = os.path.join(tmp, "editor-saw.txt")
    editor = os.path.join(tmp, "eh-editor.sh")
    with open(editor, "w") as f:
        f.write(f'#!/bin/sh\ncp "$1" {seen}\nprintf "only the body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    attachment = os.path.join(tmp, "note.txt")
    with open(attachment, "w") as f:
        f.write("attach me\n")
    cfg = os.path.join(tmp, "eh-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[identity]\nname = "Jarda"\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"petr@example.com\rheadless subject\r")
    r.expect("y:Send")  # the compose menu
    saw = open(seen).read()
    assert "To:" not in saw and "Subject:" not in saw, f"headers leaked: {saw!r}"
    r.keys(b"a")
    r.keys(attachment.encode() + b"\r")
    r.expect("text/plain")  # listed in the compose menu
    r.keys(b"\r")  # Enter views the selected entry: the body text
    r.expect("Message body", "only the body")
    r.keys(b"q")  # back to the menu
    r.keys(b"j\r")  # and the attached file
    r.expect("attach me")
    r.keys(b"q")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="sendmail ran")
    sent = open(sent_file).read()
    assert "To: petr@example.com" in sent, sent
    assert "Subject: headless subject" in sent, sent
    assert "only the body" in sent, sent
    assert 'filename="note.txt"' in sent, sent
    r.keys(b"q")
    r.close()


def scenario_line_editor(tmp):
    """R23: mid-line editing (ctrl+a/ctrl+d, arrows) and per-kind
    history (Up recalls) at the prompts; asserts on the sent files."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-le.eml")
    sendmail = os.path.join(tmp, "sendmail-le.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "le-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "le body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "le-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")
    # ctrl+a jumps home, ctrl+d deletes the stray leading char
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"Xpetr@example.com\x01\x04\r")
    r.keys(b"first subject\r")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "To: petr@example.com" in open(sent_file).read(),
             desc="ctrl+a/ctrl+d edited address")
    # arrows: insert the missing char mid-line (jne -> jane)
    r.keys(b"m")
    r.keys(b"jne@example.com" + b"\x1b[D" * 14 + b"a\r")
    r.keys(b"arrowmail\r")
    r.keys(b"y")
    wait_for(lambda: "To: jane@example.com" in open(sent_file).read()
             and "Subject: arrowmail" in open(sent_file).read(),
             desc="arrow-edited address sent")
    # history: Up recalls jane (newest), Up again petr
    r.keys(b"m")
    r.keys(b"\x1b[A\x1b[A\r")
    r.keys(b"histmail\r")
    r.keys(b"y")
    wait_for(lambda: "Subject: histmail" in open(sent_file).read(),
             desc="history-recalled send")
    assert open(sent_file).read().count("To: petr@example.com") == 2
    r.keys(b"q")
    r.close()


def scenario_pager_search(tmp):
    """R24: / inside the pager searches the displayed text
    (case-insensitive), n/N step through the hits and wrap with a
    status note; a miss reports Not found."""
    md = make_maildir(tmp, "md")
    lines = [f"filler {i:02}" for i in range(1, 41)]
    lines[29] = "the target alpha"
    lines[37] = "the target beta"
    with open(os.path.join(md, "cur/1751790000.9.host:2,S"), "w") as f:
        f.write(
            "From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: A long report\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
            "Message-ID: <long@example.com>\r\n\r\n"
            + "\r\n".join(lines) + "\r\n"
        )
    # An older second message, for the cross-message n behavior below.
    with open(os.path.join(md, "cur/1751789000.8.host:2,S"), "w") as f:
        f.write(
            "From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: Earlier note\r\nDate: Mon, 6 Jul 2026 09:00:00 +0200\r\n"
            "Message-ID: <early@example.com>\r\n\r\n"
            "gamma filler\r\nthe target gamma\r\n"
        )
    r = Rmut(md, base_env(tmp))
    r.expect("A long report")
    r.keys(b"\r")
    r.expect("filler 01")  # the hits start off-screen (40 body lines, 28 rows)
    r.keys(b"/")
    r.expect("Search for:")
    r.keys(b"TARGET\r")  # case-insensitive, like the index patterns
    r.expect("the target alpha")
    r.keys(b"n")
    r.expect("the target beta")
    r.keys(b"n")  # past the last hit: around to the first
    r.expect("Search wrapped to top.")
    # Enter (next-line) clears the status (else "bottom." diffs against
    # "top." and only changed cells reach the pty); the first N
    # re-finds alpha above.
    r.keys(b"\rNN")  # backwards from the first hit: around to the last
    r.expect("Search wrapped to bottom.")
    r.keys(b"/")
    r.keys(b"zebra\r")
    r.expect("Not found.")
    # Crossing to another message ends the search, like mutt: n
    # re-prompts, prefilled with the last pattern.
    r.keys(b"k")  # previous undeleted message
    r.expect("gamma filler")
    r.buf = ""
    r.keys(b"n")
    r.expect("zebra▁")  # the prompt, prefilled, cursor at the end
    r.keys(b"\x15absent\r")  # ctrl+u clears the prefill; new search runs
    r.expect("Not found.")
    # R80: \\ toggles the search highlighting off and back on.
    r.keys(b"/gamma\r")
    r.expect("the target gamma")
    r.keys(b"\\")
    r.expect("search highlighting off")
    r.keys(b"\\")
    r.expect("search highlighting on")
    r.keys(b"q")
    r.keys(b"q")
    r.close()


def scenario_triage(tmp):
    """R25: Tab/Alt+Tab jump to the next/previous new-or-unread
    message (wrapping), and D/U/T/ctrl+t apply delete/undelete/tag/
    untag to every pattern match; verified on disk after the purge."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "ci"])  # both seen
    with open(os.path.join(md, "cur/1751882400.7.host:2,"), "w") as f:
        f.write(
            "From: Ops Bot <ops@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: Disk almost full\r\nDate: Tue, 7 Jul 2026 12:00:00 +0200\r\n"
            "Message-ID: <msg7@example.com>\r\n\r\n"
            "Please replace the disk before nine.\r\n"
        )
    with open(os.path.join(md, "cur/1752022800.8.host:2,"), "w") as f:
        f.write(
            "From: Night Runner <night@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: Night build done\r\nDate: Thu, 9 Jul 2026 01:00:00 +0200\r\n"
            "Message-ID: <msg8@example.com>\r\n\r\n"
            "The night build finished green.\r\n"
        )

    def on_disk(base):
        return any(
            f.startswith(base)
            for sub in ("cur", "new")
            for f in os.listdir(os.path.join(md, sub))
        )

    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:4", "Disk almost full", "Night build done")
    # No new mail, so the index starts on the last message (night,
    # unread). Tab wraps around to the other unread one (urgent).
    r.keys(b"\t\r")
    r.expect("Please replace the disk before nine.")
    r.keys(b"q")
    r.keys(b"\t\r")  # forward, no wrap: night is still unread
    r.expect("The night build finished green.")
    r.keys(b"q")
    # Mark jane unread again, jump back to her with Alt+Tab from the end.
    r.keys(b"=jN*")
    r.keys(b"\x1b\t\r")
    r.expect("Are you free for lunch on Friday?")
    r.keys(b"q")
    # Tag everything, untag the ops message, delete the tagged rest.
    r.keys(b"T")
    r.keys(b"!~s zzznothing\r")
    r.expect("4 tagged")
    r.keys(b"\x14")  # ctrl+t untag-pattern
    r.keys(b"~f ops\r")
    r.expect("1 untagged")
    r.keys(b";d")  # deletes the 3 still-tagged (ci, jane, night)
    # Undelete jane by pattern, re-delete the ops message by pattern.
    r.keys(b"U")
    r.keys(b"~s lunch\r")
    r.keys(b"D")
    r.keys(b"~f ops\r")
    r.settle()
    r.keys(b"$y")  # purge ci, night, urgent
    wait_for(
        lambda: not on_disk("1751750100.3")
        and not on_disk("1752022800.8")
        and not on_disk("1751882400.7"),
        desc="pattern-deleted messages purged",
    )
    assert on_disk("1751790000.1"), "undelete-pattern should have saved jane"
    r.keys(b"q")
    r.close()


def scenario_odds(tmp):
    """R26: -R read-only (nothing written, %r shows %), the browser
    descends/creates maildirs, Q queries addresses into a compose,
    attachment pipe/print, and a %>/%P status_format."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["petr"])  # new/, stays new under -R

    # Read-only pass: viewing must not mark read, d must refuse.
    r = Rmut(md, base_env(tmp), args=("-R",))
    r.expect("rmut%:")  # the %r read-only mark in the status line
    r.keys(b"\r")
    r.expect("sejdeme se zítra v 9:00")
    r.keys(b"q")
    r.keys(b"d")
    r.expect("Mailbox is read-only.")
    r.keys(b"q")
    r.close()
    time.sleep(0.3)
    assert os.listdir(os.path.join(md, "new")), "-R must not move new mail"

    # Normal pass, with a query_command, a print command, and a
    # right-aligned status line ending in %P.
    query = os.path.join(tmp, "query.sh")
    with open(query, "w") as f:
        f.write('#!/bin/sh\nprintf "found:\\njane.q@example.com\\tJane Query\\n"\n')
    os.chmod(query, 0o755)
    sent_file = os.path.join(tmp, "sent-odds.eml")
    sendmail = os.path.join(tmp, "sendmail-odds.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "odds-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "odds body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "odds-config.toml")
    with open(cfg, "w") as f:
        f.write(
            f'[identity]\nemail = "jarda@example.com"\n'
            f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
            f'query_command = "{query} %s"\n'
            f'print = "cat >> {os.path.join(tmp, "printed.out")}"\n'
            f'[ui]\nstatus_format = "rmutST %f m:%m%>*%P"\n'
        )
    # An attachment to pipe and print.
    with open(os.path.join(md, "cur/1751883000.6.host:2,S"), "w") as f:
        f.write(
            "From: Sender <sender@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: With data\r\nDate: Tue, 7 Jul 2026 13:00:00 +0200\r\n"
            "Message-ID: <att@example.com>\r\nMIME-Version: 1.0\r\n"
            'Content-Type: multipart/mixed; boundary="mx"\r\n\r\n'
            "--mx\r\nContent-Type: text/plain\r\n\r\nsee attachment\r\n"
            "--mx\r\nContent-Type: application/octet-stream\r\n"
            'Content-Disposition: attachment; filename="data.bin"\r\n'
            "Content-Transfer-Encoding: base64\r\n\r\n"
            + base64.b64encode(b"odds-payload-42\n").decode() + "\r\n--mx--\r\n"
        )
    # A directory tree for the browser: tree/proj/inbox2 (a maildir
    # holding one message).
    inbox2 = make_maildir(tmp, "tree/proj/inbox2")
    write_msgs(inbox2, ["jane"])

    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("rmutST", "m:2", "**all")  # %> fill and %P position
    # Attachment pipe and print.
    r.keys(b"*v")  # newest message (With data), attachment menu
    r.expect("data.bin")
    r.keys(b"j|")
    r.keys(f"cat > {tmp}/part.out\r".encode())
    wait_for(
        lambda: os.path.exists(f"{tmp}/part.out")
        and open(f"{tmp}/part.out", "rb").read() == b"odds-payload-42\n",
        desc="piped attachment bytes",
    )
    r.keys(b"py")  # print part, confirmed
    wait_for(
        lambda: os.path.exists(f"{tmp}/printed.out")
        and b"odds-payload-42" in open(f"{tmp}/printed.out", "rb").read(),
        desc="printed attachment bytes",
    )
    r.keys(b"q")
    # Browser: browse the tree, descend, create a maildir, open one.
    r.keys(b"y")
    r.keys(b"c\x15")  # browse prompt, clear the prefill
    r.keys(f"{tmp}/tree\r".encode())
    r.expect("proj/")
    r.keys(b"j\r")  # descend into proj
    r.expect("proj/inbox2")
    r.keys(b"C")
    r.keys(b"fresh\r")
    r.expect("proj/fresh")  # created and re-listed
    r.keys(b"j\r")  # fresh sorts first: open the empty new maildir
    r.expect("No mail in mailbox.")
    # Query menu: pick the result, send to it.
    r.keys(b"Q")
    r.keys(b"jan\r")
    r.expect("Jane Query <jane.q@example.com>")
    r.keys(b"\r")  # compose to the pick; To is prefilled
    r.keys(b"\r")  # accept To
    r.keys(b"query subject\r")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(
        lambda: os.path.exists(sent_file)
        and "To: Jane Query <jane.q@example.com>" in open(sent_file).read()
        and "Subject: query subject" in open(sent_file).read(),
        desc="query-composed mail sent",
    )
    r.keys(b"q")
    r.close()


def scenario_pager_quotes(tmp):
    """R27: quoted-line handling in the pager: S skips past the
    quoted block, T hides quoted lines (the tail behind a long quote
    becomes visible), and quoted text is tinted (raw ANSI check)."""
    md = make_maildir(tmp, "md")
    quotes1 = "".join(f"> quoted filler {i:02}\r\n" for i in range(1, 36))
    with open(os.path.join(md, "cur/1751790000.1.host:2,S"), "w") as f:
        f.write(
            "From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: Quote heavy\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
            "Message-ID: <qh@example.com>\r\n\r\n"
            "intro-line-one\r\n" + quotes1 + "tail-after-quotes\r\n"
        )
    quotes2 = "".join(f"> block {i:02}\r\n" for i in range(1, 36))
    with open(os.path.join(md, "cur/1751876400.2.host:2,S"), "w") as f:
        f.write(
            "From: Petr <petr@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: Skip test\r\nDate: Tue, 7 Jul 2026 10:00:00 +0200\r\n"
            "Message-ID: <st@example.com>\r\n\r\n"
            "start-line\r\n" + quotes2 + "after-skip-target\r\n"
        )
    r = Rmut(md, base_env(tmp))
    r.expect("Quote heavy", "Skip test")
    r.keys(b"\r")  # newest: Skip test
    r.expect("start-line")
    # The default theme tints quoted lines cyan (crossterm emits it as
    # 38;5;6 on the default background), so check the raw output, since
    # assertions otherwise strip ANSI.
    assert "\x1b[38;5;6;49m" in r.buf, "quoted lines should be tinted"
    r.keys(b"S")  # skip past the quoted block: its end scrolls to top
    r.expect("after-skip-target")
    r.keys(b"K")  # previous message (Quote heavy)
    r.expect("intro-line-one")
    r.keys(b"T")  # hide quoted: the tail fits on screen now
    r.expect("tail-after-quotes")
    r.keys(b"q")
    r.keys(b"q")
    r.close()


def scenario_pager_polish(tmp):
    """R28: ignore/unignore/hdr_order weed and order the brief header
    view, [pager] format renders the bottom line (with %> fill and
    %P), wrap narrows the text, tilde pads below end-of-message."""
    md = make_maildir(tmp, "md")
    with open(os.path.join(md, "cur/1751790000.1.host:2,S"), "w") as f:
        f.write(
            "From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
            "Subject: Order test\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
            "X-Topic: budget\r\nMessage-ID: <ot@example.com>\r\n\r\n"
            "alpha beta gamma delta epsilon zeta\r\nplain tail\r\n"
        )
    cfg = os.path.join(tmp, "polish-config.toml")
    with open(cfg, "w") as f:
        f.write(
            '[pager]\nignore = ["*"]\nunignore = ["subject", "x-topic"]\n'
            'hdr_order = ["x-topic", "subject"]\n'
            'format = "PGRFMT %C/%m %s%>-%P"\nwrap = 20\ntilde = true\n'
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Order test")
    r.keys(b"\r")
    # Weeded brief view: X-Topic surfaced and ordered before Subject.
    r.expect("X-Topic: budget", "plain tail")
    s = squash(r.buf)
    assert s.index("X-Topic:budget") < s.index("Subject:Ordertest"), "hdr_order"
    # $wrap at 20 columns: the long line breaks with a + marker.
    r.expect("+delta epsilon zeta")
    # $tilde pads the empty rows; the custom format fills with - up
    # to the right-aligned position.
    r.expect("~~~~~", "PGRFMT 1/1", "--100%")
    r.keys(b"q")
    r.keys(b"q")
    r.close()


def scenario_notmuch(tmp):
    """R18: X runs notmuch (stubbed here) and opens the hits as a
    read-only virtual mailbox: view and copy work, delete refuses,
    and leaving the view restores a writable mailbox."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["ci"])
    store = make_maildir(tmp, "store")  # where the "database" hits live
    write_msgs(store, ["jane", "alice"])
    bindir = os.path.join(tmp, "bin")
    os.makedirs(bindir)
    with open(os.path.join(bindir, "notmuch"), "w") as f:
        f.write(f"#!/bin/sh\nls {store}/cur/* | sort\n")
    os.chmod(os.path.join(bindir, "notmuch"), 0o755)
    dest = make_maildir(tmp, "dest")
    r = Rmut(md, base_env(tmp, {"PATH": f"{bindir}:{os.environ['PATH']}"}))
    r.expect("CI failed on main")
    r.keys(b"X")
    r.expect("Notmuch query:")
    r.keys(b"from:jane\r")
    r.expect("notmuch:", "2 matching message(s)")
    r.keys(b"\r")  # newest hit (alice) opens through the symlink
    r.expect("Count me in too!")
    r.keys(b"d")  # the virtual mailbox never writes
    r.expect("Mailbox is read-only.")
    r.keys(b"q")
    r.keys(b"C")  # copying out still works, the original is read
    r.keys(f"{dest}\r".encode())
    wait_for(
        lambda: any(os.listdir(os.path.join(dest, s)) for s in ("cur", "new")),
        desc="message copied out of the notmuch view",
    )
    # Back in a real mailbox the delete works again (read-only was
    # only the virtual view's).
    r.keys(b"c")
    r.keys(f"{md}\r".encode())
    r.keys(b"d$y")
    wait_for(
        lambda: not any(
            "1751750100" in f
            for s in ("cur", "new")
            for f in os.listdir(os.path.join(md, s))
        ),
        desc="delete works after leaving the notmuch view",
    )
    r.keys(b"q")
    r.close()


def scenario_compose_round2(tmp):
    """R29: fast_reply skips the To/Subject prompts, the compose menu
    edits an attachment's description (d), content-type (ctrl+t) and
    the Fcc (f), mime_forward=ask asks, autoedit goes straight to the
    editor; verified in the sent files and the Fcc maildir."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    with open(os.path.join(tmp, "notes.txt"), "w") as f:
        f.write("attach body text\n")
    fcc = make_maildir(tmp, "fcc")
    sent_file = os.path.join(tmp, "sent-c2.eml")
    sendmail = os.path.join(tmp, "sendmail-c2.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "c2-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "reply body here\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "c2-config.toml")
    with open(cfg, "w") as f:
        f.write(
            f'[identity]\nemail = "jarda@example.com"\n'
            f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
            f'fast_reply = true\nforward = "ask"\n'
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")
    r.keys(b"r")  # fast_reply: straight to the include question
    r.expect("Include")
    r.keys(b"\r")
    r.expect("y:Send")
    r.keys(b"a")
    r.keys(f"{tmp}/notes.txt\r".encode())
    r.settle()
    r.keys(b"j")  # onto the attachment row
    r.keys(b"d")
    r.settle()
    r.keys(b"quarterly data\r")
    r.expect("(quarterly data)")
    r.settle()
    r.keys(b"\x14\x15")  # ctrl+t, clear the guessed type
    r.settle()
    r.keys(b"application/x-custom\r")
    r.expect("application/x-custom")
    r.settle()
    r.keys(b"f\x15")  # Fcc, clear the default
    r.settle()
    r.keys(f"{fcc}\r".encode())
    r.settle()
    r.keys(b"y")
    wait_for(
        lambda: os.path.exists(sent_file)
        and "To: Jane Doe <jane@example.com>" in open(sent_file).read()
        and "Subject: Re: Lunch on Friday?" in open(sent_file).read()
        and "Content-Type: application/x-custom" in open(sent_file).read()
        and "Content-Description: quarterly data" in open(sent_file).read(),
        desc="fast reply sent with edited attachment fields",
    )
    wait_for(
        lambda: any(os.listdir(os.path.join(fcc, s)) for s in ("cur", "new")),
        desc="Fcc copy in the chosen maildir",
    )
    # mime_forward = ask: the forward flow asks, yes attaches whole.
    r.keys(b"f")
    r.expect("To:")
    r.keys(b"petr@example.com\r")  # fast_reply skips the Subject prompt
    # The "Forward as attachment?" question comes next (its cells
    # overlap the To echo, so assert the outcome on disk instead).
    r.keys(b"y")
    r.settle()
    r.keys(b"y")  # send from the compose menu
    wait_for(
        lambda: "message/rfc822" in open(sent_file).read(),
        desc="ask-forward attached the original",
    )
    r.keys(b"q")
    r.close()

    # autoedit (with edit_headers): no prompts at all, the editor sets
    # the headers itself.
    sent2 = os.path.join(tmp, "sent-auto.eml")
    sendmail2 = os.path.join(tmp, "sendmail-auto.sh")
    with open(sendmail2, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent2}\nexit 0\n")
    os.chmod(sendmail2, 0o755)
    editor2 = os.path.join(tmp, "auto-editor.sh")
    with open(editor2, "w") as f:
        f.write(
            '#!/bin/sh\nsed -i "s/^To:.*/To: auto@example.com/" "$1"\n'
            'sed -i "s/^Subject:.*/Subject: automatic/" "$1"\n'
            'printf "auto body\\n" >> "$1"\n'
        )
    os.chmod(editor2, 0o755)
    cfg2 = os.path.join(tmp, "auto-config.toml")
    with open(cfg2, "w") as f:
        f.write(
            f'[identity]\nemail = "jarda@example.com"\n'
            f'[mail]\nsendmail = "{sendmail2}"\neditor = "{editor2}"\n'
            f'autoedit = true\nedit_headers = true\n'
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg2}))
    r.expect("Msgs:1")
    r.keys(b"m")  # no To/Subject prompts: editor, then the menu
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(
        lambda: os.path.exists(sent2)
        and "To: auto@example.com" in open(sent2).read()
        and "Subject: automatic" in open(sent2).read()
        and "auto body" in open(sent2).read(),
        desc="autoedit message sent",
    )
    r.keys(b"q")
    r.close()


def scenario_mutt_flow(tmp):
    """Mutt-default behaviors: the Reply-To/include/no-subject
    questions, e edits the raw message, Space past the end advances,
    and unread new mail ages to O on quit (mark_old)."""
    md = make_maildir(tmp, "md")
    with open(os.path.join(md, "cur", "1751790000.9.host:2,S"), "w") as f:
        f.write("From: Jane Doe <jane@example.com>\r\n"
                "Reply-To: list@example.com\r\n"
                "To: jarda@example.com\r\n"
                "Subject: via list\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
                "Message-ID: <rt1@example.com>\r\n\r\nshort body\r\n")
    write_msgs(md, ["petr"])  # read via the pager below
    # The newest message stays untouched: the mark_old candidate.
    with open(os.path.join(md, "new", "1751953000.5.host"), "w") as f:
        f.write("From: quiet@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: never read\r\nDate: Wed, 8 Jul 2026 09:00:00 +0200\r\n"
                "Message-ID: <mf5@example.com>\r\n\r\nnothing to see\r\n")
    sent_file = os.path.join(tmp, "sent-mf.eml")
    sendmail = os.path.join(tmp, "sendmail-mf.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "mf-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "edited-by-e2e\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "mf-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:3")
    # $reply_to ask-yes, then $abort_nosubject ask-yes (Enter = abort)
    r.keys(b"=r")
    r.expect("Reply to list@example.com? (y/n):")
    r.keys(b"y")
    r.keys(b"\r")      # accept To = list@example.com
    r.keys(b"\x15\r")  # clear the prefilled subject -> the question
    r.expect("No subject, abort? (y/n):")
    r.keys(b"\r")
    r.expect("aborted (no subject)")
    # again: n at the Reply-To question replies to From, n at the
    # include question leaves the original out
    r.keys(b"r")
    r.keys(b"n")       # -> To prefilled with the From header
    r.keys(b"\r\r")    # accept To and subject
    r.expect("Include message in reply? (y/n):")
    r.keys(b"n")
    r.expect("y:Send")  # the compose menu
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="reply sent")
    sent = open(sent_file).read()
    assert "To: Jane Doe <jane@example.com>" in sent, sent
    assert "list@example.com" not in sent.split("\n\n")[0], sent
    assert "> short body" not in sent, sent
    # e edits the raw message in place (the editor appends a body line)
    r.keys(b"=e")
    r.expect("message edited")
    r.keys(b"\r")
    r.expect("edited-by-e2e")
    # $pager_stop = no: Space at the end opens the next message
    r.keys(b" ")
    r.expect("sejdeme se")
    r.keys(b"i")
    # $mark_old: the untouched new message ages on quit, moved to
    # cur/ without gaining the seen flag
    r.keys(b"q")
    wait_for(lambda: "1751953000.5.host:2," in os.listdir(os.path.join(md, "cur")),
             desc="unread new message aged to old")
    assert os.listdir(os.path.join(md, "new")) == []
    r.close()


def scenario_message_commands(tmp):
    """R8: | pipe, C copy, Attach: pseudo-headers, b bounce, e resend."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-cmds.eml")
    args_file = os.path.join(tmp, "sendmail-args")
    sendmail = os.path.join(tmp, "sendmail-cmds.sh")
    with open(sendmail, "w") as f:
        f.write(f'#!/bin/sh\necho "$@" >> {args_file}\ncat >> {sent_file}\nexit 0\n')
    os.chmod(sendmail, 0o755)
    blob = bytes(range(256))
    with open(os.path.join(tmp, "blob.bin"), "wb") as f:
        f.write(blob)
    editor = os.path.join(tmp, "attach-editor.sh")
    with open(editor, "w") as f:
        f.write(f'#!/bin/sh\nprintf "hello attach\\n" >> "$1"\n'
                f'sed -i "1a Attach: {tmp}/blob.bin raw bytes" "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "cmds-config.toml")
    with open(cfg, "w") as f:
        # the editor script writes an Attach: pseudo-header into the
        # draft's header block, so it must be in the buffer
        f.write("[mail]\nedit_headers = true\n")
    r = Rmut(md, base_env(tmp, {"EDITOR": editor, "RMUT_SENDMAIL": sendmail,
                                "RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")
    # | pipes the raw message to a shell command
    piped = os.path.join(tmp, "piped.eml")
    r.keys(b"|")
    r.expect("Pipe to command:")
    r.keys(f"cat > {piped}\r".encode())
    r.expect("piped to")
    assert "Message-ID: <msg1@example.com>" in open(piped).read()
    # C copies without marking the original deleted
    copy_dir = os.path.join(tmp, "copies")
    r.keys(b"C")
    r.expect("Copy to mailbox:")
    r.keys(f"{copy_dir}\r".encode())
    r.expect("copied to", absent=["Del:1"])
    copies = [p for sub in ("cur", "new")
              for p in os.listdir(os.path.join(copy_dir, sub))]
    assert len(copies) == 1, copies
    # an Attach: pseudo-header becomes a base64 multipart/mixed part
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"petr@example.com\rwith attachment\r")
    r.expect("blob.bin")  # the compose menu lists the editor's Attach:
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="sendmail invoked")
    r.expect("message sent")
    sent = open(sent_file).read()
    assert "Content-Type: multipart/mixed" in sent
    assert 'filename="blob.bin"' in sent
    assert "Content-Description: raw bytes" in sent
    assert base64.b64encode(blob).decode()[:76] in sent
    assert "Attach:" not in sent  # the pseudo-header never leaves rmut
    # b bounces the original with a Resent-* block, rcpts on the argv
    r.keys(b"b")
    r.expect("Bounce message to:")
    r.keys(b"petr@example.com\r")
    r.expect("petr@example.com? (y/n):")  # the confirmation prompt
    r.keys(b"y")
    wait_for(lambda: "Resent-To: petr@example.com" in open(sent_file).read(),
             desc="bounce sent")
    r.expect("message bounced to petr@example.com")
    sent = open(sent_file).read()
    assert "Resent-From: jarda@example.com" in sent
    assert "Subject: Lunch on Friday?" in sent  # original kept as-is
    assert "-oi petr@example.com" in open(args_file).read()
    # e resends: the message becomes a fresh draft through the editor
    # (the same editor script attaches the blob again, hence 2 mixed)
    r.keys(b"\x1bey")  # resend is Alt+e now; e edits the raw message
    wait_for(
        lambda: open(sent_file).read().count("Content-Type: multipart/mixed") == 2,
        desc="resent message delivered",
    )
    r.keys(b"q")
    r.close()


def scenario_tag_save_sort(tmp):
    """Config sort/date_format, tagging with ;-prefix, save to mailbox."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci"])
    save_dir = os.path.join(tmp, "archive")
    config = os.path.join(tmp, "config.toml")
    with open(config, "w") as f:
        f.write('[index]\nsort = "reverse-date"\ndate_format = "%Y|%m"\n\n'
                f'[mail]\nsave = "{save_dir}"\n\n'
                '[ui]\nstatus_format = '
                '"---rmut: %f [Msgs:%?M?%M/?%m %?t?Tagged:%t&no tags?'
                '%?d? Del:%d?] (sort:%s)"\n\n'
                '[[color_index]]\npattern = "~f jane"\nfg = "yellow"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": config}))
    r.expect("Msgs:3", "(sort:date-rev)", "2026|07", "no tags")
    r.keys(b"=")    # first entry (newest, reverse-date)
    r.keys(b"t")    # tag the first; t advances
    r.expect("Tagged:1")  # the custom status_format counts it
    r.keys(b"t")    # tag the second
    r.keys(b";d")   # delete all tagged
    # mutt's $delete_untag: the marks take the tags off with them.
    r.expect("applied to 2", "Del:2", "no tags")
    r.keys(b"=tt")  # tag both again (t advances)
    r.expect("Tagged:2")
    r.keys(b";u")   # and undelete them again (Del:1 below proves it)
    r.keys(b"s")    # save the selected message
    r.expect("Save to mailbox:")
    r.keys(b"\r")   # accept the configured default
    r.expect("saved to", "Del:1")
    msgs = [p for sub in ("cur", "new")
            for p in os.listdir(os.path.join(save_dir, sub))]
    assert len(msgs) == 1, msgs
    r.keys(b"q")    # pending deletion -> purge prompt, straight away
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"n")    # keep it marked deleted; flags are written, rmut quits
    r.close()


def scenario_import_muttrc(tmp):
    """--import-muttrc prints reviewable TOML (no pty needed)."""
    muttrc = os.path.join(tmp, "muttrc")
    with open(muttrc, "w") as f:
        f.write(
            "set realname = \"Jan Novak\"\n"
            "set from = jarda@example.com\n"
            "set folder = ~/Mail\n"
            "set spoolfile = +inbox\n"
            "bind index \\Cd delete-message\n"
            "set imap_pass = topsecret\n"
            "macro index x \"<limit>~N<enter>\"\n"
            "set sidebar_visible = yes\n"
            "set sidebar_width = 24\n"
            "set imap_idle = yes\n"
            "set header_cache = ~/.cache/mutt\n"
        )
    proc = subprocess.run(
        [RMUT, "--import-muttrc", muttrc],
        capture_output=True, text=True, timeout=10,
    )
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout
    assert 'name = "Jan Novak"' in out
    assert 'email = "jarda@example.com"' in out
    assert 'mailboxes = ["~/Mail/inbox"]' in out
    assert 'delete = "ctrl+d"' in out
    assert "macro index x" in out  # surfaced as a comment
    assert "topsecret" not in out  # passwords never leak
    assert "(redacted)" in out
    # R57: what rmut has is imported, and what it answers its own way
    # says so rather than reading as a hole.
    assert "[sidebar]" in out and "width = 24" in out, out
    assert "rmut IDLEs whenever the server offers it" in out, out
    assert "# rmut does these its own way:" in out, out
    not_imported = out.split("# not imported:")[-1] if "# not imported:" in out else ""
    assert "sidebar_visible" not in not_imported, out
    assert "header_cache" not in not_imported, out

    # The parity fixture: the muttrc the roadmap measures itself
    # against. What it still cannot carry is the list the next round
    # works from, so the count is a ratchet and not a claim.
    parity = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          "muttrc-parity.rc")
    proc = subprocess.run(
        [RMUT, "--import-muttrc", parity],
        capture_output=True, text=True, timeout=10,
    )
    assert proc.returncode == 0, proc.stderr
    out = proc.stdout
    unclaimed = [l for l in out.split("# not imported:")[-1].splitlines()
                 if l.startswith("#   set")]
    assert len(unclaimed) == 6, "\n".join(unclaimed)
    # Settings now, or answered by what rmut already does. The ratchet
    # a round has to move: R72 claimed assumed_charset and the crypt
    # reply defaults, leaving six real holes.
    for name in ("signature", "sig_dashes", "forward_quote", "reply_to",
                 "abort_nosubject", "abort_unmodified", "honor_followup_to",
                 "mark_old", "beep_new", "wait_key", "print",
                 "reverse_realname", "timeout", "crypt_replysign",
                 "crypt_replyencrypt", "assumed_charset"):
        assert not any(f"set {name} " in l for l in unclaimed), name


def scenario_attachment_pager(tmp):
    """The pager renders the whole MIME tree (text attachments inline
    under [-- Attachment #N --] markers); the part view returns to
    the attachment menu on space past the end and refuses message
    motion; in the message pager j moves to the next undeleted
    message (mutt's pager default)."""
    md = make_maildir(tmp, "md")
    with open(os.path.join(md, "cur", "1751790000.1.host:2,S"), "w") as f:
        f.write("From: jane@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: with attachment\r\n"
                "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
                "Message-ID: <ap1@example.com>\r\nMIME-Version: 1.0\r\n"
                "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n"
                "--b\r\nContent-Type: text/plain\r\n\r\nmain body here\r\n"
                "--b\r\nContent-Type: text/plain; name=\"notes.txt\"\r\n"
                "Content-Disposition: attachment; filename=\"notes.txt\"\r\n\r\n"
                "attached notes text\r\n--b--\r\n")
    with open(os.path.join(md, "cur", "1751790001.2.host:2,S"), "w") as f:
        f.write("From: jane@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: decoy next\r\n"
                "Date: Mon, 6 Jul 2026 11:00:00 +0200\r\n"
                "Message-ID: <ap2@example.com>\r\n\r\ndecoy body\r\n")
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:2")
    r.keys(b"k\r")  # up to the older message, open it
    # The whole tree renders: body, marker block, attachment inline.
    r.expect("main body here", "[-- Attachment #2: notes.txt --]",
             "attached notes text")
    r.keys(b"v")
    r.expect("Parts:2")
    r.keys(b"j\r")  # view the attachment part
    r.expect("Content-Type: text/plain")
    r.keys(b"j")    # message motion refuses in a part view
    r.expect("Not available in this menu.", absent=("decoy body",))
    r.keys(b" ")    # space past the end of the short part...
    r.keys(b"s")    # ...lands back in the menu, where s asks for a file
    r.expect("Save to file:", absent=("decoy body",))
    r.keys(b"\x1b")  # cancel the prompt (alone: a chaser would read as alt+)
    r.settle(0.3)
    r.keys(b"q")     # menu -> the message pager
    r.keys(b"j")     # mutt's pager j: open the next undeleted message
    r.expect("decoy body")
    r.close()



def scenario_attach_mailcap(tmp):
    """The attachment menu's mailcap views: m (view-mailcap) runs the
    part's mailcap entry over a temp file, T (view-text) shows the
    decoded bytes whatever the type, and Enter on a non-text part
    with no [filters] entry falls through to mailcap, then to text
    with mutt's complaint (view-attach's order)."""
    md = make_maildir(tmp, "md-attmc")
    with open(os.path.join(md, "cur", "1751790000.1.host:2,S"), "w") as f:
        f.write("From: jane@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: odd attachments\r\n"
                "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
                "Message-ID: <am1@example.com>\r\nMIME-Version: 1.0\r\n"
                "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n"
                "--b\r\nContent-Type: text/plain\r\n\r\nmain body here\r\n"
                "--b\r\nContent-Type: application/x-blob; name=\"blob.dat\"\r\n"
                "Content-Disposition: attachment; filename=\"blob.dat\"\r\n\r\n"
                "fake blob payload\r\n"
                "--b\r\nContent-Type: video/mpeg\r\n\r\n"
                "not a film\r\n--b--\r\n")
    mailcap = os.path.join(tmp, "attach-mailcap")
    with open(mailcap, "w") as f:
        f.write("application/x-blob; printf 'VIEWED ' && cat %s; copiousoutput\n")
    r = Rmut(md, base_env(tmp, {"MAILCAPS": mailcap}))
    r.expect("Msgs:1")
    r.keys(b"v")
    r.expect("Parts:3")
    r.keys(b"jT")   # the decoded bytes as plain text first
    r.expect("fake blob payload", absent=("VIEWED",))
    r.keys(b"q")
    r.keys(b"m")    # the blob through its mailcap viewer
    r.expect("VIEWED fake blob payload")
    r.keys(b"q")
    r.keys(b"\r")   # Enter falls through to mailcap for this type
    r.expect("VIEWED fake blob payload")
    r.keys(b"q")
    r.keys(b"j\r")  # no entry for video/mpeg: the complaint, then text
    r.expect("no matching mailcap entry found, viewing as text",
             "not a film")
    r.keys(b"q")
    r.expect("Parts:3")
    r.close()


def scenario_enter_command(tmp):
    """R32: the `:` prompt applies config commands to the live session:
    set/unset/toggle with a `?` query, bind and macro against the key
    tables, exec and push, and errors reported in the error style."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:2")
    # set: a new index_format takes effect on the next draw
    r.keys(b':set index_format="XX %s"\r')
    r.expect("XX Lunch on Friday?", "XX CI failed on")
    # ? queries the value instead of changing it; booleans read back
    # in mutt's no-prefixed spelling
    r.keys(b":set index_format?\r")
    r.expect('index_format="XX %s"')
    r.keys(b":set beep?\r")
    r.expect("beep")
    r.keys(b":unset beep\r")
    r.keys(b":set beep?\r")
    r.expect("nobeep")
    r.keys(b":toggle beep\r")
    r.keys(b":set beep?\r")
    r.expect("beep")
    # a bad option and a bad command both report as errors
    r.keys(b":set nosuchoption=1\r")
    r.expect("unknown or read-only option")
    r.keys(b":frobnicate\r")
    r.expect("unknown command")
    r.keys(b":set pager_index_lines=x\r")
    r.expect("wants a number")
    # macro: mutt syntax, checked against the live key tables
    r.keys(b':macro index L "l~f jane<enter>"\r')
    r.keys(b"L")
    r.expect("Msgs:1/2", "limit:~f jane")
    r.keys(b"l")
    r.keys(b"\x15\r")  # ctrl+u clears the prefilled limit
    r.expect("Msgs:2")
    # bind takes mutt key spellings and mutt function names
    r.keys(b":bind index D delete-message\r")
    r.keys(b"D")
    r.expect("Del:1")
    r.keys(b":bind index nosuchkey quit\r")
    r.expect("no such key")
    r.keys(b":bind index Z nosuchfunction\r")
    r.expect("no such function")
    # exec runs a function straight away, push feeds the input queue
    r.keys(b":exec help\r")
    r.expect("quit (writes changes")
    r.keys(b"q")
    r.keys(b':push "<enter>"\r')
    r.expect("Lunch on Friday?", "Date:")
    r.keys(b"q")
    # ignore/unignore reshape the pager's brief header view
    r.keys(b":unignore message-id\r")
    r.keys(b"\r")
    r.expect("Message-ID: <msg1@example.com>")
    r.keys(b"q")
    r.keys(b"x")
    r.close()



def scenario_patterns_v3(tmp):
    """R31's pattern terms reach the index through the keys: the limit
    prompt (`l`) and a pattern-op (`T`). Which messages each term
    matches is asserted in the session tests, where it is a value
    rather than a screen scrape (rmut-session/src/tests.rs)."""
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "ci"])
    # A message carrying an unusual header, so the limit has something
    # only ~h finds.
    with open(os.path.join(md, "cur", "1751790500.9.host:2,S"), "w") as f:
        f.write("From: bulk@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: newsletter\r\nDate: Mon, 6 Jul 2026 11:00:00 +0200\r\n"
                "Message-ID: <msg9@example.com>\r\n"
                "X-Spam-Score: 9.5\r\n\r\nbody\r\n")
    # A duplicate of jane's message: same Message-ID, different file.
    with open(os.path.join(md, "cur", "1751790600.10.host:2,S"), "w") as f:
        f.write("From: Jane Doe <jane@example.com>\r\nTo: jarda@example.com\r\n"
                "Subject: Lunch on Friday? (dup)\r\n"
                "Date: Mon, 6 Jul 2026 12:00:00 +0200\r\n"
                "Message-ID: <msg1@example.com>\r\n\r\nsecond copy\r\n")
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:4")
    # The limit prompt takes a pattern and the status bar shows it.
    r.keys(b"l~h x-spam\r")
    r.expect("Msgs:1/4", "limit:~h x-spam")
    # Ctrl+U clears the prefill, and an empty limit is all of them.
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.expect("Msgs:4")
    # A pattern-op takes the same terms.
    r.keys(b"T~=\r")
    r.expect("2 tagged")
    r.keys(b"x")
    r.close()

def scenario_batch_cli(tmp):
    """R33: sending without the TUI (-s/-c/-b/-a/-i, recipients after
    --), a mailto: URL opening a prefilled draft, -e running a config
    command at startup, -y entering the mailbox list, and the -z/-Z
    exit codes."""
    md = make_maildir(tmp, "md-cli")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-cli.eml")
    sendmail = os.path.join(tmp, "sendmail-cli.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "cli-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "mailto body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    fcc = make_maildir(tmp, "sent-cli-box")
    att = os.path.join(tmp, "cli-att.txt")
    with open(att, "w") as f:
        f.write("attached payload\n")
    cfg = os.path.join(tmp, "cli-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[identity]\nname = "Jarda"\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                f'sent = "{fcc}"\nmailboxes = ["{md}"]\n')
    env = dict(os.environ)
    env.update(base_env(tmp, {"RMUT_CONFIG": cfg}))

    def rmut(args, stdin=b"", timeout=15):
        return subprocess.run([RMUT, *args], input=stdin, env=env,
                              capture_output=True, timeout=timeout)

    # batch send: headers from the flags, body from stdin, Fcc kept
    proc = rmut(["-s", "batch subject", "-c", "cc@example.com",
                 "-b", "bcc@example.com", "-a", att, "--",
                 "jane@example.com", "petr@example.com"],
                stdin=b"batch body line\n")
    assert proc.returncode == 0, proc.stderr
    text = open(sent_file).read()
    assert "Subject: batch subject" in text
    assert "To: jane@example.com, petr@example.com" in text
    assert "Cc: cc@example.com" in text
    assert "Bcc: bcc@example.com" in text
    assert "From: Jarda <jarda@example.com>" in text
    assert "batch body line" in text
    assert "MIME-Version: 1.0" in text
    assert 'filename="cli-att.txt"' in text
    assert "YXR0YWNoZWQgcGF5bG9hZAo=" in text  # the attachment, base64
    assert len(os.listdir(os.path.join(fcc, "cur"))) == 1, "Fcc copy kept"

    # -i reads the body from a file, and -e applies a config setting
    body_file = os.path.join(tmp, "cli-body.txt")
    with open(body_file, "w") as f:
        f.write("body from a file\n")
    os.truncate(sent_file, 0)
    proc = rmut(["-i", body_file, "-s", "from file",
                 "-e", 'set realname="Batch Sender"', "--", "jane@example.com"])
    assert proc.returncode == 0, proc.stderr
    text = open(sent_file).read()
    assert "body from a file" in text
    assert "From: Batch Sender <jarda@example.com>" in text

    # a send with no recipients fails instead of sending
    proc = rmut(["-s", "nobody", "--"], stdin=b"x\n")
    assert proc.returncode != 0
    assert b"no recipients" in proc.stderr

    # -z / -Z report through the exit code without starting
    empty = make_maildir(tmp, "md-empty")
    assert rmut(["-z", empty]).returncode == 1
    assert rmut(["-Z", md]).returncode == 1, "no new mail in md-cli"

    # -e runs a config command before the first draw
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}),
             args=("-e", 'set index_format="ZZ %s"'))
    r.expect("ZZ Lunch on Friday?")
    r.keys(b"x")
    r.close()

    # -p goes to the postponed picker, which says so when empty
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), args=("-p",))
    r.expect("no postponed messages")
    r.keys(b"x")
    r.close()

    # -y opens the mailbox list straight away
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), args=("-y",))
    r.expect("md-cli")
    r.keys(b"q")
    r.keys(b"x")
    r.close()

    # a mailto: URL opens a prefilled draft: the editor adds a body and
    # the compose menu sends it
    os.truncate(sent_file, 0)
    url = ("mailto:jane@example.com?subject=Lunch%20on%20Friday"
           "&cc=petr@example.com&body=prefilled%20line")
    r = Rmut(url, base_env(tmp, {"RMUT_CONFIG": cfg}), args=("-f", md))
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: "Subject: Lunch on Friday" in open(sent_file).read(),
             desc="mailto draft sent")
    text = open(sent_file).read()
    assert "To: jane@example.com" in text
    assert "Cc: petr@example.com" in text
    assert "prefilled line" in text
    assert "mailto body" in text
    r.close()



def scenario_mailing_lists(tmp):
    """R34: mail.lists/subscribed drive ~l, L replies to the list only
    (List-Post, else a known list address in To/Cc), a group reply
    honours the sender's Mail-Followup-To, and mail to a subscribed
    list carries one."""
    md = make_maildir(tmp, "md-lists")
    write_msgs(md, ["jane"])
    with open(os.path.join(md, "cur", "1751791000.20.host:2,S"), "w") as f:
        f.write("From: Dev Person <dev-person@example.com>\r\n"
                "To: rmut-dev@lists.example.com\r\n"
                "Subject: patch review\r\n"
                "Date: Mon, 6 Jul 2026 13:00:00 +0200\r\n"
                "Message-ID: <list1@example.com>\r\n"
                "List-Id: Dev talk <rmut-dev.lists.example.com>\r\n"
                "List-Post: <mailto:rmut-dev@lists.example.com>\r\n\r\n"
                "please review\r\n")
    with open(os.path.join(md, "cur", "1751791100.21.host:2,S"), "w") as f:
        f.write("From: Careful Sender <careful@example.com>\r\n"
                "To: jarda@example.com\r\nCc: petr@example.com\r\n"
                "Subject: reply here please\r\n"
                "Date: Mon, 6 Jul 2026 14:00:00 +0200\r\n"
                "Message-ID: <mft1@example.com>\r\n"
                "Mail-Followup-To: followup@example.com\r\n\r\n"
                "use the followup address\r\n")
    sent_file = os.path.join(tmp, "sent-lists.eml")
    sendmail = os.path.join(tmp, "sendmail-lists.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "lists-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "reply body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "lists-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                f'subscribed = ["rmut-dev@lists.example.com"]\n'
                f'lists = ["announce@lists.example.com"]\n')
    env = base_env(tmp, {"RMUT_CONFIG": cfg})
    r = Rmut(md, env)
    r.expect("Msgs:3")
    # ~l limits to mail addressed to a known list
    r.keys(b"l~l\r")
    r.expect("Msgs:1/3", "patch review")
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.expect("Msgs:3")

    # L on a non-list message refuses instead of mailing the author
    r.keys(b"=")  # oldest first: jane's message
    r.expect("Lunch on Friday?")
    r.keys(b"L")
    r.expect("not a message from a known mailing list")

    # L on the list message replies to the list alone, and being
    # subscribed puts a Mail-Followup-To on it without my address
    r.keys(b"l~l\r")
    r.expect("Msgs:1/3")
    r.keys(b"L")
    r.expect("To:")
    r.keys(b"\r")       # take the prefilled list address
    r.expect("Subject:")
    r.keys(b"\r")       # take "Re: patch review"
    r.expect("Include")
    r.keys(b"y")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: Re: patch review" in open(sent_file).read(),
             desc="list reply sent")
    text = open(sent_file).read()
    assert "To: rmut-dev@lists.example.com" in text
    assert "dev-person@example.com" not in text.split("\n\n")[0], \
        "a list reply does not go to the author"
    assert "Mail-Followup-To: rmut-dev@lists.example.com" in text
    assert "jarda@example.com" not in text.split("Mail-Followup-To:")[1].split("\n")[0], \
        "subscribed: my address stays out of Mail-Followup-To"

    # a group reply honours the sender's Mail-Followup-To
    os.truncate(sent_file, 0)
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.keys(b"l~i mft1@\r")
    r.expect("Msgs:1/3", "reply here please")
    r.keys(b"g")
    r.expect("To:")
    r.keys(b"\r")
    r.expect("Subject:")
    r.keys(b"\r")
    r.expect("Include")
    r.keys(b"y")        # include the original, as the list reply did
    r.expect("Atts:0")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: Re: reply here please" in open(sent_file).read(),
             desc="group reply sent")
    text = open(sent_file).read()
    assert "To: followup@example.com" in text
    assert "Cc:" not in text, "Mail-Followup-To replaces the recipient set"
    r.keys(b"x")
    r.close()

def scenario_alternates_my_hdr(tmp):
    """R35: mail.alternates widens who counts as me (the %Z marks, ~p
    and ~P, reverse_name), a group reply drops my own addresses, and
    mail.my_hdr rides on every draft."""
    md = make_maildir(tmp, "md-alt")
    msgs = [
        ("1751791000.30.host:2,S",
         "From: Boss <boss@example.com>\r\n"
         "To: jp@old.example.com\r\n"
         "Subject: only to my old address\r\n"
         "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
         "Message-ID: <alt1@example.com>\r\n\r\nhello\r\n"),
        ("1751791100.31.host:2,S",
         "From: Jarda Old <jp@old.example.com>\r\n"
         "To: team@example.com\r\n"
         "Subject: sent by me\r\n"
         "Date: Mon, 6 Jul 2026 11:00:00 +0200\r\n"
         "Message-ID: <mine1@example.com>\r\n\r\nmine\r\n"),
        ("1751791200.32.host:2,S",
         "From: Petr <petr@example.com>\r\n"
         "To: jarda@example.com, Team <team@example.com>\r\n"
         "Cc: boss@example.com, jp@old.example.com\r\n"
         "Subject: reply to all of us\r\n"
         "Date: Mon, 6 Jul 2026 12:00:00 +0200\r\n"
         "Message-ID: <grp1@example.com>\r\n\r\nwho is in?\r\n"),
        ("1751791300.33.host:2,S",
         "From: Dev Person <dev-person@example.com>\r\n"
         "To: rmut-dev@lists.example.com\r\n"
         "Subject: on the list\r\n"
         "Date: Mon, 6 Jul 2026 13:00:00 +0200\r\n"
         "Message-ID: <lst1@example.com>\r\n\r\nlist mail\r\n"),
    ]
    for rel, content in msgs:
        with open(os.path.join(md, "cur", rel), "w") as f:
            f.write(content)
    sent_file = os.path.join(tmp, "sent-alt.eml")
    sendmail = os.path.join(tmp, "sendmail-alt.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "alt-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "reply body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "alt-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\nreverse_name = true\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                "alternates = ['jp@old\\.example\\.com']\n"
                'my_hdr = ["Organization: Acme", "Reply-To: jarda@example.com"]\n'
                'subscribed = ["rmut-dev@lists.example.com"]\n'
                '[index]\nformat = "[%Z] %s"\n')
    env = base_env(tmp, {"RMUT_CONFIG": cfg})
    r = Rmut(md, env)
    r.expect("Msgs:4")
    # mutt's $to_chars in the third %Z slot: '+' sole recipient (via an
    # alternate address), 'F' sent by me, 'T' one of several, 'L' to a
    # subscribed list.
    r.expect("[+] only to my old address")
    r.expect("[F] sent by me")
    r.expect("[T] reply to all of us")
    r.expect("[L] on the list")

    # ~p counts an alternate as me; ~P is the mail I sent
    r.keys(b"l~p\r")
    r.expect("Msgs:2/4")
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.keys(b"l~P\r")
    r.expect("Msgs:1/4", "sent by me")
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.expect("Msgs:4")

    # A group reply drops my own addresses (both of them) from the Cc,
    # and my_hdr rides along on the draft.
    r.keys(b"l~i grp1@\r")
    r.expect("Msgs:1/4", "reply to all of us")
    r.keys(b"g")
    r.expect("To:")
    r.keys(b"\r")
    # The Subject prompt overwrites the To prompt character for
    # character, so a redraw-diffed screen can lose letters from it;
    # settle and take the prefill rather than matching on it.
    r.settle()
    r.keys(b"\r")
    r.expect("Include")
    r.keys(b"y")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: Re: reply to all of us" in open(sent_file).read(),
             desc="group reply sent")
    text = open(sent_file).read()
    head = text.split("\n\n")[0]
    assert "To: Petr <petr@example.com>" in head, head
    assert "Cc: Team <team@example.com>, boss@example.com" in head, head
    assert "jarda@example.com" not in head.split("Cc:")[1].split("\n")[0], \
        "a group reply does not copy me"
    assert "jp@old.example.com" not in head.split("Cc:")[1].split("\n")[0], \
        "an alternate address is me too"
    assert "Organization: Acme" in head, head
    assert "Reply-To: jarda@example.com" in head, head

    # reverse_name picks the alternate the mail was addressed to
    os.truncate(sent_file, 0)
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.keys(b"l~i alt1@\r")
    r.expect("Msgs:1/4", "only to my old address")
    r.keys(b"r")
    r.expect("To:")
    r.keys(b"\r")
    r.settle()
    r.keys(b"\r")
    r.expect("Include")
    r.keys(b"y")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: Re: only to my old address" in open(sent_file).read(),
             desc="reply to the old address sent")
    head = open(sent_file).read().split("\n\n")[0]
    assert "From: jp@old.example.com" in head, head

    # unalternates at the `:` prompt takes the address back off
    r.keys(b"l")
    r.keys(b"\x15\r")
    r.keys(b":unalternates *\r")
    r.settle()
    r.keys(b"l~p\r")
    r.expect("Msgs:1/4", "reply to all of us")
    r.keys(b"x")
    r.close()

def scenario_hooks(tmp):
    """R36: folder-hook runs any command line on open, message-hook
    applies while its message is selected and is taken back off when
    it stops matching, reply-hook shapes a reply's From, and fcc-hook
    picks where the sent copy goes."""
    md = make_maildir(tmp, "md-hooks")
    work = make_maildir(tmp, "md-work")
    ext_sent = make_maildir(tmp, "ext-sent")
    default_sent = make_maildir(tmp, "default-sent")
    write_msgs(md, ["jane"])
    with open(os.path.join(md, "cur", "1751795000.40.host:2,S"), "w") as f:
        f.write("From: Boss <boss@example.com>\r\n"
                "To: jarda@example.com\r\n"
                "Subject: budget\r\n"
                "Date: Mon, 6 Jul 2026 18:00:00 +0200\r\n"
                "Message-ID: <boss1@example.com>\r\n\r\nnumbers\r\n")
    with open(os.path.join(work, "cur", "1751795100.41.host:2,S"), "w") as f:
        f.write("From: Colleague <col@work.example.com>\r\n"
                "To: jarda@work.example.com\r\n"
                "Subject: standup\r\n"
                "Date: Mon, 6 Jul 2026 19:00:00 +0200\r\n"
                "Message-ID: <work1@example.com>\r\n\r\nnotes\r\n")
    sent_file = os.path.join(tmp, "sent-hooks.eml")
    sendmail = os.path.join(tmp, "sendmail-hooks.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "hooks-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "body\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "hooks-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                f'sent = "{default_sent}"\n'
                f'mailboxes = ["{md}", "{work}"]\n'
                '[index]\nformat = "<%s>"\n'
                '[[folder_hooks]]\n'
                'folder = "*md-work*"\ncommand = \'set index_format="WORK %s"\'\n'
                '[[message_hooks]]\n'
                'pattern = "~f boss@example.com"\n'
                'command = \'set index_format="BOSS %s"\'\n'
                '[[reply_hooks]]\n'
                'pattern = "~f boss@example.com"\n'
                'command = "set from=jarda@work.example.com"\n'
                '[[fcc_hooks]]\n'
                "pattern = '~t @external\\.example\\.com'\n"
                f'mailbox = "{ext_sent}"\n')
    env = base_env(tmp, {"RMUT_CONFIG": cfg})
    r = Rmut(md, env)
    # Newest message is the boss's, so its message-hook is in force.
    r.expect("Msgs:2", "BOSS budget", "BOSS Lunch on Friday?")
    # Move off it and the hook is taken back off, format and all.
    r.keys(b"=")
    r.expect("<Lunch on Friday?>", "<budget>")
    r.keys(b"*")
    r.expect("BOSS budget")

    # reply-hook: replying to the boss uses the work From
    r.keys(b"r")
    r.expect("To:")
    r.keys(b"\r")
    r.settle()
    r.keys(b"\r")
    r.expect("Include")
    r.keys(b"y")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: Re: budget" in open(sent_file).read(),
             desc="reply to the boss sent")
    head = open(sent_file).read().split("\n\n")[0]
    assert "From: jarda@work.example.com" in head, head
    # and the hook is undone once the draft is built
    r.expect("BOSS budget")

    # fcc-hook: a message to the matching domain lands in ext-sent,
    # not in the [mail] sent maildir the reply's copy went to
    os.truncate(sent_file, 0)
    default_cur = os.path.join(default_sent, "cur")
    before = len(os.listdir(default_cur))
    assert before == 1, "the reply's copy went to [mail] sent"
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"someone@external.example.com\r")
    r.settle()
    r.keys(b"outside\r")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: outside" in open(sent_file).read(),
             desc="external message sent")
    wait_for(lambda: os.listdir(os.path.join(ext_sent, "cur")),
             desc="fcc-hook copy in ext-sent")
    copy = os.path.join(ext_sent, "cur", os.listdir(os.path.join(ext_sent, "cur"))[0])
    assert "Subject: outside" in open(copy).read()
    assert len(os.listdir(default_cur)) == before, \
        "the fcc-hook target wins over [mail] sent"

    # folder-hook: opening the work mailbox runs its command line
    r.keys(b"c")
    r.expect("Open mailbox")
    r.keys(work.encode() + b"\r")
    r.expect("WORK standup")
    r.keys(b"x")
    r.close()

def scenario_format_flowed(tmp):
    """R37: a format=flowed part is put back into paragraphs and
    wrapped at the display width (reflow_text = false leaves it
    alone), and text_flowed declares and space-stuffs what goes out."""
    md = make_maildir(tmp, "md-flowed")
    long_tail = " ".join(f"word{n}" for n in range(1, 13))
    with open(os.path.join(md, "cur", "1751797000.50.host:2,S"), "w") as f:
        f.write("From: Flow Sender <flow@example.com>\r\n"
                "To: jarda@example.com\r\n"
                "Subject: flowed mail\r\n"
                "Date: Mon, 6 Jul 2026 20:00:00 +0200\r\n"
                "Message-ID: <flow1@example.com>\r\n"
                "MIME-Version: 1.0\r\n"
                "Content-Type: text/plain; charset=us-ascii; format=flowed\r\n"
                "\r\n"
                "This sentence was \r\n"
                "cut in three \r\n"
                f"by the sender: {long_tail}\r\n"
                "\r\n"
                "> quoted and \r\n"
                "> continued\r\n"
                "-- \r\n"
                "Flow\r\n")
    sent_file = os.path.join(tmp, "sent-flowed.eml")
    sendmail = os.path.join(tmp, "sendmail-flowed.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "flowed-editor.sh")
    # A body whose lines need space-stuffing on the way out.
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf ">not a quote\\n indented\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "flowed-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                'text_flowed = true\n'
                '[pager]\nwrap = 40\n')
    env = base_env(tmp, {"RMUT_CONFIG": cfg})
    r = Rmut(md, env)
    r.expect("Msgs:1", "flowed mail")
    r.keys(b"\r")
    # The sender's two quoted lines are one paragraph again: no second
    # "> " in the middle. (Assertions run on whitespace-squashed text,
    # so the quote marks are what tells the two renderings apart.)
    r.expect("> quoted and continued")

    # reflow_text = false puts the sender's line breaks back
    r.keys(b":set noreflow_text\r")
    r.settle()
    r.keys(b"i")      # out to the index and back in, to re-render
    r.keys(b"\r")
    r.expect("> quoted and > continued")
    r.keys(b"i")

    # text_flowed on the way out: the part is declared and stuffed
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"bob@example.org\r")
    r.settle()
    r.keys(b"flowing out\r")
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: flowing out" in open(sent_file).read(),
             desc="flowed message sent")
    sent = open(sent_file).read()
    assert "Content-Type: text/plain; charset=utf-8; format=flowed" in sent, sent
    assert " >not a quote" in sent, sent
    assert "  indented" in sent, sent
    r.keys(b"x")
    r.close()


def scenario_mime_polish(tmp):
    """R38: alternative_order decides which part of an alternative
    shows, auto_view takes its command from mailcap (copiousoutput
    only, %s through a temp file), and unauto_view takes it off."""
    md = make_maildir(tmp, "md-mime")
    with open(os.path.join(md, "cur", "1752000000.10.host:2,S"), "w") as f:
        f.write("From: Alt Sender <alt@example.com>\r\n"
                "To: jarda@example.com\r\n"
                "Subject: two flavours\r\n"
                "Date: Wed, 8 Jul 2026 12:00:00 +0200\r\n"
                "Message-ID: <alt1@example.com>\r\n"
                "MIME-Version: 1.0\r\n"
                'Content-Type: multipart/alternative; boundary="b"\r\n'
                "\r\n"
                "--b\r\nContent-Type: text/plain\r\n\r\n"
                "the plain flavour\r\n"
                "--b\r\nContent-Type: text/html\r\n\r\n"
                "<b>the rich flavour</b>\r\n"
                "--b--\r\n")
    with open(os.path.join(md, "cur", "1751900000.11.host:2,S"), "w") as f:
        f.write("From: Odd Sender <odd@example.com>\r\n"
                "To: jarda@example.com\r\n"
                "Subject: odd parts\r\n"
                "Date: Tue, 7 Jul 2026 12:00:00 +0200\r\n"
                "Message-ID: <odd1@example.com>\r\n"
                "MIME-Version: 1.0\r\n"
                'Content-Type: multipart/mixed; boundary="m"\r\n'
                "\r\n"
                "--m\r\nContent-Type: text/plain\r\n\r\n"
                "see the parts below\r\n"
                "--m\r\nContent-Type: application/x-thing\r\n\r\n"
                "the thing payload\r\n"
                "--m\r\nContent-Type: video/mpeg\r\n\r\n"
                "not really a film\r\n"
                "--m--\r\n")
    htmlfilter = os.path.join(tmp, "htmlfilter.sh")
    with open(htmlfilter, "w") as f:
        f.write("#!/bin/sh\nprintf 'FILTEREDHTML '\nsed -e 's/<[^>]*>//g'\n")
    os.chmod(htmlfilter, 0o755)
    mailcap = os.path.join(tmp, "test-mailcap")
    with open(mailcap, "w") as f:
        f.write("# a test mailcap\n"
                "text/html; false; copiousoutput; test=false\n"
                f"text/html; {htmlfilter}; copiousoutput; test=true\n"
                "application/x-thing; cat %s; \\\n"
                "  copiousoutput\n"
                # needsterminal cannot render into the pager, and there
                # is no other entry: the part stays a stub.
                "video/mpeg; mpv %s; needsterminal\n")
    cfg = os.path.join(tmp, "mime-config.toml")
    with open(cfg, "w") as f:
        # html = "raw": this scenario is about alternative_order and
        # auto_view mechanics, so mutt's literal source view is kept
        # (the built-in renderer has scenario_html_text to itself).
        f.write('[identity]\nemail = "jarda@example.com"\n'
                '[pager]\nalternative_order = ["text/html"]\nhtml = "raw"\n'
                '[filters]\n"application/x-thing" = ""\n"video/mpeg" = ""\n')
    env = base_env(tmp, {"RMUT_CONFIG": cfg, "MAILCAPS": mailcap})
    r = Rmut(md, env)
    r.expect("Msgs:2", "two flavours")

    # alternative_order asked for html, so html is what shows, raw:
    # no filter is configured for it yet.
    r.keys(b"\r")
    r.expect("<b>the rich flavour</b>")
    r.keys(b"i")

    # auto_view text/html: the command comes from mailcap, skipping
    # the entry whose test= fails.
    r.keys(b":auto_view text/html\r")
    r.settle()
    r.keys(b"\r")
    r.expect("Autoview using", "FILTEREDHTML the rich flavour")
    r.keys(b"i")

    # unauto_view puts it back to raw html, unalternative_order back
    # to the plain part.
    r.keys(b":unauto_view text/html\r:unalternative_order text/html\r")
    r.settle()
    r.keys(b"\r")
    r.expect("the plain flavour")
    r.keys(b"i")

    # The other message: a mailcap command with %s gets the part in a
    # temp file, and a needsterminal entry is passed over.
    r.keys(b"k\r")
    r.expect("the thing payload", "video/mpeg is unsupported")
    r.keys(b"ix")
    r.close()


def scenario_html_text(tmp):
    """The built-in html-to-text: an html-only message renders as
    readable text by default (entities decoded, links keeping their
    targets, blockquotes as > prefixes), no lynx or mailcap needed;
    [pager] html = "raw" restores mutt's literal source view."""
    md = make_maildir(tmp, "md-html")
    with open(os.path.join(md, "cur", "1751790000.1.host:2,S"), "w") as f:
        f.write("From: jane@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: html only\r\n"
                "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
                "Message-ID: <ht1@example.com>\r\nMIME-Version: 1.0\r\n"
                "Content-Type: text/html\r\n\r\n"
                "<html><head><style>p{color:red}</style></head><body>"
                "<p>Hello &amp; goodbye</p>"
                "<p>see <a href=\"https://example.com/x\">the docs</a></p>"
                "<blockquote>quoted words</blockquote>"
                "</body></html>\r\n")
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:1")
    r.keys(b"\r")
    r.expect("Hello & goodbye", "the docs <https://example.com/x>",
             "> quoted words", absent=("<p>", "color:red"))
    r.keys(b"iq")
    r.close()

    cfg = os.path.join(tmp, "raw-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                '[pager]\nhtml = "raw"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")
    r.keys(b"\r")
    r.expect("<p>Hello &amp; goodbye</p>")
    r.keys(b"iq")
    r.close()


def scenario_undo(tmp):
    """Beyond mutt: `z` walks back the last step from the index, the
    pager and after a save, and a sync puts the changes out of its
    reach. What each step restores is asserted in the session tests;
    this is about the key reaching it (rmut-session/src/tests.rs)."""
    md = make_maildir(tmp, "md-undo")
    write_msgs(md, ["jane", "petr", "ci"])
    target = os.path.join(tmp, "md-undo-archive")

    def files():
        return sorted(os.listdir(os.path.join(md, "cur"))
                      + os.listdir(os.path.join(md, "new")))

    start = files()
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")

    # One delete, walked back.
    r.keys(b"d")
    r.expect("Del:1")
    r.keys(b"z")
    r.expect("undone: delete (1 message(s))")

    # A pattern delete is one step, however many it marked.
    r.keys(b"D~A\r")
    r.expect("3 deleted")
    r.keys(b"z")
    r.expect("undone: deleted by pattern (3 message(s))")

    # A delete from the pager lands on the same stack (the oldest
    # message is already seen, so opening it changes nothing else).
    r.keys(b"=\r")
    r.settle()
    r.keys(b"d")
    r.settle()
    r.keys(b"iz")
    r.settle()

    # A save leaves a copy behind and marks the original deleted;
    # undoing takes both back.
    r.keys(b"s")
    r.expect("Save to mailbox:")
    r.keys(target.encode() + b"\r")
    r.expect("original marked deleted")
    wait_for(lambda: len(os.listdir(os.path.join(target, "cur"))) == 1,
             desc="the saved copy")
    # (No screen assertion here: the status that replaces "saved to
    # ..." shares its characters, and ratatui redraws only changed
    # cells, so the accumulated pty text is not a reliable witness.
    # The copy going away, and the sync below, are.)
    r.keys(b"z")
    wait_for(lambda: os.listdir(os.path.join(target, "cur")) == [],
             desc="the saved copy removed")

    # Nothing is pending after all that, so the sync writes nothing:
    # the maildir is byte for byte where it started, flag suffixes
    # included. (A status assertion would be unreliable here, for the
    # cell-diff reason above.)
    r.keys(b"$")
    r.settle()
    assert files() == start, files()

    # A written change is out of undo's reach.
    r.keys(b"d$y")
    wait_for(lambda: len(files()) == len(start) - 1, desc="the purge")
    r.keys(b"z")
    r.expect("nothing to undo")
    r.keys(b"x")
    r.close()

def scenario_undo_send(tmp):
    """Beyond mutt: undo_send holds a sent message for a few seconds,
    z takes it back to the compose menu, and what is still waiting
    goes out when rmut exits."""
    md = make_maildir(tmp, "md-undosend")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-undosend.eml")
    sendmail = os.path.join(tmp, "sendmail-undosend.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "undosend-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "body of the held message\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "undosend-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                'undo_send = 2\ncopy = false\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")

    def compose(subject):
        r.keys(b"m")
        r.expect("To:")
        r.keys(b"bob@example.org\r")
        r.settle()
        r.keys(subject.encode() + b"\r")
        r.expect("y:Send")
        r.keys(b"y")

    # Sent, but held: z takes it back to the compose menu, and the
    # window passes with nothing sent.
    compose("held then cancelled")
    r.expect("in 2s (z cancels)")
    r.keys(b"z")
    r.expect("cancelled")
    time.sleep(3)
    assert not os.path.exists(sent_file), open(sent_file).read()

    # The draft is intact in the menu: send it again and let it go.
    r.expect("y:Send")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: held then cancelled" in open(sent_file).read(),
             desc="the held message sent after its window")
    assert "body of the held message" in open(sent_file).read()

    # Quitting is not cancelling: what is still waiting goes out.
    compose("still waiting at exit")
    r.settle()
    r.keys(b"x")
    # The send happens as rmut winds down, so wait for it before
    # dropping the pty (closing it would signal the child first).
    wait_for(lambda: "Subject: still waiting at exit" in open(sent_file).read(),
             desc="the held message sent on the way out")
    r.close()


def scenario_search_direction(tmp):
    """R45: Alt+/ searches backwards and n keeps going that way, which
    is what makes a ./, macro pair step through the messages marked
    for deletion in both directions and rescue one from the middle."""
    md = make_maildir(tmp, "md-searchdir")
    # Oldest first: 1 ci, 2 jane, 3 petr, 4 alice. Three of them get
    # marked, so forward and backward from the same place land on
    # different messages and the test can tell the two apart.
    write_msgs(md, ["ci", "jane", "petr", "alice"])
    cfg = os.path.join(tmp, "searchdir-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                '[macros.index]\n'
                '"." = "/~D<enter>"\n'
                '"," = "<alt+/>~D<enter>"\n')

    def files():
        return [os.path.join(md, sub, n)
                for sub in ("cur", "new")
                for n in os.listdir(os.path.join(md, sub))]

    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:4")

    r.keys(b"D~s CI\r")
    r.expect("1 deleted")
    r.keys(b"D~f petr\r")
    r.keys(b"D~f alice\r")
    r.settle()

    # Sitting on the unmarked message at 2: . walks forward to the
    # marked one at 3, and , from there walks backwards to 1, not
    # forward to 4. The pager's "Message n/m" is what tells them
    # apart, since either direction would find something.
    r.keys(b"=j")
    r.settle()
    r.keys(b".")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 3/4")
    r.keys(b"i,")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 1/4", "CI failed on main")
    r.keys(b"i")

    # Rescue that one from the middle of the run, and purge the rest.
    r.keys(b"u")
    r.settle()
    r.keys(b"$y")
    wait_for(lambda: len(files()) == 2, desc="the other two purged")
    left = [open(f, encoding="utf-8", errors="replace").read() for f in files()]
    assert not any("petr@example.com" in t or "alice@example.com" in t for t in left), left
    assert any("CI failed on main" in t for t in left), left
    r.keys(b"x")
    r.close()


def scenario_tagged_and_pager(tmp):
    """R41: ; hands the tagged set to save, pipe and print, refuses
    the functions that take one message, and the pager can mark a
    message rather than only delete it."""
    md = make_maildir(tmp, "md-tagged")
    write_msgs(md, ["jane", "petr", "ci"])  # oldest first: ci, jane, petr
    target = os.path.join(tmp, "md-tagged-archive")
    piped = os.path.join(tmp, "tagged-pipe.txt")

    def files():
        return [os.path.join(md, sub, n)
                for sub in ("cur", "new")
                for n in os.listdir(os.path.join(md, sub))]

    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")

    # Tag the first two (t advances), then hand them to save.
    r.keys(b"=tt")
    r.settle()
    # The bar says Tag- while the prefix waits for its function, the
    # way mutt's message line does.
    r.keys(b";")
    r.repaint()
    r.expect("Tag-")
    r.keys(b"s" + target.encode() + b"\r")
    r.expect("saved 2 to")
    wait_for(lambda: len(os.listdir(os.path.join(target, "cur"))) == 2,
             desc="both copies delivered")

    # One undo step takes both copies and both delete marks back.
    r.keys(b"z")
    wait_for(lambda: os.listdir(os.path.join(target, "cur")) == [],
             desc="both copies removed")
    r.keys(b"$")
    r.settle()
    assert len(files()) == 3, files()

    # A function that takes one message says so instead of quietly
    # doing one of the twelve.
    r.keys(b";e")
    r.expect("edit does not take the tagged set")

    # Pipe gets them concatenated, in one run of the command.
    r.keys(b";|cat >> " + piped.encode() + b"\r")
    r.expect("piped 2 messages to")
    wait_for(lambda: os.path.exists(piped)
             and "CI failed on main" in open(piped, encoding="utf-8").read()
             and "Lunch on Friday" in open(piped, encoding="utf-8").read(),
             desc="both messages piped")

    # ;t toggles the tag on the tagged, which is how mutt clears them.
    r.keys(b";t")
    r.settle()

    # The pager can tag, and z takes that back from the pager too.
    r.keys(b"=\r")
    r.settle()
    r.keys(b"t")
    r.settle()
    r.keys(b"z")
    r.expect("undone: tag (1 message(s))")

    # It can flag, and undelete what the index marked.
    r.keys(b"F")
    r.settle()
    r.keys(b"i")
    r.keys(b"dk\r")
    r.settle()
    r.keys(b"u")
    r.settle()
    r.keys(b"i$")
    r.expect("synced: 0 deleted")
    ci = [f for f in files() if "CI failed on main" in open(f, encoding="utf-8").read()]
    assert len(ci) == 1, ci
    assert "F" in os.path.basename(ci[0]).split(":2,")[-1], ci[0]
    r.keys(b"x")
    r.close()


def scenario_thread_ops(tmp):
    """R42: Alt+d/u/t act on the whole thread, Ctrl+D/Ctrl+U on the
    subthread under the cursor, and Alt+n/Alt+p step between threads.
    All of it needs thread sort, as it does in mutt."""
    md = make_maildir(tmp, "md-threads")
    # ci, then Jane's thread (jane -> alice -> bob), then petr:
    # 5 rows, 3 thread roots.
    write_msgs(md, ["jane", "petr", "ci", "alice", "bob"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:5")

    # Without thread sort they refuse, like mutt.
    r.keys(b"\x1bd")
    r.expect("thread operations need thread sort")

    r.keys(b"ot")
    r.expect("sorted by threads")

    # Alt+n / Alt+p walk the roots: row 1 ci, row 2 jane, row 5 petr.
    r.keys(b"=\x1bn")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 2/5")
    r.keys(b"i\x1bn")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 5/5")
    r.keys(b"i\x1bp\x1bp")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 1/5")
    r.keys(b"i")

    # Alt+d takes the thread the cursor sits in, all three of it, and
    # one z brings it back.
    r.keys(b"=j\x1bd")
    r.expect("3 deleted")
    r.keys(b"z")
    r.repaint()
    r.expect("undone: delete thread (3 message(s))")

    # Ctrl+D takes the message under the cursor and its replies only.
    r.keys(b"j\x04")
    r.repaint()
    r.expect("2 deleted")
    r.keys(b"z")
    r.repaint()
    r.expect("undone: delete subthread (2 message(s))")

    # Alt+t tags the thread, and follows the cursor's tag like mutt:
    # a second press untags it.
    r.keys(b"k\x1bt")
    r.repaint()
    r.expect("3 tagged")
    r.keys(b"\x1bt")
    r.repaint()
    r.expect("3 untagged")

    # Nothing reached disk: every mark was walked back.
    r.keys(b"$")
    r.repaint()
    r.expect("synced: 0 deleted")
    r.keys(b"x")
    r.close()


def scenario_thread_surgery(tmp):
    """R63: # breaks a thread at the cursor, & links the tagged
    messages under it, Ctrl+R marks a thread read, P climbs to the
    parent; the file is rewritten and z writes it back."""
    md = make_maildir(tmp, "md-surgery")
    # jane -> alice -> bob is one thread; petr and ci stand alone.
    write_msgs(md, ["jane", "petr", "ci", "alice", "bob"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:5")
    r.keys(b"ot")
    r.expect("sorted by threads")

    # P from bob (row 4) climbs to alice, then jane, then refuses.
    r.keys(b"=jjj")
    r.settle()
    r.keys(b"P")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 3/5")
    r.keys(b"iP")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 2/5")
    r.keys(b"iP")
    r.expect("no parent message")

    # Ctrl+R marks Jane's thread read; z takes it back.
    r.keys(b"\x12")
    r.expect("3 marked read")
    r.keys(b"z")
    r.expect("undone: read thread (3 message(s))")

    # # on alice: alice and bob become a thread of their own, and the
    # file no longer says In-Reply-To.
    alice = os.path.join(md, "cur", MSGS["alice"][0].split("/")[-1])
    r.keys(b"j#")
    r.expect("thread broken")
    r.settle()
    with open(alice) as f:
        text = f.read()
    assert "In-Reply-To" not in text, text
    assert "Count me in too" in text, text

    # & hangs the tagged alice back under jane. Tag advances the
    # cursor (mutt's tag-entry), so after breaking, alice sits at row
    # 4: tag her, climb to jane at row 2, link.
    r.keys(b"tkkk&")
    r.expect("1 linked")
    r.settle()
    with open(alice) as f:
        text = f.read()
    assert "In-Reply-To: <msg1@example.com>" in text, text

    # Back down the stack: the link, the tag the `t` key made, then
    # the break, each its own step.
    r.keys(b"z")
    r.expect("undone: link threads (1 message(s))")
    r.keys(b"z")
    r.expect("undone: tag (1 message(s))")
    r.keys(b"z")
    r.expect("undone: break thread (1 message(s))")
    with open(alice) as f:
        text = f.read()
    assert "In-Reply-To: <msg1@example.com>" in text, text
    assert "References: <msg1@example.com>" in text, text

    # The thread patterns: only Jane's thread has bob in it.
    r.keys(b"l~(~f bob)\r")
    r.expect("Msgs:3")
    r.keys(b"l\r")
    r.expect("Msgs:5")
    r.keys(b"x")
    r.close()


def scenario_labels_and_flags(tmp):
    """R64: Y sets an X-Label that ~y finds, %y shows and o y sorts by;
    V shows the version; the pattern table gains ~R/~O/~Q and refuses
    the crypto terms by name."""
    md = make_maildir(tmp, "md-labels")
    write_msgs(md, ["jane", "petr", "ci"])
    cfg = os.path.join(tmp, "labels-config.toml")
    with open(cfg, "w") as f:
        f.write('[index]\nformat = "%4C %Z %-6d %-10y %s"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:3")

    # Y labels the message under the cursor; %y shows it in the index.
    r.keys(b"=Ywork\r")
    r.expect("labelled 1 message(s)", "work")
    r.settle()

    # ~y finds it.
    r.keys(b"l~y work\r")
    r.expect("Msgs:1")
    r.settle()
    r.keys(b"l\x15\r")  # ctrl+u clears the line, enter drops the limit
    r.expect("Msgs:3")
    r.settle()

    # V shows the version.
    r.keys(b"V")
    r.expect(f"rmut {VERSION}")
    r.settle()

    # A refused pattern names itself.
    r.keys(b"l\x15~G\r")
    r.expect("~G is not supported")
    r.keys(b"x")
    r.close()


def scenario_decode_family(tmp):
    """R65: Alt+s decode-saves the decoded message to a mailbox."""
    md = make_maildir(tmp, "md-decode")
    with open(os.path.join(md, "cur", "1751000000.1.host:2,S"), "w") as f:
        f.write("From: Jane <jane@example.com>\r\nTo: me@example.com\r\n"
                "Subject: encoded\r\nDate: Mon, 10 Mar 2024 10:00:00 +0000\r\n"
                "Message-ID: <enc@example.com>\r\nMIME-Version: 1.0\r\n"
                "Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n"
                "aGVsbG8gZnJvbSBiYXNlNjQK\r\n")
    out = os.path.join(tmp, "decoded")
    r = Rmut(md, base_env(tmp))
    r.expect("encoded")
    r.keys(b"\x1bs")
    r.expect("Decode-save to mailbox:")
    r.keys(out.encode() + b"\r")
    r.expect("saved to")
    r.settle()
    cur = os.path.join(out, "cur")
    files = os.listdir(cur)
    assert len(files) == 1, files
    with open(os.path.join(cur, files[0])) as f:
        body = f.read()
    assert "hello from base64" in body, body
    assert "aGVsbG8" not in body, body
    r.keys(b"q")
    r.close()


def scenario_outgoing_envelope(tmp):
    """R67: $hostname sets the Message-ID host, $user_agent adds the
    header, $sig_on_top puts the signature above the quote."""
    md = make_maildir(tmp, "md-envelope")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "envelope-sent.eml")
    editor = os.path.join(tmp, "envelope-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "the body of my reply\\n" >> "$1"\n')
    sendmail = os.path.join(tmp, "envelope-sendmail.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(editor, 0o755)
    os.chmod(sendmail, 0o755)
    sig = os.path.join(tmp, "envelope-sig")
    with open(sig, "w") as f:
        f.write("Jarda\n")
    cfg = os.path.join(tmp, "envelope-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[mail]\nsignature = "{sig}"\nsig_on_top = true\n'
                'hostname = "mail.example.net"\nuser_agent = true\ninclude = "no"\n')
    r = Rmut(md, base_env(tmp, {"EDITOR": editor, "RMUT_SENDMAIL": sendmail,
                                "RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")

    # Reply, send.
    r.keys(b"r")
    r.expect("To:")
    r.keys(b"jane@example.com\r\ry")
    wait_for(lambda: os.path.exists(sent_file), desc="the reply went out")
    r.expect("message sent")
    sent = open(sent_file).read()
    assert "Message-ID:" in sent and "@mail.example.net>" in sent, sent
    assert "User-Agent: rmut/" in sent, sent
    # sig_on_top: the signature sits above the body.
    assert "-- \nJarda" in sent, sent
    assert sent.index("Jarda") < sent.index("the body of my reply"), sent
    r.keys(b"q")
    r.close()


def scenario_navigation(tmp):
    """R68: number-jump moves to a message by index, @ shows the
    sender's full address, o o sorts by To."""
    md = make_maildir(tmp, "md-nav")
    write_msgs(md, ["jane", "petr", "ci", "alice"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:4")

    # Type 3 then Enter: jump to message 3.
    r.keys(b"3")
    r.expect("Jump to message: 3")
    r.keys(b"\r")
    r.settle()
    r.keys(b"\r")  # open it
    r.expect("Message 3/4")
    r.keys(b"i")

    # @ shows the full From address of the selected message.
    r.keys(b"=@")
    r.expect("@example.com")

    # Sort by To (o then o).
    r.keys(b"oo")
    r.expect("sorted by to")

    # A number past the end says so.
    r.keys(b"99\r")
    r.expect("no message 99")
    r.keys(b"x")
    r.close()


def scenario_compose_menu(tmp):
    """R71: recall = no goes straight to a new message, and F on the
    compose menu overrides the From header."""
    md = make_maildir(tmp, "md-cmenu")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "cmenu-sent.eml")
    editor = os.path.join(tmp, "cmenu-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "the body\\n" >> "$1"\n')
    sendmail = os.path.join(tmp, "cmenu-sendmail.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(editor, 0o755)
    os.chmod(sendmail, 0o755)
    cfg = os.path.join(tmp, "cmenu-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[mail]\nrecall = "no"\n')
    env = base_env(tmp, {"EDITOR": editor, "RMUT_SENDMAIL": sendmail,
                         "RMUT_CONFIG": cfg})
    r = Rmut(md, env)
    r.expect("Msgs:1")

    # Compose and postpone, so a draft is waiting.
    r.keys(b"mfirst@example.com\rfirst subject\rP")
    r.expect("postponed to")
    r.settle()

    # recall = no: m goes straight to a new message (the To prompt),
    # never the (n)ew/(r)ecall question.
    r.keys(b"m")
    r.expect("To:", absent=("recall postponed",))
    # New recipient, subject, editor runs, compose menu appears.
    r.keys(b"second@example.com\rsecond subject\r")
    r.settle()
    # F on the menu overrides From, then send.
    r.keys(b"Fboss@example.com\r")
    r.settle()
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file) and "second subject" in open(sent_file).read(),
             desc="the message went out")
    sent = open(sent_file).read()
    assert "From: boss@example.com" in sent, sent
    assert "Subject: second subject" in sent, sent
    r.keys(b"q")
    r.close()


def scenario_title_and_write(tmp):
    """R73: $ts_enabled sets the terminal title (an OSC escape), and %
    toggles the mailbox read-only."""
    md = make_maildir(tmp, "md-title")
    write_msgs(md, ["jane", "petr"])
    cfg = os.path.join(tmp, "title-config.toml")
    with open(cfg, "w") as f:
        f.write('[ui]\nset_title = true\ntitle_format = "rmut: %f [%m]"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:2")
    # The OSC 2 title escape carries the format's output.
    r.expect("\x1b]", "rmut: ")
    assert "[2]" in r.buf, r.buf[-200:]

    # % marks the mailbox read-only; a delete then refuses.
    r.keys(b"%")
    r.expect("marked read-only")
    r.keys(b"d")
    r.expect("read-only")
    # % again makes it writable.
    r.keys(b"%")
    r.expect("marked writable")
    r.keys(b"x")
    r.close()


def scenario_simple_search(tmp):
    """R74: $simple_search expands a bare word; adding ~b %s makes a
    bare search find the body too."""
    md = make_maildir(tmp, "md-simple")
    # "budget" is in one subject and one body.
    with open(os.path.join(md, "cur", "1751000001.1.host:2,S"), "w") as f:
        f.write("From: a@x\r\nSubject: the budget\r\nDate: Mon, 10 Mar 2024 10:00:00 +0000\r\n"
                "Message-ID: <s1@x>\r\n\r\nhello\r\n")
    with open(os.path.join(md, "cur", "1751000002.2.host:2,S"), "w") as f:
        f.write("From: a@x\r\nSubject: lunch\r\nDate: Tue, 11 Mar 2024 10:00:00 +0000\r\n"
                "Message-ID: <s2@x>\r\n\r\nthe budget is tight\r\n")
    cfg = os.path.join(tmp, "simple-config.toml")
    with open(cfg, "w") as f:
        f.write('[mail]\nsimple_search = "~f %s | ~s %s | ~b %s"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:2")
    # A bare word, with the body in the template, finds both.
    r.keys(b"lbudget\r")
    r.expect("Msgs:2")
    r.keys(b"l\r")
    r.expect("Msgs:2")
    r.keys(b"x")
    r.close()


def scenario_page_motion(tmp):
    """R77: H and M jump to the top and middle of the visible page."""
    md = make_maildir(tmp, "md-page")
    # Enough messages to fill more than a screen.
    for i in range(20):
        with open(os.path.join(md, "cur", f"17510000{i:02d}.1.host:2,S"), "w") as f:
            f.write(f"From: a@x\r\nSubject: msg {i:02d}\r\n"
                    f"Date: Mon, 10 Mar 2024 10:00:00 +0000\r\nMessage-ID: <m{i}@x>\r\n\r\nbody\r\n")
    r = Rmut(md, base_env(tmp), rows=14)
    r.expect("Msgs:20")

    def opened_number():
        # Drop the scrollback first, so we read this frame's number,
        # not one left in the cumulative buffer. Sync on "/20" (a clean
        # render; a stale cell-diff artifact reads "Message2020").
        r.buf = ""
        r.expect("/20")
        return int(re.findall(r"Message(\d+)/20", squash(r.buf))[-1])

    # Jump to the last message: the page scrolls to the bottom.
    r.keys(b"*\r")
    assert opened_number() == 20
    r.keys(b"i")
    r.settle()
    # H moves to the top of that scrolled page -- off the bottom, but
    # not all the way back to message 1.
    r.keys(b"H\r")
    top = opened_number()
    assert 1 < top < 20, top
    r.keys(b"i")
    r.settle()
    # M is the middle of the page: below the top, above the bottom.
    r.keys(b"M\r")
    mid = opened_number()
    assert top < mid < 20, (top, mid)
    r.keys(b"ix")
    r.close()


def scenario_history_file(tmp):
    """R79: $history_file persists prompt history across sessions."""
    md = make_maildir(tmp, "md-hist")
    write_msgs(md, ["jane", "petr"])
    histfile = os.path.join(tmp, "rmut-history")
    cfg = os.path.join(tmp, "hist-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[ui]\nhistory_file = "{histfile}"\n')
    env = base_env(tmp, {"RMUT_CONFIG": cfg})

    # First session: run a limit search, then quit.
    r = Rmut(md, env)
    r.expect("Msgs:2")
    r.keys(b"l~f jane\r")
    r.expect("Msgs:1")
    r.keys(b"l\r")   # clear the limit
    r.expect("Msgs:2")
    r.keys(b"q")     # quit (writes the history on the way out)
    wait_for(lambda: os.path.exists(histfile), desc="history file written")
    r.close()
    saved = open(histfile).read()
    assert "pattern\t~f jane" in saved, saved

    # Second session: the limit prompt's Up recalls it.
    r = Rmut(md, env)
    r.expect("Msgs:2")
    r.keys(b"l")            # open the limit prompt
    r.expect("Limit")
    r.keys(b"\x1b[A\r")    # Up recalls "~f jane", Enter applies it
    r.expect("Msgs:1")
    r.keys(b"q")
    r.close()


def scenario_layout(tmp):
    """R78: $arrow_cursor marks the selection with -> and
    $status_on_top puts the status bar near the top."""
    md = make_maildir(tmp, "md-layout")
    write_msgs(md, ["jane", "petr"])
    cfg = os.path.join(tmp, "layout-config.toml")
    with open(cfg, "w") as f:
        f.write('[ui]\nstatus_on_top = true\narrow_cursor = true\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), rows=12)
    r.expect("Msgs:2")
    # The arrow marks the selected row.
    r.expect("->")
    # status_on_top: the "---rmut:" bar and the index both drawn; the
    # bar appears above the second message row in the raw stream.
    r.expect("---rmut:")
    r.keys(b"q")
    r.close()


def scenario_help_bar_off(tmp):
    """R85: $help = false drops the key-help line; the rest of the
    screen (the index and the status bar) is intact and one row
    taller. $menu_context keeps lines in view past the cursor."""
    md = make_maildir(tmp, "md-nohelp")
    write_msgs(md, ["jane", "petr", "ci", "alice", "bob"])
    cfg = os.path.join(tmp, "nohelp-config.toml")
    with open(cfg, "w") as f:
        f.write('[ui]\nhelp = false\nmenu_context = 1\n')
    # Six rows: without the help bar four go to the index (with it,
    # three), so five messages need one scroll, not two.
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), rows=6)
    r.expect("Msgs:5", absent=["q:Quit"])
    # The cursor starts on the last message; the first row is scrolled
    # off. Moving up to the top brings it back, one line at a time.
    r.keys(b"gg")
    r.expect("Lunch on Friday?")
    r.keys(b"q")
    r.close()


def scenario_last_keys(tmp):
    """R86: what-key names keys until Ctrl+G, error-history shows the
    recent complaints on a screen, and ~ (mark-message) binds a
    stroke that jumps back to the message."""
    md = make_maildir(tmp, "md-lastkeys")
    write_msgs(md, ["jane", "petr", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")
    r.keys(b":exec what-key\r")
    r.expect("Enter keys (^G to abort)")
    r.keys(b"a")
    r.expect("Char = a, Octal = 141, Decimal = 97")
    r.keys(b"\x07")  # Ctrl+G ends it; the next key is a key again
    # An error to remember, then the history screen.
    r.keys(b"n")
    r.expect("no search pattern")
    r.keys(b":exec error-history\r")
    r.expect("Error history", "no search pattern (use /)")
    r.keys(b"q")
    # mark-message: the stroke becomes a hotkey for this message.
    r.keys(b"=")
    r.keys(b"~")
    r.expect("Enter macro stroke: ")
    r.keys(b"1\r")
    r.expect("Message bound to 1.")
    r.keys(b"*")   # last message
    r.keys(b"1")   # the hotkey: a search for the Message-ID
    r.expect("Msgs:3", absent=["bad pattern", "not found"])
    r.keys(b"q")
    r.close()


def scenario_compose_functions(tmp):
    """R75: the heavier compose-menu functions: A attaches the tagged
    (or current) message as message/rfc822, Ctrl+O renames a file for
    sending, u marks it to be unlinked, w writes the message to a
    mailbox without sending, and the send honours all of it."""
    md = make_maildir(tmp, "md-cfn")
    write_msgs(md, ["jane"])
    archive = make_maildir(tmp, "md-cfn-archive")
    sent_file = os.path.join(tmp, "cfn-sent.eml")
    editor = os.path.join(tmp, "cfn-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "the body\\n" >> "$1"\n')
    sendmail = os.path.join(tmp, "cfn-sendmail.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(editor, 0o755)
    os.chmod(sendmail, 0o755)
    doomed = os.path.join(tmp, "cfn-doomed.txt")
    with open(doomed, "w") as f:
        f.write("going, going\n")
    env = base_env(tmp, {"EDITOR": editor, "RMUT_SENDMAIL": sendmail})
    r = Rmut(md, env)
    r.expect("Msgs:1")
    r.keys(b"mto@example.com\rfunctions\r")
    r.settle()
    r.expect("-- Attachments")
    # A: the message under the cursor, as message/rfc822.
    r.keys(b"A")
    r.expect("attached 1 message(s)", "message/rfc822")
    # a file, then Ctrl+O renames it and u marks it for unlinking.
    r.keys(b"a" + doomed.encode() + b"\r")
    r.expect("cfn-doomed.txt")
    r.keys(b"jj")   # onto the file (body, message, file)
    r.keys(b"\x0f")
    r.expect("Send attachment with name: ")
    r.keys(b"\x15renamed.txt\r")  # ctrl+u clears the prefill
    r.expect("as renamed.txt")
    r.keys(b"u")
    r.expect("[unlink]", "deleted after sending")
    # w: written to the archive, not sent, the draft still here.
    r.keys(b"w")
    r.expect("Write message to mailbox: ")
    r.keys(b"\x15" + archive.encode() + b"\r")
    r.expect("Message written to")
    written = [p for sub in ("cur", "new")
               for p in os.listdir(os.path.join(archive, sub))]
    assert len(written) == 1, written
    assert not os.path.exists(sent_file)
    # y: sent with the renamed part inline the message, and the file
    # is gone.
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="the message went out")
    sent = open(sent_file).read()
    assert 'filename="renamed.txt"' in sent, sent
    assert "Content-Type: message/rfc822" in sent, sent
    assert "Subject: Lunch on Friday?" in sent, "the attached message rides whole"
    wait_for(lambda: not os.path.exists(doomed), desc="the unlinked file went")
    r.keys(b"q")
    r.close()


def scenario_status_chars(tmp):
    """R82: $status_chars sets the %r mailbox-state marker."""
    md = make_maildir(tmp, "md-schars")
    write_msgs(md, ["jane"])
    cfg = os.path.join(tmp, "schars-config.toml")
    with open(cfg, "w") as f:
        # Put %r somewhere visible, and give it distinctive chars.
        f.write('[ui]\nstatus_chars = "=!%"\n'
                'status_format = "--rmut[%r] %f Msgs:%m"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")
    # Unchanged: char[0] = "=".
    r.expect("rmut[=]")
    # Delete a message: pending change, char[1] = "!".
    r.keys(b"d")
    r.expect("rmut[!]")
    r.keys(b"u")
    r.keys(b"q")
    r.close()


def scenario_search_context(tmp):
    """R83: $search_context keeps a few lines above a pager search hit."""
    md = make_maildir(tmp, "md-sctx")
    body = [f"line {i:02}" for i in range(1, 41)]
    body[29] = "the needle here"
    with open(os.path.join(md, "cur/1751790000.9.host:2,S"), "w") as f:
        f.write("From: Jane <jane@example.com>\r\nSubject: report\r\n"
                "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\nMessage-ID: <r@x>\r\n\r\n"
                + "\r\n".join(body) + "\r\n")
    cfg = os.path.join(tmp, "sctx-config.toml")
    with open(cfg, "w") as f:
        f.write('[pager]\nsearch_context = 3\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), rows=20)
    r.expect("report")
    r.keys(b"\r")
    r.expect("line 01")
    r.keys(b"/needle\r")
    # With 3 lines of context, the hit is not the top line: lines 27-29
    # (three above) are visible above "the needle here".
    r.expect("the needle here", "line 27")
    r.keys(b"qq")
    r.close()


def scenario_subject_threading(tmp):
    """R90: mail that carries no References at all still reads as one
    thread, the way mutt's pseudo-threading has it; $strict_threads
    turns it off, and # takes one message out of the group for good."""
    md = make_maildir(tmp, "md-subject")
    # A notification robot: a fresh Message-ID every time, no
    # References anywhere, one subject.
    subject = "epog-devel | mediator error logging (!1661)"
    for i, hour in enumerate((10, 11, 12)):
        prefix = "" if i == 0 else "Re: "
        with open(os.path.join(md, "cur", f"175179000{i}.{i}.host:2,S"), "w") as f:
            f.write(f"From: gitlab@example.com\r\nTo: jarda@example.com\r\n"
                    f"Subject: {prefix}{subject}\r\n"
                    f"Date: Mon, 6 Jul 2026 {hour}:00:00 +0200\r\n"
                    f"Message-ID: <note{i}@example.com>\r\n\r\nnote {i}\r\n")
    with open(os.path.join(md, "cur", "1751790009.9.host:2,S"), "w") as f:
        f.write("From: jane@example.com\r\nSubject: lunch?\r\n"
                "Date: Mon, 6 Jul 2026 13:00:00 +0200\r\n"
                "Message-ID: <lunch@example.com>\r\n\r\nfree friday?\r\n")
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:4")
    r.keys(b"ot")
    r.expect("sorted by threads")
    # The two replies hang under the first, and the tree stars them:
    # mutt's mark for a message placed by its subject.
    r.expect("└*")
    r.keys(b"=\x1bv")           # Alt+v folds the thread at the cursor
    r.expect("(2 hidden)")
    r.keys(b"\x1bv")

    # $strict_threads takes the grouping away again. The buffer is
    # cumulative, so drop what is in it before asking for a screen
    # without the star on it.
    r.keys(b":set strict_threads=yes\r")
    r.repaint()
    r.buf = ""
    r.repaint()
    r.expect("lunch?", absent=("└*",))
    r.keys(b":set strict_threads=no\r")
    r.repaint()
    r.expect("└*")

    # # on a grouped message takes it out and keeps it out: the file
    # says so, which is how it survives a restart.
    r.keys(b"=j#")
    r.expect("thread broken")
    r.settle()
    text = open(os.path.join(md, "cur", "1751790001.1.host:2,S")).read()
    assert "X-Rmut-Thread: broken" in text, text
    r.keys(b"z")
    r.expect("undone: break thread (1 message(s))")
    r.keys(b"x")
    r.close()


def scenario_folder_shorthand(tmp):
    """R43: =x and +x name a mailbox under [mail] folder, whether
    typed at a prompt, replayed from a macro, or written in the
    config."""
    root = os.path.join(tmp, "Mail")
    md = make_maildir(root, "inbox")
    make_maildir(root, "archive")
    write_msgs(md, ["jane", "ci"])
    cfg = os.path.join(tmp, "shorthand-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nfolder = "{root}"\n'
                'mailboxes = ["=inbox", "=archive"]\n'
                'trash = "=trash"\n'
                '[macros.index]\n'
                'A = "s=archive<enter>"\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:2")

    # Typed at the save prompt.
    r.keys(b"=s")
    r.expect("Save to mailbox:")
    r.keys(b"=archive\r")
    r.repaint()
    r.expect("saved to " + os.path.join(root, "archive"))
    wait_for(lambda: len(os.listdir(os.path.join(root, "archive", "cur"))) == 1,
             desc="the copy under =archive")
    r.keys(b"z")
    r.repaint()

    # Replayed from a macro, which is how an imported mutt config
    # reaches it: "<save-message>=archive<enter>".
    r.keys(b"A")
    r.repaint()
    r.expect("original marked deleted")
    wait_for(lambda: len(os.listdir(os.path.join(root, "archive", "cur"))) == 1,
             desc="the copy the macro saved")

    # Configured: the trash mailbox is =trash, and purging creates it
    # there rather than in a directory called "=trash".
    r.keys(b"$y")
    wait_for(lambda: os.path.isdir(os.path.join(root, "trash", "cur"))
             and len(os.listdir(os.path.join(root, "trash", "cur"))) == 1,
             desc="the purged message in =trash")
    # A literal "=trash" directory would mean the expansion never ran;
    # it would land in the process's cwd, so that is where to look.
    assert not os.path.exists("=trash"), "a literal =trash directory was created"

    # And at the change-folder prompt.
    r.keys(b"c=archive\r")
    r.repaint()
    r.expect(os.path.join(root, "archive"))
    r.keys(b"x")
    r.close()


def scenario_paper_cuts(tmp):
    """R44: Space pages the index, ! runs a shell command, Alt+c opens
    a mailbox read-only, Ctrl+L repaints and Ctrl+Z suspends."""
    md = make_maildir(tmp, "md-cuts")
    for n in range(1, 11):
        with open(os.path.join(md, "cur", f"17520000{n:02d}.9.host:2,S"), "w") as f:
            f.write(f"From: Sender {n} <s{n}@example.com>\r\n"
                    "To: jarda@example.com\r\n"
                    f"Subject: message number {n}\r\n"
                    f"Date: Mon, 6 Jul 2026 {n:02d}:00:00 +0200\r\n"
                    f"Message-ID: <cut{n}@example.com>\r\n\r\nbody {n}\r\n")
    other = make_maildir(tmp, "md-cuts-other")
    write_msgs(other, ["jane"])
    touched = os.path.join(tmp, "shell-escape-ran")

    # A short window, so one page is a known number of rows: content
    # is rows minus the help bar, the status bar and the message line,
    # and PageDown moves the cursor by that much.
    r = Rmut(md, base_env(tmp), rows=10)
    r.expect("Msgs:10")
    r.keys(b"=")
    r.settle()
    r.keys(b" ")
    r.settle()
    r.keys(b"\r")
    r.expect("Message 8/10")
    r.keys(b"i")

    # Ctrl+L cannot be seen directly (a repaint of the same screen),
    # but it must not disturb anything: the keys after it still work.
    r.keys(b"\x0c")
    r.settle()

    # ! runs a command with the TUI stood down, then waits.
    r.keys(b"!touch " + touched.encode() + b"\r")
    wait_for(lambda: os.path.exists(touched), desc="the shell command ran")
    r.keys(b"\r")  # "Press Enter to continue"
    r.repaint()
    r.expect("finished")

    # Alt+c opens read-only: the mailbox loads, and marks are refused.
    r.keys(b"\x1bc" + other.encode() + b"\r")
    r.repaint()
    r.expect("read-only")
    r.keys(b"d")
    r.repaint()
    r.expect("Mailbox is read-only.")

    # Ctrl+Z hands the terminal back and raises SIGTSTP. The stop
    # itself cannot be asserted here: pty.fork makes rmut a session
    # leader, so its process group is orphaned and the kernel discards
    # stop signals. What this does cover is the terminal handoff
    # around it, which is the part that can wreck a session: the
    # screen comes back and the keys still work.
    r.keys(b"\x1a")
    r.settle()
    r.repaint()
    r.keys(b"\r")
    r.expect("Message 1/1")
    r.keys(b"ix")
    r.close()


def scenario_attach_reminder(tmp):
    """Beyond mutt (neomutt's $abort_noattach): a body that mentions
    an attachment with none attached is asked about, unless the
    mention is in quoted text or a signature."""
    md = make_maildir(tmp, "md-attach")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-attach.eml")
    sendmail = os.path.join(tmp, "sendmail-attach.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    # The body comes from a file the editor appends, so each draft can
    # say something different.
    body = os.path.join(tmp, "attach-body.txt")
    editor = os.path.join(tmp, "attach-editor.sh")
    with open(editor, "w") as f:
        f.write(f'#!/bin/sh\ncat {body} >> "$1"\n')
    os.chmod(editor, 0o755)
    attachment = os.path.join(tmp, "the-file.txt")
    with open(attachment, "w") as f:
        f.write("the actual file\n")
    cfg = os.path.join(tmp, "attach-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
                'abort_noattach = "ask"\ncopy = false\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")

    def compose(subject):
        r.keys(b"m")
        r.expect("To:")
        r.keys(b"bob@example.org\r")
        r.settle()
        r.keys(subject.encode() + b"\r")
        r.expect("y:Send")
        r.keys(b"y")

    # Mentioned, not attached: asked, and n goes back to the menu.
    with open(body, "w") as f:
        f.write("The report is attached.\n")
    compose("one")
    r.repaint()
    r.expect("mentions an attachment and none is attached")
    r.keys(b"n")
    r.repaint()
    r.expect("a attaches a file")
    assert not os.path.exists(sent_file), "sent despite the reminder"

    # Attach the file it was asking about, and it goes without a word.
    r.keys(b"a" + attachment.encode() + b"\r")
    r.settle()
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file)
             and "Subject: one" in open(sent_file).read(),
             desc="the message with its attachment")
    # The file rides along base64-encoded, so look for its name.
    assert 'filename="the-file.txt"' in open(sent_file).read()

    # A mention only in quoted text or under the signature is not a
    # forgotten attachment: this one sends straight out.
    os.truncate(sent_file, 0)
    with open(body, "w") as f:
        f.write("> did you see the attachment?\nNo.\n-- \nsent with an attachment opener\n")
    compose("two")
    wait_for(lambda: "Subject: two" in open(sent_file).read(),
             desc="the quoted mention sent without a question")

    # And y at the question sends it as it is.
    os.truncate(sent_file, 0)
    with open(body, "w") as f:
        f.write("Attached you will find nothing.\n")
    compose("three")
    r.repaint()
    r.expect("Send? (y/n)")
    r.keys(b"y")
    wait_for(lambda: "Subject: three" in open(sent_file).read(),
             desc="sent anyway on y")
    r.keys(b"x")
    r.close()


def scenario_getting_started(tmp):
    """R47: with no mailbox on the command line rmut looks where mutt
    looks, says what to do when it finds nothing, and --import-muttrc
    -w saves the config instead of printing it."""
    home = os.path.join(tmp, "start-home")
    make_maildir(os.path.join(home, "Mail"), "inbox")
    write_msgs(os.path.join(home, "Mail", "inbox"), ["jane"])
    env = base_env(tmp, {"HOME": home, "MAIL": "", "USER": "nobody"})

    # ~/Mail is mutt's $folder, and a directory of maildirs answers
    # with its inbox.
    r = Rmut(None, env)
    r.expect("Msgs:1", "Lunch on Friday?")
    r.keys(b"x")
    r.close()

    # Nothing anywhere: the failure says where it looked and what to
    # do about it.
    empty = os.path.join(tmp, "start-empty")
    os.makedirs(empty, exist_ok=True)
    r = Rmut(None, base_env(tmp, {"HOME": empty, "MAIL": "", "USER": "nobody"}))
    r.expect("no mailbox found", "Looked in:", "mkdir -p")
    r.close()

    # An mbox spool named by $MAIL is a mailbox too, file and all.
    spool = os.path.join(tmp, "start-spool")
    with open(spool, "w") as f:
        f.write("From jane@example.com Mon Jul  6 10:00:00 2026\n"
                "From: Jane Doe <jane@example.com>\nTo: jarda@example.com\n"
                "Subject: spooled mail\nDate: Mon, 6 Jul 2026 10:00:00 +0200\n\n"
                "hello from the spool\n\n")
    r = Rmut(None, base_env(tmp, {"HOME": empty, "MAIL": spool, "USER": "nobody"}))
    r.expect("spooled mail")
    r.keys(b"x")
    r.close()

    # --import-muttrc -w writes the config, creating the directory,
    # and will not overwrite one that is already there.
    muttrc = os.path.join(tmp, "start-muttrc")
    with open(muttrc, "w") as f:
        f.write('set realname = "Started Here"\nset folder = ~/Mail\n'
                'alias petr Petr Novak <petr@example.com>\n')
    out = os.path.join(tmp, "start-config", "config.toml")
    aliases = os.path.join(tmp, "start-config", "aliases")
    env = base_env(tmp, {"RMUT_CONFIG": out, "RMUT_ALIASES": aliases})
    r = Rmut(None, env, args=("--import-muttrc", "-w", muttrc))
    r.expect("wrote " + out)
    r.close()
    assert 'name = "Started Here"' in open(out).read()
    # Aliases live in a mutt-format file of their own, so the import
    # writes that too rather than leaving them commented out.
    assert open(aliases).read() == "alias petr Petr Novak <petr@example.com>\n"
    assert "alias" not in open(out).read(), open(out).read()
    # An imported config carries whatever set imap_pass held, so both
    # files are written for their owner only.
    assert oct(os.stat(out).st_mode)[-3:] == "600", oct(os.stat(out).st_mode)
    assert oct(os.stat(aliases).st_mode)[-3:] == "600"
    r = Rmut(None, env, args=("--import-muttrc", "-w", muttrc))
    r.expect("already exists")
    r.close()


def scenario_control_chars(tmp):
    """A tab in a header used to be written to the terminal as it
    stood: the terminal expanded it, pushing the rest of the index row
    past the window edge and wrapping it onto a second line."""
    md = make_maildir(tmp, "md-ctrl")
    with open(os.path.join(md, "cur", "1751790000.1.host:2,S"), "w") as f:
        f.write("From: Tabbed\tSender <t@example.com>\r\n"
                "To: jarda@example.com\r\n"
                "Subject: before\ttab after\r\n"
                "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\n"
                "Message-ID: <ctrl1@example.com>\r\n\r\nbody\r\n")
    env = base_env(tmp)
    r = Rmut(md, env)
    r.expect("before tab after")
    assert "\t" not in r.buf, "a raw tab reached the terminal"
    r.keys(b"\r")
    r.expect("Subject: before tab after")
    assert "\t" not in r.buf, "a raw tab reached the terminal from the pager"
    r.keys(b"ix")
    r.close()

    # The header cache keeps the parsed subject, so a second run must
    # not serve the tab back from it.
    r = Rmut(md, env)
    r.expect("before tab after")
    assert "\t" not in r.buf, "a raw tab came back out of the header cache"
    r.keys(b"x")
    r.close()


def scenario_purge_question(tmp):
    """mutt's $delete is ask-yes: Enter takes the yes, and delete=yes
    skips the question. Enter used to call the purge off silently."""
    def mailbox(name, n=3):
        md = make_maildir(tmp, name)
        for i in range(1, n + 1):
            with open(os.path.join(md, "cur", f"17517900{i:02d}.{i}.host:2,S"), "w") as f:
                f.write(f"From: S{i} <s{i}@example.com>\nTo: jarda@example.com\n"
                        f"Subject: purge {i}\nDate: Mon, 6 Jul 2026 1{i}:00:00 +0200\n"
                        f"Message-ID: <pg{i}{name}@x>\n\nbody\n")
        return md

    def count(md):
        return len(os.listdir(os.path.join(md, "cur")))

    # Enter at the question purges, like mutt's ask-yes.
    md = mailbox("md-purge")
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3")
    r.keys(b"d$")
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"\r")
    wait_for(lambda: count(md) == 2, desc="purged on Enter")
    r.keys(b"x")
    r.close()

    # n keeps the mark, as before.
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:2")
    r.keys(b"d$")
    r.expect("Purge 1 deleted message(s)?")
    r.keys(b"n")
    r.repaint()
    r.expect("synced: 0 deleted")
    assert count(md) == 2, count(md)
    r.keys(b"x")
    r.close()

    # delete = "yes": no question at all.
    md2 = mailbox("md-purge-yes")
    cfg = os.path.join(tmp, "purge-config.toml")
    with open(cfg, "w") as f:
        f.write('[mail]\ndelete = "yes"\n')
    r = Rmut(md2, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:3")
    r.keys(b"d$")
    wait_for(lambda: count(md2) == 2, desc="purged without asking")
    r.keys(b"x")
    r.close()


def scenario_password_permissions(tmp):
    """A config holding a plaintext password, readable by anyone, is
    worth saying out loud at startup."""
    md = make_maildir(tmp, "md-secret")
    write_msgs(md, ["jane"])
    cfg = os.path.join(tmp, "secret-config.toml")
    with open(cfg, "w") as f:
        f.write('[[accounts]]\nname = "work"\nuser = "jane"\n'
                'password = "hunter2"\nimap_host = "imap.example.com"\n')
    os.chmod(cfg, 0o644)
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), cols=200)
    r.expect("chmod 600", "others can read it")
    r.keys(b"x")
    r.close()

    # Shut the bits and it says nothing.
    os.chmod(cfg, 0o600)
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), cols=200)
    r.expect("Msgs:1")
    r.settle()
    assert "chmod 600" not in squash(r.buf), "warned about a 600 config"
    r.keys(b"x")
    r.close()


def scenario_network_timeouts(tmp):
    """R54: an unreachable server costs seconds, not the OS default of
    about two minutes, and the message says which host and what it was
    doing. 203.0.113.1 is TEST-NET-3: it black-holes, so the connect
    waits out the timeout rather than being refused."""
    md = make_maildir(tmp, "md-timeout")
    write_msgs(md, ["jane"])
    cfg = os.path.join(tmp, "timeout-config.toml")
    with open(cfg, "w") as f:
        f.write('[identity]\nemail = "jarda@example.com"\n'
                '[net]\nconnect_timeout = 2\n'
                '[[accounts]]\nname = "slow"\nuser = "jarda"\n'
                'password = "x"\nimap_host = "203.0.113.1"\nimap_port = 993\n')
    env = dict(os.environ)
    env.update(base_env(tmp, {"RMUT_CONFIG": cfg}))
    start = time.time()
    proc = subprocess.run([RMUT, "imap:slow"], env=env, capture_output=True,
                          timeout=30)
    took = time.time() - start
    err = proc.stderr.decode()
    assert proc.returncode != 0, err
    assert "timed out after 2s" in err, err
    assert "203.0.113.1:993" in err, err
    # The OS would have taken about two minutes over the same address.
    assert took < 15, f"{took:.1f}s"



def scenario_network_abort(tmp):
    """R55: a slow server does not stop the screen. The connection runs
    on a thread of its own, so the fetch says what it is doing while it
    waits, Ctrl+G gives up on it (mutt's abort), and the next one gets
    a fresh connection."""
    imap = FakeImap()
    imap.add(1, {"\\Seen"}, IMAP_MSG.format(
        sender="one@remote.example", subject="slow one",
        date="Mon, 6 Jul 2026 10:00:00 +0200", mid="s1", body="the slow body"))
    imap.start()
    cfg = os.path.join(tmp, "abort-config.toml")
    with open(cfg, "w") as f:
        f.write(f"""
[identity]
email = "jarda@example.com"
[mail]
poll_seconds = 600
[[accounts]]
name = "slow"
user = "jane"
password = "x"
imap_host = "127.0.0.1"
imap_port = {imap.port}
imap_tls = false
""")
    env = base_env(tmp, {
        "RMUT_CONFIG": cfg,
        "XDG_CACHE_HOME": os.path.join(tmp, "cache"),
    })
    r = Rmut("imap:slow", env)
    r.expect("imap:slow/INBOX", "Msgs:1", "slow one")
    # The body is not cached yet, and the server takes its time over
    # it: the message line says so while the screen keeps drawing.
    imap.body_delay = 4
    r.keys(b"\r")
    r.expect("fetching the message", "Ctrl+G aborts")
    # mutt's Ctrl+G: give up. The key is read, which is the point.
    r.keys(b"\x07")
    r.expect("aborted: fetching the message")
    # The connection recovers: a second try, with the server no longer
    # dawdling, opens the message.
    imap.body_delay = 0
    r.keys(b"\r")
    r.expect("the slow body")
    r.keys(b"ix")
    r.close()



def scenario_reply_text(tmp):
    """R56: the three strings mutt users change. $attribution and
    $indent_string shape a quoted reply, $forward_format the subject a
    forward carries, $include decides the quote without asking, and
    $askcc puts a Cc prompt between To and Subject."""
    md = make_maildir(tmp, "md-reply-text")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "sent-reply-text.eml")
    sendmail = os.path.join(tmp, "sendmail-reply-text.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(sendmail, 0o755)
    editor = os.path.join(tmp, "reply-text-editor.sh")
    with open(editor, "w") as f:
        f.write('#!/bin/sh\nprintf "my answer\\n" >> "$1"\n')
    os.chmod(editor, 0o755)
    cfg = os.path.join(tmp, "reply-text-config.toml")
    with open(cfg, "w") as f:
        f.write(
            f'[identity]\nemail = "jarda@example.com"\n'
            f'[mail]\nsendmail = "{sendmail}"\neditor = "{editor}"\n'
            f'attribution = "%n wrote (%{{%Y}}):"\n'
            f'indent_string = "| "\n'
            f'forward_format = "Fwd: %s"\n'
            f'include = "yes"\n'
            f'ask_cc = true\n'
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")

    # Reply: To, then the Cc prompt $askcc adds, then Subject. No
    # include question: $include said yes.
    r.keys(b"r")
    r.expect("To:")
    r.keys(b"\r")
    r.expect("Cc:")
    r.keys(b"cc@example.com\r")
    r.expect("Subject:")
    r.keys(b"\r")
    r.expect("y:Send", "Cc: cc@example.com")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="the reply sent")
    text = open(sent_file).read()
    assert "Jane Doe wrote (20" in text, text
    assert "| Hi Jarda," in text, text
    assert "Cc: cc@example.com" in text, text

    # Forward: the subject comes from $forward_format.
    r.expect("Msgs:1")
    r.keys(b"f")
    r.expect("To:")
    r.keys(b"petr@example.com\r")
    r.expect("Cc:")  # $askcc asks on every compose, as mutt does
    r.keys(b"\r")
    r.expect("Subject:", "Fwd: Lunch on Friday?")
    r.keys(b"\x1b")  # Esc: the draft is not needed
    r.keys(b"x")
    r.close()



def scenario_reading_habits(tmp):
    """R58: mutt's reading options. $pager_stop keeps Space on the last
    page instead of opening the next message, and $markers = false
    takes the + off wrapped continuation lines."""
    md = make_maildir(tmp, "md-habits")
    write_msgs(md, ["jane", "petr"])
    # A message with one long line, so there is something to wrap.
    with open(os.path.join(md, "cur", "1751790900.4.host:2,S"), "w") as f:
        f.write("From: long@example.com\r\nTo: jarda@example.com\r\n"
                "Subject: a long one\r\nDate: Wed, 8 Jul 2026 12:00:00 +0200\r\n"
                "Message-ID: <long1@example.com>\r\n\r\n"
                + ("wrapped " * 40) + "\r\n")
    cfg = os.path.join(tmp, "habits-config.toml")
    with open(cfg, "w") as f:
        f.write('[pager]\npager_stop = true\nmarkers = false\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}), cols=40)
    r.expect("Msgs:3")
    r.keys(b"*\r")  # the newest message: the long one
    r.expect("a long one", "wrapped")
    # markers = false: no + at the start of a continuation line.
    assert "+wrapped" not in r.buf, r.buf[-400:]
    # $pager_stop: Space on the last page stays put.
    r.keys(b" " * 6)
    r.settle()
    r.expect("Message 3/3")
    r.keys(b"ix")
    r.close()



def scenario_leaving_habits(tmp):
    """R59: mutt's $quit asks before leaving, and $confirmappend asks
    before adding to a mailbox that already exists."""
    md = make_maildir(tmp, "md-leaving")
    write_msgs(md, ["jane"])
    target = make_maildir(tmp, "md-leaving-archive")
    cfg = os.path.join(tmp, "leaving-config.toml")
    with open(cfg, "w") as f:
        f.write('[mail]\nquit = "ask-yes"\nconfirmappend = true\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")

    # $confirmappend: the target maildir is there, so it asks.
    r.keys(b"C")  # copy, not save: the original stays put
    r.expect("Copy to mailbox:")
    r.keys(target.encode() + b"\r")
    r.expect("Append messages to")
    r.keys(b"n")
    r.settle()
    assert os.listdir(os.path.join(target, "cur")) == [], "n copied nothing"
    r.keys(b"C")
    r.expect("Copy to mailbox:")
    r.keys(target.encode() + b"\r")
    r.expect("Append messages to")
    r.keys(b"y")
    wait_for(lambda: len(os.listdir(os.path.join(target, "cur"))) == 1,
             desc="the confirmed copy")

    # $quit: q asks, n stays, y leaves.
    r.keys(b"q")
    r.expect("Quit rmut?")
    r.keys(b"n")
    r.expect("Msgs:1")
    r.keys(b"q")
    r.expect("Quit rmut?")
    r.keys(b"y")
    r.close()



def scenario_signature_and_send_questions(tmp):
    """R61: mutt's $signature ends a draft, $forward_quote indents the
    forwarded message, and $abort_unmodified drops an untouched one."""
    md = make_maildir(tmp, "md-signature")
    write_msgs(md, ["jane"])
    sent_file = os.path.join(tmp, "signature-sent.eml")
    quiet = os.path.join(tmp, "edit-nothing")
    editor = os.path.join(tmp, "signature-editor.sh")
    with open(editor, "w") as f:
        # With the marker there the editor is a :q: the file comes
        # back exactly as it was handed over.
        f.write(f'#!/bin/sh\n[ -f {quiet} ] && exit 0\n'
                f'printf "a word of my own\\n" >> "$1"\n')
    sendmail = os.path.join(tmp, "signature-sendmail.sh")
    with open(sendmail, "w") as f:
        f.write(f"#!/bin/sh\ncat >> {sent_file}\nexit 0\n")
    os.chmod(editor, 0o755)
    os.chmod(sendmail, 0o755)
    sig = os.path.join(tmp, "signature")
    with open(sig, "w") as f:
        f.write("Jarda\nexample.com\n")
    cfg = os.path.join(tmp, "signature-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[mail]\nsignature = "{sig}"\nforward_quote = true\n'
                'indent_string = "| "\n')
    r = Rmut(md, base_env(tmp, {"EDITOR": editor, "RMUT_SENDMAIL": sendmail,
                                "RMUT_CONFIG": cfg}))
    r.expect("Msgs:1")

    # A forward: recipient, the prefilled subject, editor, send.
    r.keys(b"f")
    r.expect("To:")
    r.keys(b"someone@example.com\r\ry")
    wait_for(lambda: os.path.exists(sent_file), desc="the forward went out")
    r.expect("message sent")
    sent = open(sent_file).read()
    assert "| Are you free for lunch on Friday?" in sent, sent
    assert "----- End forwarded message -----" in sent, sent
    # The signature is under the forwarded text, where the editor
    # found it and typed on past it.
    assert "\n-- \nJarda\nexample.com\n" in sent, sent
    assert sent.index("-- \nJarda") > sent.index("End forwarded message"), sent

    # $abort_unmodified: an editor that changes nothing is not a
    # message, and the draft never reaches the compose menu.
    open(quiet, "w").close()
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"someone@example.com\rnothing to say\r")
    r.expect("aborted unmodified message")
    r.keys(b"q")
    r.close()


def scenario_small_habits(tmp):
    """R62: mutt's $print question, $wait_key after a shell escape,
    and $mark_old leaving unread mail alone."""
    md = make_maildir(tmp, "md-habits")
    write_msgs(md, ["jane", "petr"])   # petr is in new/
    printed = os.path.join(tmp, "printed.txt")
    touched = os.path.join(tmp, "shell-ran")
    cfg = os.path.join(tmp, "habits-config.toml")
    with open(cfg, "w") as f:
        f.write(f'[mail]\nmark_old = false\nprint_confirm = "ask-yes"\n'
                f'print = "cat > {printed}"\n\n[ui]\nwait_key = false\n')
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("Msgs:2")

    # $print = ask-yes: the question comes, and Enter takes the yes.
    r.keys(b"p")
    r.expect("Print message?")
    r.keys(b"\r")
    wait_for(lambda: os.path.exists(printed), desc="the printed message")
    # The cursor starts on the new message (mutt's first-new).
    assert "petr@example.com" in open(printed).read()

    # $wait_key off: the index comes straight back, with nothing
    # waiting for an Enter that the test never sends.
    r.keys(b"!")
    r.expect("Shell command:")
    r.keys(f"touch {touched}\r".encode())
    wait_for(lambda: os.path.exists(touched), desc="the shell escape")
    r.expect("finished")
    r.expect("Msgs:2")

    # $mark_old off: the unread arrival is still in new/ afterwards.
    r.keys(b"q")
    r.close()
    assert os.listdir(os.path.join(md, "new")), "the arrival stayed new"


SCENARIOS = [
    scenario_view_and_pager,
    scenario_pager_save_advances,
    scenario_pager_save_last_exits,
    scenario_sync_delete_flag_limit,
    scenario_threads_and_fold,
    scenario_compose_send_postpone,
    scenario_config,
    scenario_sidebar,
    scenario_send_via_config_sendmail,
    scenario_imap,
    scenario_mbox,
    scenario_trash_and_alias,
    scenario_pgp,
    scenario_print,
    scenario_edit_headers,
    scenario_mutt_flow,
    scenario_line_editor,
    scenario_enter_command,
    scenario_patterns_v3,
    scenario_batch_cli,
    scenario_mailing_lists,
    scenario_alternates_my_hdr,
    scenario_hooks,
    scenario_format_flowed,
    scenario_mime_polish,
    scenario_html_text,
    scenario_undo,
    scenario_undo_send,
    scenario_search_direction,
    scenario_tagged_and_pager,
    scenario_thread_ops,
    scenario_folder_shorthand,
    scenario_paper_cuts,
    scenario_attach_reminder,
    scenario_getting_started,
    scenario_control_chars,
    scenario_purge_question,
    scenario_password_permissions,
    scenario_pager_search,
    scenario_triage,
    scenario_odds,
    scenario_pager_quotes,
    scenario_pager_polish,
    scenario_notmuch,
    scenario_compose_round2,
    scenario_message_commands,
    scenario_identities,
    scenario_tag_save_sort,
    scenario_import_muttrc,
    scenario_attachment_pager,
    scenario_attach_mailcap,
    scenario_network_timeouts,
    scenario_network_abort,
    scenario_reply_text,
    scenario_reading_habits,
    scenario_leaving_habits,
    scenario_signature_and_send_questions,
    scenario_small_habits,
    scenario_thread_surgery,
    scenario_labels_and_flags,
    scenario_decode_family,
    scenario_outgoing_envelope,
    scenario_navigation,
    scenario_compose_menu,
    scenario_title_and_write,
    scenario_simple_search,
    scenario_page_motion,
    scenario_history_file,
    scenario_layout,
    scenario_status_chars,
    scenario_search_context,
    scenario_help_bar_off,
    scenario_last_keys,
    scenario_compose_functions,
    scenario_subject_threading,
]



def main():
    if not os.path.exists(RMUT):
        print(f"missing {RMUT}; run `cargo build` first", file=sys.stderr)
        return 1
    failed = 0
    for scenario in SCENARIOS:
        tmp = tempfile.mkdtemp(prefix="rmut-e2e-")
        try:
            scenario(tmp)
            print(f"PASS {scenario.__name__}")
        except Exception as exc:  # noqa: BLE001 - report and continue
            failed += 1
            print(f"FAIL {scenario.__name__}: {exc}")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)
    print(f"{len(SCENARIOS) - failed}/{len(SCENARIOS)} scenarios passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
