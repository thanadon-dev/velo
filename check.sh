#!/usr/bin/env sh
set -e
BIN=target/x86_64-unknown-linux-musl/release
ADDR=${ADDR:-127.0.0.1:8098}

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release

"$BIN/velo" check examples/api.velo
"$BIN/velo" check examples/todo.velo
"$BIN/velo" check examples/shop/app.velo
"$BIN/velo" run examples/api.velo "$ADDR" &
PID=$!
sleep 1
test "$(curl -sf "http://$ADDR/health")" = ok
curl -sf -XPOST "http://$ADDR/users" -d '{"name":"check"}' | grep -q '"id":1'
"$BIN/velobench" -c 8 -d 2 "http://$ADDR/health" | grep throughput
kill $PID
echo "all checks passed"
