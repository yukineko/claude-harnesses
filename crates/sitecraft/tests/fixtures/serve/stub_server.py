"""Stub dev server for crates/sitecraft/tests/serve.rs (standard library only).

Usage: python3 stub_server.py <mode> <port> <outdir>

Every mode first writes, into <outdir>:
  marker          proof that sitecraft actually spawned the command
  leader.pid      this process's pid
  pgid            this process's process-group id
  grandchild.pid  pid of a `sleep` child forked into the same process group

Modes:
  serve    answer every GET with HTTP 200 on 127.0.0.1:<port>
  noready  never listen (the ready probe must hit its deadline)
  exit1    exit with status 1 immediately (early exit before ready)

Safety net against orphans if a test or the implementation under test fails
to clean up: the process exits on its own after LIFETIME seconds and the
grandchild `sleep` is bounded by the same value.
"""

import http.server
import os
import subprocess
import sys
import threading
import time

LIFETIME = 90


def write(outdir, name, value):
    path = os.path.join(outdir, name)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(value))
    os.rename(tmp, path)


class Ok(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<!doctype html><title>stub</title>ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def main():
    mode, port, outdir = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    threading.Timer(LIFETIME, lambda: os._exit(3)).start()
    write(outdir, "marker", mode)
    write(outdir, "leader.pid", os.getpid())
    write(outdir, "pgid", os.getpgrp())
    if mode == "exit1":
        sys.stdout.flush()
        os._exit(1)
    child = subprocess.Popen(["sleep", str(LIFETIME)])
    write(outdir, "grandchild.pid", child.pid)
    if mode == "noready":
        time.sleep(LIFETIME)
        os._exit(3)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Ok)
    server.daemon_threads = True
    server.serve_forever()


if __name__ == "__main__":
    main()
