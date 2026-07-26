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
}


def write_msgs(maildir, names):
    for name in names:
        rel, content = MSGS[name]
        with open(os.path.join(maildir, rel), "w") as f:
            f.write(content)


class Rmut:
    def __init__(self, maildir, env=None, rows=30, cols=160):
        self.buf = ""
        cmd = [RMUT, maildir]
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
        deadline = time.time() + timeout
        while time.time() < deadline:
            self._drain()
            text = squash(self.buf)
            if all(squash(n) in text for n in needles):
                for a in absent:
                    assert squash(a) not in text, f"unexpected {a!r} on screen"
                return
        raise AssertionError(
            f"timed out waiting for {needles!r}; tail: {squash(self.buf)[-400:]!r}"
        )

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
    r.keys(b"l\x15~x\r")
    r.expect("bad pattern: unknown pattern ~x")
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
    # reply: accept prefilled To/Subject, then discard at the send prompt
    r.keys(b"r\r\rq")
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
    r.keys(b"mx@y\rposty\rp")
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
    r.expect("[1 attachment(s)]")
    r.keys(b"v")
    r.expect("Draft attachments", "notes.txt", "text/plain")
    r.keys(b"qy")  # back to the send prompt, send
    wait_for(lambda: "Subject: attprompt" in open(sent_file).read(),
             desc="attach-prompt send")
    assert 'filename="notes.txt"' in open(sent_file).read()
    # R20: two postponed drafts -> the recall picker
    r.keys(b"mx@y\rdraft-one\rp")
    r.expect("postponed to")
    r.keys(b"mn")  # postponed exist: answer (n)ew first
    r.keys(b"x@y\rdraft-two\rp")
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
    r.keys(b"  ")  # two pages down: the action list has grown
    r.expect("~p addressed to me")  # the patterns block is on screen
    r.keys(b"q")
    # the macro replays its sequence through the limit prompt
    r.keys(b"L")
    r.expect("limit:~f jane")
    r.keys(b"l\x15\r")  # clear the limit again
    # new-mail detection: drop a message into new/ and wait for the poll
    write_msgs(md, ["petr"])
    r.expect("new mail in", "+1", timeout=8)
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
    r.expect("Send message?")
    r.keys(b"y")
    r.expect("message sent")
    r.settle()
    sent = open(sent_file).read()
    assert "From: Jarda <jarda@example.com>" in sent
    r.keys(b"q")
    r.close()


class FakeImap(threading.Thread):
    """Stateful IMAP server: enough of RFC 3501 (+ IDLE) for rmut's
    client. Connections are served concurrently — rmut keeps a second
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
    r.expect("! Jul 07 Petr")
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
    with open(gpg, "w") as f:
        f.write(
            '#!/bin/sh\ncase "$*" in\n'
            "*--decrypt*)\n"
            "  cat >/dev/null\n"
            '  echo "[GNUPG:] BEGIN_DECRYPTION" >&2\n'
            '  echo "[GNUPG:] DECRYPTION_OKAY" >&2\n'
            '  echo "[GNUPG:] GOODSIG AAA Jane <jane@example.com>" >&2\n'
            "  printf 'Content-Type: text/plain\\r\\n\\r\\nthe secret plan\\r\\n' ;;\n"
            "*--detach-sign*)\n"
            "  cat >/dev/null\n"
            '  echo "[GNUPG:] SIG_CREATED D 1 8 00 12 FPR" >&2\n'
            "  printf -- '-----BEGIN PGP SIGNATURE-----\\nAAAA\\n"
            "-----END PGP SIGNATURE-----\\n' ;;\n"
            "esac\nexit 0\n"
        )
    os.chmod(gpg, 0o755)
    config = os.path.join(tmp, "config.toml")
    with open(config, "w") as f:
        f.write(f'[pgp]\ncommand = "{gpg}"\n')
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
    r.expect("the secret plan", "decrypted", "good signature from Jane")
    r.keys(b"i")
    # Compose, pick sign from the security menu, send.
    r.keys(b"m")
    r.expect("To:")
    r.keys(b"bob@example.org\rsigned subject\r")  # editor appends the body
    r.expect("Send message?")
    r.keys(b"ss")  # security menu -> sign
    r.expect("[PGP: sign]")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="sendmail invoked")
    r.expect("message sent")
    sent = open(sent_file).read()
    assert "Content-Type: multipart/signed" in sent
    assert "micalg=pgp-sha256" in sent
    assert "BEGIN PGP SIGNATURE" in sent
    assert "signed body line" in sent
    r.keys(b"q")  # both messages were already seen — quits directly
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
    r.keys(b"r\r\ry")
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
    # block c/y/ctrl+o — it syncs silently on the way out, like q
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
    r.expect("Send message?")
    saw = open(seen).read()
    assert "To:" not in saw and "Subject:" not in saw, f"headers leaked: {saw!r}"
    r.keys(b"a")
    r.keys(attachment.encode() + b"\r")
    r.expect("[1 attachment(s)]")
    r.keys(b"y")
    wait_for(lambda: os.path.exists(sent_file), desc="sendmail ran")
    sent = open(sent_file).read()
    assert "To: petr@example.com" in sent, sent
    assert "Subject: headless subject" in sent, sent
    assert "only the body" in sent, sent
    assert 'filename="note.txt"' in sent, sent
    r.keys(b"q")
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
    r.expect("[1 attachment(s)]")
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
    r.keys(b"ey")
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
    r.expect("applied to 2", "Del:2")
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


SCENARIOS = [
    scenario_view_and_pager,
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
    scenario_message_commands,
    scenario_identities,
    scenario_tag_save_sort,
    scenario_import_muttrc,
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
