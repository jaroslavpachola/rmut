#!/usr/bin/env python3
"""End-to-end tests: drive the rmut binary in a pty and assert on
screens and on-disk maildir state.

Screen matching note: ratatui redraws only changed cells, so captured
output loses spaces between unchanged regions. All assertions therefore
match against whitespace-squashed, ANSI-stripped text.
"""

import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
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
    }
    env.update(extra or {})
    return env


def scenario_view_and_pager(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "petr", "ci"])
    r = Rmut(md, base_env(tmp))
    r.expect("Msgs:3", "New:1", "Lunch on Friday?", "Schůzka zítra", "CI failed on main")
    r.keys(b"\r")  # newest (Petr, new) opens in the pager
    r.expect("sejdeme se zítra v 9:00", "Message 3/3")
    r.keys(b"K")  # previous message
    r.expect("Are you free for lunch")
    r.keys(b"i")
    r.keys(b"q")  # pending read-marks -> quit prompt
    r.expect("save & quit")
    r.keys(b"n")
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
    r.keys(b"$")
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
        f.write("alias petr Petr Novak <petr@example.com>\n")
    env = base_env(tmp, {
        "EDITOR": editor,
        "RMUT_SENDMAIL": sendmail,
        "RMUT_ALIASES": aliases,
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
    r.keys(b"q")
    r.close()


def scenario_config(tmp):
    md = make_maildir(tmp, "md")
    write_msgs(md, ["jane", "ci"])
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
format = "%C|%-4.4F|%s"
[keys.index]
sync = "w"
"""
        )
    r = Rmut(md, base_env(tmp, {"RMUT_CONFIG": cfg}))
    r.expect("1|buil|CI failed on main", "2|Jane|Lunch on Friday?")
    r.keys(b"=d")  # first message (CI), mark deleted
    r.keys(b"w")  # remapped sync
    r.expect("synced: 1 deleted")
    wait_for(
        lambda: not any("1751750100" in f for f in os.listdir(os.path.join(md, "cur"))),
        desc="deleted file removed",
    )
    # help screen shows the remapped key and, after paging, the patterns
    r.keys(b"?")
    r.expect("write changes to the maildir")
    r.keys(b" ")
    r.expect("Patterns")
    r.keys(b"q")
    # new-mail detection: drop a message into new/ and wait for the poll
    write_msgs(md, ["petr"])
    r.expect("new mail in", "+1", timeout=8)
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


SCENARIOS = [
    scenario_view_and_pager,
    scenario_sync_delete_flag_limit,
    scenario_threads_and_fold,
    scenario_compose_send_postpone,
    scenario_config,
    scenario_send_via_config_sendmail,
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
