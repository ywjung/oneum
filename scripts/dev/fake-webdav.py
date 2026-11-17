#!/usr/bin/env python3
"""A stand-in for a NAS that shares a folder over WebDAV, for trying the app's network
folders and for its smoke tests: it lists a folder (PROPFIND) and hands its files out,
also in parts (GET with Range), to who gives the right name and password.

    scripts/dev/fake-webdav.py <folder> [--host 127.0.0.1] [--port 0]
        [--user NAME --password WORD] [--auth basic|digest]
        [--tls CERT.pem KEY.pem] [--prefix /dav]

It prints "listening http://HOST:PORT/PREFIX/" (https with --tls) once it serves; that
address is what the app's "Add a Network Folder…" takes. Only the Python standard
library is used. Without --user anyone may read. Nothing is ever written: the server
knows OPTIONS, PROPFIND, HEAD and GET.

Every request is counted in what it prints at the end ("requests N, refused M"), and
with --log each one is printed as it comes (the password never is).
"""
import argparse
import base64
import hashlib
import os
import secrets
import signal
import ssl
import sys
import time
import urllib.parse
from email.utils import formatdate
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from xml.sax.saxutils import escape

REALM = "OnEum test share"


def parse_args():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("folder")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=0)
    p.add_argument("--user")
    p.add_argument("--password")
    p.add_argument("--auth", choices=["basic", "digest"], default="basic")
    p.add_argument("--tls", nargs=2, metavar=("CERT", "KEY"))
    p.add_argument("--prefix", default="/dav")
    p.add_argument("--log", action="store_true")
    return p.parse_args()


ARGS = parse_args()
ROOT = os.path.realpath(ARGS.folder)
PREFIX = "/" + ARGS.prefix.strip("/")
NONCES = {}
COUNT = {"requests": 0, "refused": 0}


