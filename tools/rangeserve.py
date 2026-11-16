#!/usr/bin/env python3
"""Serve one file over HTTP with Range requests and an ETag, the way nginx serves the planet.

For tools/cache.sh: a server the test can stop (offline) and restart under another ETag (the
archive replaced), with every request printed so the test can count what reached the network.

  tools/rangeserve.py FILE PORT ETAG

A request whose If-Range is not ETAG gets 200 and the whole file, as nginx answers.
"""

import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PATH, PORT, ETAG = sys.argv[1], int(sys.argv[2]), sys.argv[3]
SIZE = os.path.getsize(PATH)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        m = re.fullmatch(r"bytes=(\d+)-(\d+)", self.headers.get("Range", ""))
        if_range = self.headers.get("If-Range")
        if not m or (if_range is not None and if_range != ETAG):
            start, end, status = 0, SIZE - 1, 200
        else:
            start, end, status = int(m[1]), min(int(m[2]), SIZE - 1), 206
        print(f"[range] {status} bytes={start}-{end} if-range={if_range}", flush=True)
        self.send_response(status)
        self.send_header("ETag", ETAG)
        self.send_header("Content-Length", str(end - start + 1))
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{SIZE}")
        self.end_headers()
        with open(PATH, "rb") as f:
            f.seek(start)
            left = end - start + 1
            while left:
                chunk = f.read(min(left, 1 << 20))
                if not chunk:
                    break
                try:
                    self.wfile.write(chunk)
                except (BrokenPipeError, ConnectionResetError):
                    return  # the client refused a 200 and hung up: expected
                left -= len(chunk)

    def log_message(self, format, *args):  # quiet: the [range] lines are the log
        pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
