#!/usr/bin/env sh
set -e
ADDR=${ADDR:-127.0.0.1:8099}
C=${C:-50}
D=${D:-3}
P=${P:-1}
BIN=target/x86_64-unknown-linux-musl/release

cargo build --release
"$BIN/velo" run examples/api.velo "$ADDR" &
PID=$!
sleep 1
curl -s -XPOST "http://$ADDR/users" -d '{"name":"bench"}' >/dev/null

for path in /health /version /users /users/1 /stats /teams/1/members/2; do
  printf '%-26s ' "$path"
  "$BIN/velobench" -c "$C" -d "$D" -p "$P" "http://$ADDR$path" |
    awk '/throughput/{r=$2} /latency/{p50=$4; p99=$7} END{printf "%8s req/s   p50 %sms   p99 %sms\n", r, p50, p99}'
done

printf '%-26s ' "POST /users"
"$BIN/velobench" -c "$C" -d "$D" -m POST -b '{"name":"bench"}' "http://$ADDR/users" |
  awk '/throughput/{r=$2} /latency/{p50=$4; p99=$7} END{printf "%8s req/s   p50 %sms   p99 %sms\n", r, p50, p99}'

printf '%-26s %s kB\n' RSS "$(ps -o rss= -p $PID | tr -d ' ')"
kill $PID
