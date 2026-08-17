#!/usr/bin/env sh
# Break velo on purpose, one fault at a time, and report which ones the tests catch.
# Each mutants/*.patch is a fault a person could plausibly write. A patch that
# survives is a hole in the suite, unless it changes no behaviour at all, in
# which case it belongs in the equivalent list below.
set -u
cd "$(dirname "$0")"

EQUIVALENT="fnv-broken nparams-not-set no-backpressure"

if [ -n "$(git status --porcelain -- src/)" ]; then
  echo "mutants: src/ has uncommitted changes, commit or stash them first" >&2
  exit 1
fi

current=""
trap 'if [ -n "$current" ]; then git apply -R "$current" 2>/dev/null; fi' EXIT INT TERM

only=${1:-}
caught=0
survived=0
skipped=0

for patch in mutants/*.patch; do
  name=$(basename "$patch" .patch)
  case "$only" in "" ) ;; *) [ "$name" = "$only" ] || continue ;; esac
  current="$patch"
  if ! git apply "$patch" 2>/dev/null; then
    current=""
    echo "SKIPPED  $name (the code it changes has moved)"
    skipped=$((skipped + 1))
    continue
  fi
  if cargo test --release 2>&1 | grep -qE "^test result: FAILED|^error"; then
    echo "caught   $name"
    caught=$((caught + 1))
  else
    case " $EQUIVALENT " in
      *" $name "*) echo "same     $name (changes no behaviour)" ;;
      *)
        echo "SURVIVED $name"
        survived=$((survived + 1))
        ;;
    esac
  fi
  git apply -R "$patch"
  current=""
done

echo "$caught caught, $survived survived, $skipped skipped"
[ "$survived" -eq 0 ] || exit 1
