#!/usr/bin/env python3
"""Static server for the csp-origin pages that can also set response headers.

`python3 -m http.server` cannot send a Content-Security-Policy header, and these pages need
one: the engine builds a document's CSP list from the response headers, before the body is
parsed, which is the code path that used to panic. For every file `<name>.html` served, if
`<name>.html.csp` sits next to it, its first line is sent as that response's
`Content-Security-Policy` header.
"""
import functools
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class Handler(SimpleHTTPRequestHandler):
    def end_headers(self):
        path = self.translate_path(self.path)
        if path.endswith(".html"):
            try:
                with open(path + ".csp", "rb") as f:
                    value = f.readline().decode().strip()
            except OSError:
                value = ""
            if value:
                self.send_header("Content-Security-Policy", value)
        super().end_headers()

    def log_message(self, fmt, *args):
        sys.stderr.write("http %s\n" % (fmt % args))
        sys.stderr.flush()


if __name__ == "__main__":
    port = int(sys.argv[1])
    handler = functools.partial(Handler, directory=sys.argv[2])
    ThreadingHTTPServer(("127.0.0.1", port), handler).serve_forever()
