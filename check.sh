#!/usr/bin/env sh
set -e
BIN=target/x86_64-unknown-linux-musl/release
ADDR=${ADDR:-127.0.0.1:8098}

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
cargo run --release --quiet --example embed >/dev/null

"$BIN/velo" check examples/api.velo
"$BIN/velo" check examples/todo.velo
"$BIN/velo" check examples/shop/app.velo
"$BIN/velo" check examples/auth.velo
"$BIN/velomicro" --check bench/baseline.json
"$BIN/velo" run examples/api.velo "$ADDR" &
PID=$!
sleep 1
test "$(curl -sf --max-time 10 "http://$ADDR/health")" = ok
curl -sf --max-time 10 -XPOST "http://$ADDR/users" -d '{"name":"check"}' | grep -q '"id":1'
"$BIN/velobench" -c 8 -d 2 "http://$ADDR/health" | grep throughput
kill $PID
wait $PID 2>/dev/null || true
# to append to check.sh before the "all checks passed" line

printf '{"users":{"next_id":1,"rows":[{"id":"1","name":"a"}]}}' > /tmp/velo-check-migrate.json
"$BIN/velo" migrate /tmp/velo-check-migrate.json users rename name label | grep -q "touches 1 of 1"
grep -q '"label":"a"' /tmp/velo-check-migrate.json
rm -f /tmp/velo-check-migrate.json /tmp/velo-check-migrate.json.lock

rm -rf /tmp/velo-check-uploads && mkdir -p /tmp/velo-check-uploads
VELO_UPLOAD_DIR=/tmp/velo-check-uploads "$BIN/velo" run examples/shop/app.velo "$ADDR" &
PID=$!
sleep 1
curl -sf --max-time 10 -XPOST "http://$ADDR/products" -d '{"id":"p1","name":"kettle","price":10,"stock":2}' >/dev/null
curl -sf --max-time 10 -XPOST "http://$ADDR/orders" -H 'x-customer: c1' -d '{"item":"p1","qty":1}' | grep -q '"stock":1'
curl -s --max-time 10 -XPOST "http://$ADDR/orders" -H 'x-customer: c1' -d '{"item":"p1","qty":9}' | grep -q '"error"'
curl -sf --max-time 10 "http://$ADDR/products/p1" | grep -q '"stock":1'
curl -sf --max-time 10 "http://$ADDR/orders" -H 'x-customer: c1' | grep -q '"name":"kettle"'
kill $PID
wait $PID 2>/dev/null || true
rm -rf /tmp/velo-check-uploads

echo "all checks passed"