def md5(text):
    return hashlib.md5(text.encode("utf-8")).hexdigest()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "FakeDAV/1"

    def log_message(self, fmt, *args):
        if ARGS.log:
            sys.stderr.write("%s %s\n" % (self.command, self.path))

    # --- who is asking ---------------------------------------------------------------
    def allowed(self):
        if not ARGS.user:
            return True
        header = self.headers.get("Authorization", "")
        if ARGS.auth == "basic" and header.startswith("Basic "):
            try:
                name, _, word = base64.b64decode(header[6:]).decode("utf-8").partition(":")
            except Exception:
                return False
            return secrets.compare_digest(name, ARGS.user) and secrets.compare_digest(word, ARGS.password)
        if ARGS.auth == "digest" and header.startswith("Digest "):
            fields = {}
            for part in header[7:].split(","):
                key, _, value = part.strip().partition("=")
                fields[key] = value.strip('"')
            if fields.get("username") != ARGS.user or fields.get("nonce") not in NONCES:
                return False
            ha1 = md5("%s:%s:%s" % (ARGS.user, REALM, ARGS.password))
            ha2 = md5("%s:%s" % (self.command, fields.get("uri", "")))
            if fields.get("qop") == "auth":
                expected = md5(":".join([ha1, fields["nonce"], fields.get("nc", ""), fields.get("cnonce", ""), "auth", ha2]))
            else:
                expected = md5(":".join([ha1, fields["nonce"], ha2]))
            return secrets.compare_digest(expected, fields.get("response", ""))
        return False

    def refuse(self):
        COUNT["refused"] += 1
        self.send_response(401)
        if ARGS.auth == "basic":
            self.send_header("WWW-Authenticate", 'Basic realm="%s"' % REALM)
        else:
            nonce = secrets.token_hex(16)
            NONCES[nonce] = time.time()
            self.send_header("WWW-Authenticate", 'Digest realm="%s", qop="auth", nonce="%s", algorithm=MD5' % (REALM, nonce))
        self.send_header("Content-Length", "0")
        self.end_headers()

    # --- what is asked for -----------------------------------------------------------
    def local(self):
        """The file or folder the request names, or None (outside the share)."""
        path = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
        if path != PREFIX and not path.startswith(PREFIX + "/"):
            return None
        local = os.path.realpath(os.path.join(ROOT, path[len(PREFIX):].lstrip("/")))
        if local != ROOT and not local.startswith(ROOT + os.sep):
            return None
        return local

    def href(self, local, folder):
        rel = os.path.relpath(local, ROOT)
        rel = "" if rel == "." else rel
        path = PREFIX + "/" + "/".join(urllib.parse.quote(p) for p in rel.split(os.sep) if p)
        if folder and not path.endswith("/"):
            path += "/"
        return path

    def entry(self, local):
        folder = os.path.isdir(local)
        stat = os.stat(local)
        props = "<D:displayname>%s</D:displayname>" % escape(os.path.basename(local) or "share")
        props += "<D:getlastmodified>%s</D:getlastmodified>" % formatdate(stat.st_mtime, usegmt=True)
        if folder:
            props += "<D:resourcetype><D:collection/></D:resourcetype>"
        else:
            props += "<D:resourcetype/><D:getcontentlength>%d</D:getcontentlength>" % stat.st_size
        return (
            "<D:response><D:href>%s</D:href><D:propstat><D:prop>%s</D:prop>"
            "<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        ) % (escape(self.href(local, folder)), props)

    def answer(self, status, body=b"", kind="text/plain", extra=()):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        for key, value in extra:
            self.send_header(key, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def begin(self):
        COUNT["requests"] += 1
        length = int(self.headers.get("Content-Length", "0") or 0)
        if length:
            self.rfile.read(length)
        if not self.allowed():
            self.refuse()
            return None
        local = self.local()
        if local is None or not os.path.exists(local):
            self.answer(404)
            return None
        return local

    def do_OPTIONS(self):
        COUNT["requests"] += 1
        self.answer(200, extra=[("DAV", "1"), ("Allow", "OPTIONS, PROPFIND, GET, HEAD")])

    def do_PROPFIND(self):
        local = self.begin()
        if local is None:
            return
        parts = [self.entry(local)]
        if os.path.isdir(local) and self.headers.get("Depth", "1") != "0":
            for name in sorted(os.listdir(local), key=str.lower):
                if not name.startswith("."):
                    parts.append(self.entry(os.path.join(local, name)))
        body = '<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">%s</D:multistatus>' % "".join(parts)
        self.answer(207, body.encode("utf-8"), 'application/xml; charset="utf-8"')

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        local = self.begin()
        if local is None:
            return
        if os.path.isdir(local):
            self.answer(405)
            return
        size = os.path.getsize(local)
        first, last, status = 0, size - 1, 200
        asked = self.headers.get("Range", "")
        if asked.startswith("bytes="):
            a, _, b = asked[6:].partition("-")
            try:
                first = int(a) if a else max(0, size - int(b))
                last = min(int(b), size - 1) if a and b else size - 1
            except ValueError:
                first, last = 0, size - 1
            if first >= size:
                self.answer(416, extra=[("Content-Range", "bytes */%d" % size)])
                return
            status = 206
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(last - first + 1))
        if status == 206:
            self.send_header("Content-Range", "bytes %d-%d/%d" % (first, last, size))
        self.end_headers()
        if self.command == "HEAD":
            return
        with open(local, "rb") as f:
            f.seek(first)
            left = last - first + 1
            while left > 0:
                piece = f.read(min(65536, left))
                if not piece:
                    break
                try:
                    self.wfile.write(piece)
                except (BrokenPipeError, ConnectionResetError):
                    return
                left -= len(piece)


def main():
    if bool(ARGS.user) != bool(ARGS.password):
        sys.exit("--user and --password go together")
    server = ThreadingHTTPServer((ARGS.host, ARGS.port), Handler)
    server.daemon_threads = True
    scheme = "http"
    if ARGS.tls:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(ARGS.tls[0], ARGS.tls[1])
        server.socket = context.wrap_socket(server.socket, server_side=True)
        scheme = "https"
    host, port = server.server_address[:2]
    print("listening %s://%s:%d%s/" % (scheme, host, port, PREFIX), flush=True)

    def stop(*_):
        print("requests %d, refused %d" % (COUNT["requests"], COUNT["refused"]), flush=True)
        os._exit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    server.serve_forever()


if __name__ == "__main__":
    main()
