#!/bin/sh
# LucinaNet profile across a kill and a restart, on Juno (or any machine with the binary). Real sites: httpbin.org sets the cookie.
#   tests/profile/restart.sh [lucinanet binary] [profile dir]
# Pass: the second run sees the cookie the server set, and localStorage and IndexedDB values the first run wrote.
set -u
bin=${1:-/usr/bin/lucinanet}
profile=${2:-/data/lucina/net-test}
here=$(cd "$(dirname "$0")" && pwd)
rm -rf "$profile"; mkdir -p "$profile"
# IndexedDB is experimental in Servo and off by default (dom_indexeddb_enabled); the test turns it on.
export LUCINANET_PROFILE_DIR=$profile LUCINANET_CONSOLE=1 LUCINANET_PREFS=dom_indexeddb_enabled=true
tag=juno$$
# First run: the server sets cookie lucina=$tag; the page writes localStorage and IndexedDB; the engine runs 40 s (one cookie save,
# every 30 s), then ends with _exit, as a killed browser: no exit-time save.
LUCINANET_USER_JS=$here/set.js LUCINANET_RENDER_HOLD=40 "$bin" --render "https://httpbin.org/cookies/set?lucina=$tag" /tmp/profile-1.png 2>&1 | grep -E "profile-test|render:" 
echo "profile files: $(cd "$profile" && find . -type f | sort | tr '\n' ' ')"
LUCINANET_USER_JS=$here/get.js "$bin" --render "https://httpbin.org/cookies" /tmp/profile-2.png 2>&1 | grep -E "profile-test|render:" > /tmp/profile-2.log
cat /tmp/profile-2.log
if grep -q "lucina=$tag" /tmp/profile-2.log && grep -q "ls=ls-httpbin.org" /tmp/profile-2.log && grep -q "idb=idb-httpbin.org" /tmp/profile-2.log; then
    echo "profile-test: PASS (cookie, localStorage, IndexedDB kept across a kill)"; rm -rf "$profile" /tmp/profile-1.png /tmp/profile-2.png /tmp/profile-2.log
else
    echo "profile-test: FAIL"; exit 1
fi
