#!/usr/bin/env python3
"""Drive a program through a real pty, so a terminal program can be tested without a person.

The window puts the terminal in raw mode with `tcgetattr(0)`, which fails against a pipe, so
anything that checks it must give it a terminal. This is that terminal.

Why not `script(1)`: it was the first choice and it hangs. `script` cannot reliably feed a pty
non-interactively — the child sees end-of-input before the piped bytes are readable, and a
program that reads one byte at a time then waits forever on a terminal nobody is typing into
(the M8 smoke job sat for 25 minutes before this was found). A pty the driver owns has no such
ambiguity: keys go in on a schedule, output is read until the child exits or the deadline passes,
and the deadline is enforced with a kill rather than hoped for.

Usage:
    drive.py --timeout 30 --out session.bin -- <argv...>  (keys on stdin as JSON: [[delay, text]])

Exits 0 when the child exited 0 within the deadline, 1 otherwise, and prints a one-line summary so
a caller can grep it the way the other measured jobs grep their markers.
"""

import argparse
import json
import os
import pty
import select
import signal
import sys
import time


def drive(argv, keys, timeout, size=None):
    """Run `argv` on a pty, feed `keys` (a list of (delay_seconds, text)), return (output, code).

    `code` is the child's exit status, or `None` if it had to be killed at the deadline. `None`
    is distinct from a non-zero status on purpose: a program that hung is a different bug from a
    program that failed, and the caller reports them differently.

    `size` is an optional `(columns, rows)` applied to the pty's window before the program starts.
    A pty otherwise inherits the *caller's* terminal size, so a test run under a narrow terminal
    (or a pipe whose ioctl fails) renders a wrapped screen and an assertion about visible text
    fails for a reason unrelated to the program. A caller that greps the screen should declare the
    window it expects.
    """
    pid, fd = pty.fork()
    if pid == 0:  # child
        if size is not None:
            columns, rows = size
            try:
                import fcntl
                import struct
                import termios

                fcntl.ioctl(
                    sys.stdout.fileno(),
                    termios.TIOCSWINSZ,
                    struct.pack("HHHH", rows, columns, 0, 0),
                )
            except OSError:
                pass
        try:
            os.execv(argv[0], argv)
        except OSError as error:
            os.write(2, f"drive: cannot exec {argv[0]}: {error}\n".encode())
            os._exit(127)

    output = bytearray()
    deadline = time.monotonic() + timeout
    pending = list(keys)
    next_key_at = time.monotonic() + (pending[0][0] if pending else 0.0)
    code = None

    while True:
        now = time.monotonic()
        if now > deadline:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break

        readable, _, _ = select.select([fd], [], [], 0.02)
        if readable:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                # The child closed the pty; whatever it wrote is already in `output`.
                break
            if not chunk:
                break
            output.extend(chunk)

        # Send the next key once its delay has elapsed. Delays let the child reach its prompt,
        # which matters for a raw-mode program that must install termios before reading.
        if pending and now >= next_key_at:
            _, text = pending.pop(0)
            try:
                os.write(fd, text.encode())
            except OSError:
                break
            next_key_at = time.monotonic() + (pending[0][0] if pending else 0.0)

        # All keys sent and the child has been quiet: reap it if it has exited on its own.
        if not pending:
            reaped, status = os.waitpid(pid, os.WNOHANG)
            if reaped == pid:
                code = os.waitstatus_to_exitcode(status)
                break

    if code is None:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            _, status = os.waitpid(pid, 0)
            # Only report a code if the child had already exited before the kill.
            if os.WIFEXITED(status) and not os.WIFSIGNALED(status):
                code = os.waitstatus_to_exitcode(status)
        except ChildProcessError:
            pass

    try:
        os.close(fd)
    except OSError:
        pass
    return bytes(output), code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--out", default=None, help="write the session output here")
    parser.add_argument("--keys", default=None, help="JSON file of [[delay, text], ...]")
    parser.add_argument(
        "--cols", type=int, default=None, help="pty width in columns (default: inherited)"
    )
    parser.add_argument(
        "--rows", type=int, default=None, help="pty height in rows (default: inherited)"
    )
    parser.add_argument("argv", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    argv = args.argv
    if argv and argv[0] == "--":
        argv = argv[1:]
    if not argv:
        parser.error("no program to run after --")

    keys = json.load(open(args.keys)) if args.keys else []
    size = None
    if args.cols is not None and args.rows is not None:
        size = (args.cols, args.rows)
    output, code = drive(argv, [(float(d), t) for d, t in keys], args.timeout, size)

    if args.out:
        with open(args.out, "wb") as handle:
            handle.write(output)

    sys.stdout.write(f"HOLONOMY_DRIVE exit={code} bytes={len(output)}\n")
    sys.stdout.flush()
    if code != 0:
        sys.stderr.write(f"drive: {'killed at the deadline' if code is None else f'exit {code}'}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
