# Sourced by the smoke tests: the checks of a network folder (a share of a NAS over
# WebDAV), which are the same on every system. They use the caller's `ui`, `check` and
# `entry`, its $WORK and $MUSIC, and leave the servers' process ids in DAV_PIDS for the
# caller's cleanup.
#
#   webdav_checks <host the app reaches this computer at> <host to listen on>
#
# Three stand-ins for a NAS share the test library (scripts/dev/fake-webdav.py): one over
# http:// that asks for a name and a password by Digest, one over https:// with a
# certificate of its own making that asks by Basic, and one over http:// that asks by
# Basic (which is never answered there). The app is given their addresses: nothing is
# asked of the real network. The password is one made for the test.
DAV_PIDS=
webdav_checks() {
  dav_host=$1
  dav_listen=$2
  dav_word=smoke-test-word
  openssl req -x509 -newkey rsa:2048 -nodes -keyout "$WORK/dav-key.pem" -out "$WORK/dav-cert.pem" \
    -days 2 -subj "/CN=oneum-smoke" -addext "subjectAltName=IP:127.0.0.1,IP:10.0.2.2,DNS:localhost" >/dev/null 2>&1
  dav_print=$(openssl x509 -in "$WORK/dav-cert.pem" -noout -fingerprint -sha256 | sed 's/.*=//' | tr -d ':' | tr 'A-F' 'a-f')
  python3 scripts/dev/fake-webdav.py "$MUSIC" --host "$dav_listen" --user tester --password "$dav_word" --auth digest > "$WORK/dav.out" 2>&1 &
  DAV_PIDS=$!
  python3 scripts/dev/fake-webdav.py "$MUSIC" --host "$dav_listen" --user tester --password "$dav_word" --auth basic \
    --tls "$WORK/dav-cert.pem" "$WORK/dav-key.pem" > "$WORK/davs.out" 2>&1 &
  DAV_PIDS="$DAV_PIDS $!"
  python3 scripts/dev/fake-webdav.py "$MUSIC" --host "$dav_listen" --user tester --password "$dav_word" --auth basic > "$WORK/davb.out" 2>&1 &
  DAV_PIDS="$DAV_PIDS $!"
  i=0
  until grep -q '^listening ' "$WORK/dav.out" 2>/dev/null && grep -q '^listening ' "$WORK/davs.out" 2>/dev/null \
      && grep -q '^listening ' "$WORK/davb.out" 2>/dev/null; do
    sleep 0.2; i=$((i + 1))
    [ $i -lt 50 ] || { echo "the stand-in network folders did not start" >&2; exit 1; }
  done
  dav=$(sed -n "s/^listening \(https*:\/\/\)[^:]*:/\1$dav_host:/p" "$WORK/dav.out")
  davs=$(sed -n "s/^listening \(https*:\/\/\)[^:]*:/\1$dav_host:/p" "$WORK/davs.out")
  davb=$(sed -n "s/^listening \(https*:\/\/\)[^:]*:/\1$dav_host:/p" "$WORK/davb.out")

  ui "net folder-add $dav tester wrong-word" 'wait 2000' 'dump w1.json' "net folder-add $dav tester $dav_word" 'wait 2500' 'dump w2.json'
  check w1.json 'd["network"]["share_failed"] == "refused" and d["network"]["shares"] == []' "a network folder is not added with the wrong password"
  check w2.json 'd["network"]["in_share"] and d["network"]["folders"] == 5 and d["network"]["entries"] == 7' "with the right one it opens: the share's 5 folders and 2 songs"
  check w2.json 'len(d["network"]["shares"]) == 1 and d["network"]["shares"][0]["user"] == "tester" and d["network"]["shares"][0]["url"] == "'"$dav"'"' "and is kept in the list, with its address and the name to be there as"
  ui "net open $(entry w2.json 아이유)" 'wait 1500' 'net open 0' 'wait 1500' 'dump w3.json'
  check w3.json 'd["network"]["depth"] == 2 and d["network"]["playable"] == 4' "the album in a folder of it has 4 songs that can be played"
  ui 'net open 1' 'wait 3000' 'dump w4.json' 'wait 1500' 'dump w5.json'
  check w4.json 'd["state"] == "Playing" and d["queue_len"] == 4 and d["queue_index"] == 1' "a song of the folder plays as the folder's account, with its folder as the queue"
  check w4.json 'd["current_path"].startswith("'"$dav"'") and "'"$dav_word"'" not in str(d)' "its address carries neither name nor password, and the app's state tells no password"
  check w5.json 'd["position_ms"] > 3500 and d["underruns"] == 0' "the music goes on without a break in the sound"

  ui 'net up' 'wait 400' 'net up' 'wait 400' 'net up' 'wait 400' "net folder-add $davs tester $dav_word" 'wait 2500' 'dump w6.json' \
     "net folder-trust $davs tester $dav_word" 'wait 2500' 'dump w7.json'
  check w6.json 'd["network"]["share_failed"] == "untrusted" and len(d["network"]["shares"]) == 1' "a server whose certificate no authority vouches for is refused"
  check w7.json 'd["network"]["in_share"] and d["network"]["folders"] == 5 and len(d["network"]["shares"]) == 2' "once the listener trusts the certificate the folder opens, over TLS"
  check w7.json 'd["network"]["shares"][1]["pinned"] == "'"$dav_print"'" and d["network"]["shares"][0]["pinned"] is None' "what is trusted is that one certificate, by its fingerprint"
  ui "net open $(entry w7.json 아이유)" 'wait 1500' 'net open 0' 'wait 1500' 'net open 2' 'wait 3000' 'dump w8.json'
  check w8.json 'd["state"] == "Playing" and d["queue_index"] == 2 and d["current_path"].startswith("'"$davs"'") and d["underruns"] == 0' "a song is read over the encrypted connection"
  ui 'net up' 'wait 400' 'net up' 'wait 400' 'net up' 'wait 400' "net folder-add $davb tester $dav_word" 'wait 2500' 'dump w10.json'
  check w10.json 'd["network"]["share_failed"] == "plain password" and len(d["network"]["shares"]) == 2' "a server that takes the password only as it is, at an http:// address, is not given it (and the folder is not added)"
  ui 'net secret-selftest' 'wait 1000' 'net up' 'wait 400' 'net up' 'wait 400' 'net up' 'wait 400' 'net folder-forget 1' 'wait 300' \
     'net folder-forget 0' 'wait 500' 'dump w9.json'
  check w9.json 'd["network"]["shares"] == [] and d["network"]["server"] is None' "the folders are taken off the list (and their passwords forgotten)"
  for dav_pid in $DAV_PIDS; do kill "$dav_pid" 2>/dev/null || true; done
  DAV_PIDS=
}
