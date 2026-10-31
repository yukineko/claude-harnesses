"""Dev server whose listener escapes the process group (standard library only).

Used by crates/sitecraft/tests/serve_stop_port.rs.

Usage: python3 escaping_server.py <mode> <port> <outdir>

The leader forks a daemon child. The child calls os.setsid() (new session,
so a new process group: killpg on the leader's group no longer reaches it),
binds 127.0.0.1:<port>, records its pid, and only then tells the leader (via a
pipe) that it is in place. Files written into <outdir>:
  leader.pid   the leader's pid
  pgid         the leader's process-group id (the group sitecraft records)
  daemon.pid   the escaped daemon's pid (it is also its own pgid / sid)

Modes:
  idle   the daemon answers every GET with HTTP 200; the leader idles, so
         `serve start` succeeds (the leader is alive and the port answers).
         `serve stop` then kills the leader's group, the daemon survives.
  exit   the daemon only listens (never answers HTTP); the leader exits
         right after the daemon is in place, so `serve start` takes the
         "exited before it was ready" path while the daemon holds the port.

Safety net against orphans: both processes exit on their own after LIFETIME
seconds; the tests also SIGKILL daemon.pid / pgid in a drop guard.
"""

import http.server
import os
import socket
import sys
import threading
import time

LIFETIME = 60


def write(outdir, name, value):
    path = os.path.join(outdir, name)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(value))
    os.rename(tmp, path)


class Ok(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<!doctype html><title>escaped</title>ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def daemon(mode, port, outdir, wfd):
    os.setsid()
    threading.Timer(LIFETIME, lambda: os._exit(3)).start()
    if mode == "idle":
        server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Ok)
        server.daemon_threads = True
    else:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", port))
        sock.listen(64)
    write(outdir, "daemon.pid", os.getpid())
    os.write(wfd, b"1")
    os.close(wfd)
    if mode == "idle":
        server.serve_forever()
    else:
        time.sleep(LIFETIME)
    os._exit(0)


def main():
    mode, port, outdir = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    write(outdir, "leader.pid", os.getpid())
    write(outdir, "pgid", os.getpgrp())
    rfd, wfd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(rfd)
        try:
            daemon(mode, port, outdir, wfd)
        finally:
            os._exit(4)
    os.close(wfd)
    # Wait until the daemon has left the group and holds the port.
    os.read(rfd, 1)
    os.close(rfd)
    if mode == "exit":
        os._exit(1)
    threading.Timer(LIFETIME, lambda: os._exit(3)).start()
    time.sleep(LIFETIME)
    os._exit(3)


if __name__ == "__main__":
    main()
