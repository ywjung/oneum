#!/usr/bin/env python3
"""A stand-in for the lyrics service (LRCLIB), for trying the app's online lyrics and
for its smoke tests: it answers GET /api/get?track_name=…&artist_name=… with synced
lyrics made up from the song's title, and writes down what it was asked.

    scripts/dev/fake-lyrics.py [--host 127.0.0.1] [--port 0] [--log FILE]
        [--unknown TITLE ...] [--plain TITLE ...]

It prints "listening http://HOST:PORT" once it serves; that address is what the app's
verification command `menu lyrics_service <address>` takes. A title given with
--unknown is answered as the service answers a song it does not know (404), one given
with --plain with words that have no times. With --log every request is written to
FILE as one line of JSON: its query (names and values as they were meant) and the
names of its headers. Only the Python standard library is used.
"""
import argparse
import json
import os
import signal
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
p.add_argument("--host", default="127.0.0.1")
p.add_argument("--port", type=int, default=0)
p.add_argument("--log")
p.add_argument("--unknown", nargs="*", default=[])
p.add_argument("--plain", nargs="*", default=[])
ARGS = p.parse_args()
COUNT = {"requests": 0}
# When each line is sung, in seconds.
TIMES = [0.5, 2.0, 3.5, 5.0, 6.5, 8.0]


def stamp(seconds):
    return "[%02d:%05.2f]" % (int(seconds // 60), seconds % 60)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "FakeLyrics/1"

    def log_message(self, fmt, *args):
        pass

    def answer(self, status, value):
        body = json.dumps(value, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        COUNT["requests"] += 1
        parts = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(parts.query, keep_blank_values=True))
        if ARGS.log:
            with open(ARGS.log, "a", encoding="utf-8") as log:
                line = {"path": parts.path, "query": query, "headers": sorted(k.lower() for k in self.headers.keys())}
                log.write(json.dumps(line, ensure_ascii=False) + "\n")
        title = query.get("track_name", "")
        seconds = query.get("duration", "1")
        if parts.path != "/api/get" or not title or not query.get("artist_name"):
            self.answer(400, {"statusCode": 400, "name": "BadRequest", "message": "what song?"})
        elif not seconds.isdigit() or not 1 <= int(seconds) <= 3600:
            # (As the real service: it takes no length of more than an hour.)
            self.answer(400, {"statusCode": 400, "name": "ValidationError", "message": "duration: must be between 1 and 3600"})
        elif title in ARGS.unknown:
            self.answer(404, {"code": 404, "name": "TrackNotFound", "message": "Failed to find specified track"})
        else:
            words = ["%s, line %d" % (title, n + 1) for n in range(len(TIMES))]
            synced = None if title in ARGS.plain else "\n".join("%s %s" % (stamp(t), w) for t, w in zip(TIMES, words))
            self.answer(200, {
                "id": COUNT["requests"], "trackName": title, "artistName": query.get("artist_name"),
                "albumName": query.get("album_name", ""), "instrumental": False,
                "plainLyrics": "\n".join(words), "syncedLyrics": synced,
            })


def main():
    server = ThreadingHTTPServer((ARGS.host, ARGS.port), Handler)
    server.daemon_threads = True
    host, port = server.server_address[:2]
    print("listening http://%s:%d" % (host, port), flush=True)

    def stop(*_):
        print("requests %d" % COUNT["requests"], flush=True)
        os._exit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    server.serve_forever()


if __name__ == "__main__":
    main()
