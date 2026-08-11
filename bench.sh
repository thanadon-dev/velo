#!/usr/bin/env sh
set -e
ADDR=${ADDR:-127.0.0.1:8099}
N=${N:-30000}
C=${C:-50}
BIN=target/x86_64-unknown-linux-musl/release/velo

cargo build --release
"$BIN" run examples/api.velo "$ADDR" &
PID=$!
sleep 1
curl -s -XPOST "http://$ADDR/users" -d '{"name":"bench"}' >/dev/null

for path in /health /version /users /users/1 /stats /teams/1/members/2; do
  rps=$(ab -k -c "$C" -n "$N" "http://$ADDR$path" 2>/dev/null | awk '/Requests per second/{print $4}')
  printf '%-24s %s req/s\n' "$path" "$rps"
done

printf '%-24s %s kB\n' RSS "$(ps -o rss= -p $PID | tr -d ' ')"
kill $PID
