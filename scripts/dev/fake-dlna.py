#!/usr/bin/env python3
"""A stand-in for a DLNA/UPnP media server, for trying the app's network view and for
its smoke tests: it shows a folder of music as a server would (subfolders as
containers, music files as items) and hands the files out, also in parts.

    scripts/dev/fake-dlna.py <music folder> [--host 127.0.0.1] [--port 0] [--ssdp]

It prints "listening http://HOST:PORT/desc.xml" once it serves; that address is what
the app's "Add by Address…" takes. Only the Python standard library is used.

By default nothing is said to the network: the server only answers who asks it at its
address. With --ssdp it also answers searches for media servers on the local network
(so that the app finds it by itself), which needs port 1900.

The addresses it hands out carry numbers of its own table, never paths: nothing
outside the folder can be asked for.
"""
import argparse
import os
import re
import signal
import socket
import struct
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from xml.sax.saxutils import escape, quoteattr

KINDS = {
    ".flac": "audio/flac",
    ".mp3": "audio/mpeg",
    ".m4a": "audio/mp4",
    ".wav": "audio/wav",
    ".aiff": "audio/aiff",
    ".aif": "audio/aiff",
    ".ogg": "audio/ogg",
    ".dsf": "audio/x-dsf",
    ".dff": "audio/x-dff",
}
UDN = "uuid:0e0e0e0e-0000-4000-8000-6f6e65756d21"
NAME = "OnEum test server"


class Tree:
    """The folder as a table: entry 0 is the root, every folder and music file has a
    number, and a folder knows its children's numbers."""

    def __init__(self, root):
        self.entries = [{"path": root, "title": NAME, "folder": True, "children": []}]
        self._walk(0)

    def _walk(self, number):
        folder = self.entries[number]
        names = sorted(os.listdir(folder["path"]), key=str.lower)
        dirs = [n for n in names if os.path.isdir(os.path.join(folder["path"], n))]
        files = [n for n in names if os.path.splitext(n)[1].lower() in KINDS]
        for name in dirs + files:
            path = os.path.join(folder["path"], name)
            child = len(self.entries)
            is_folder = os.path.isdir(path)
            self.entries.append(
                {
                    "path": path,
                    "title": name if is_folder else os.path.splitext(name)[0],
                    "folder": is_folder,
                    "children": [],
                    "parent": number,
                }
            )
            folder["children"].append(child)
            if is_folder:
                self._walk(child)

    def didl(self, number, start, count, base):
        """The children of entry `number` from `start` on, `count` of them at most (0:
        all), as DIDL-Lite, with how many there are in all."""
        children = self.entries[number]["children"]
        chosen = children[start : start + count] if count else children[start:]
        out = [
            '<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" '
            'xmlns:dc="http://purl.org/dc/elements/1.1/" '
            'xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">'
        ]
        for child in chosen:
            entry = self.entries[child]
            title = escape(entry["title"])
            if entry["folder"]:
                out.append(
                    f'<container id="{child}" parentID="{number}" restricted="1" '
                    f'childCount="{len(entry["children"])}"><dc:title>{title}</dc:title>'
                    "<upnp:class>object.container.storageFolder</upnp:class></container>"
                )
                continue
            mime = KINDS[os.path.splitext(entry["path"])[1].lower()]
            album = self.entries[entry["parent"]]
            artist = self.entries[album.get("parent", 0)]
            size = os.path.getsize(entry["path"])
            out.append(
                f'<item id="{child}" parentID="{number}" restricted="1">'
                f"<dc:title>{title}</dc:title>"
                f"<upnp:album>{escape(album['title'])}</upnp:album>"
                f"<upnp:artist>{escape(artist['title'])}</upnp:artist>"
                "<upnp:class>object.item.audioItem.musicTrack</upnp:class>"
                f"<res protocolInfo={quoteattr('http-get:*:' + mime + ':*')} "
                f'size="{size}">{escape(base)}/media/{child}</res></item>'
            )
        out.append("</DIDL-Lite>")
        return "".join(out), len(chosen), len(children)


DESCRIPTION = f"""<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
<specVersion><major>1</major><minor>0</minor></specVersion>
<device>
<deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
<friendlyName>{NAME}</friendlyName>
<manufacturer>OnEum</manufacturer>
<modelName>fake-dlna</modelName>
<UDN>{UDN}</UDN>
<serviceList><service>
<serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
<serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>
<SCPDURL>/scpd.xml</SCPDURL>
<controlURL>/ctl</controlURL>
<eventSubURL>/evt</eventSubURL>
</service></serviceList>
</device>
</root>
"""

ENVELOPE = (
    '<?xml version="1.0" encoding="utf-8"?>'
    '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" '
    's:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>{}</s:Body>'
    "</s:Envelope>"
)


def field(body, name, default):
    found = re.search(rf"<{name}[^>]*>([^<]*)</{name}>", body)
    return found.group(1) if found else default


class Handler(BaseHTTPRequestHandler):
    tree = None

    def log_message(self, *args):  # quiet: the smoke tests read stdout
        pass

    def base(self):
        # As the client reached this server: an emulator sees the host under another
        # address than the one the server listens on.
        return "http://" + self.headers.get("Host", "%s:%d" % self.server.server_address)

    def reply(self, status, kind, body, extra=()):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_GET(self):
        if self.path == "/desc.xml":
            return self.reply(200, 'text/xml; charset="utf-8"', DESCRIPTION.encode())
        found = re.fullmatch(r"/media/(\d+)", self.path)
        entries = self.tree.entries
        if not found or int(found.group(1)) >= len(entries) or entries[int(found.group(1))]["folder"]:
            return self.reply(404, "text/plain", b"not here")
        path = entries[int(found.group(1))]["path"]
        size = os.path.getsize(path)
        kind = KINDS[os.path.splitext(path)[1].lower()]
        asked = re.fullmatch(r"bytes=(\d+)-(\d*)", self.headers.get("Range", ""))
        start, end, status, extra = 0, size - 1, 200, [("Accept-Ranges", "bytes")]
        if asked:
            start = int(asked.group(1))
            if asked.group(2):
                end = min(int(asked.group(2)), size - 1)
            if start >= size:
                return self.reply(416, "text/plain", b"", [("Content-Range", f"bytes */{size}")])
            status = 206
            extra.append(("Content-Range", f"bytes {start}-{end}/{size}"))
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(end - start + 1))
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        if self.command == "HEAD":
            return
        with open(path, "rb") as file:
            file.seek(start)
            left = end - start + 1
            while left > 0:
                chunk = file.read(min(left, 64 * 1024))
                if not chunk:
                    break
                try:
                    self.wfile.write(chunk)
                except OSError:  # the listener went elsewhere
                    return
                left -= len(chunk)

    do_HEAD = do_GET

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0") or 0)
        body = self.rfile.read(min(length, 1 << 20)).decode("utf-8", "replace")
        number = field(body, "ObjectID", "")
        entries = self.tree.entries
        if (
            self.path != "/ctl"
            or not number.isdigit()
            or int(number) >= len(entries)
            or not entries[int(number)]["folder"]
        ):
            fault = ENVELOPE.format(
                "<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>"
                '<detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0">'
                "<errorCode>701</errorCode><errorDescription>No such object"
                "</errorDescription></UPnPError></detail></s:Fault>"
            )
            return self.reply(500, 'text/xml; charset="utf-8"', fault.encode())
        start = int(field(body, "StartingIndex", "0") or 0)
        count = int(field(body, "RequestedCount", "0") or 0)
        didl, returned, total = self.tree.didl(int(number), start, count, self.base())
        answer = ENVELOPE.format(
            '<u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">'
            f"<Result>{escape(didl)}</Result><NumberReturned>{returned}</NumberReturned>"
            f"<TotalMatches>{total}</TotalMatches><UpdateID>1</UpdateID></u:BrowseResponse>"
        )
        self.reply(200, 'text/xml; charset="utf-8"', answer.encode())


def answer_searches(port):
    """Answers M-SEARCH for media servers, as sent to the multicast group or straight to
    this computer, with where the device description is."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if hasattr(socket, "SO_REUSEPORT"):
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    sock.bind(("", 1900))
    group = struct.pack("4s4s", socket.inet_aton("239.255.255.250"), socket.inet_aton("0.0.0.0"))
    try:
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, group)
    except OSError as error:
        print(f"no multicast here ({error}): only direct searches are answered", file=sys.stderr)
    while True:
        data, sender = sock.recvfrom(8192)
        text = data.decode("utf-8", "replace")
        if not text.startswith("M-SEARCH") or not re.search(r"MediaServer|ssdp:all|upnp:rootdevice", text):
            continue
        # The address of this computer as the asker reaches it.
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            probe.connect(sender)
            mine = probe.getsockname()[0]
        finally:
            probe.close()
        reply = (
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\n"
            f"LOCATION: http://{mine}:{port}/desc.xml\r\nSERVER: fake-dlna UPnP/1.0\r\n"
            "ST: urn:schemas-upnp-org:device:MediaServer:1\r\n"
            f"USN: {UDN}::urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n"
        )
        sock.sendto(reply.encode(), sender)


def main():
    parser = argparse.ArgumentParser(description="A stand-in DLNA media server for tests.")
    parser.add_argument("folder")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--ssdp", action="store_true", help="answer searches of the local network")
    options = parser.parse_args()
    if not os.path.isdir(options.folder):
        sys.exit(f"not a folder: {options.folder}")
    Handler.tree = Tree(os.path.abspath(options.folder))
    server = ThreadingHTTPServer((options.host, options.port), Handler)
    server.daemon_threads = True
    port = server.server_address[1]
    if options.ssdp:
        threading.Thread(target=answer_searches, args=(port,), daemon=True).start()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    print(f"listening http://{options.host}:{port}/desc.xml", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
