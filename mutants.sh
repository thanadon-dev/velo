#!/usr/bin/env sh
# Break velo on purpose, one fault at a time, and report which ones the tests catch.
# Each mutants/*.patch is a fault a person could plausibly write. A patch that
# survives is a hole in the suite, unless it changes no behaviour at all, in
# which case it belongs in the equivalent list below.
set -u
cd "$(dirname "$0")"

# Faults that change no behaviour at all: a weaker hash the map still compares
# keys behind, and a parameter count only ever read within range.
EQUIVALENT="fnv-broken nparams-not-set"

# Faults that are real and have no test yet. Empty, and meant to stay that way:
# a fault listed here is a hole someone has decided to live with.
UNCOVERED=""

if [ -n "$(git status --porcelain -- src/)" ]; then
  echo "mutants: src/ has uncommitted changes, commit or stash them first" >&2
  exit 1
fi

# Putting the tree back is the one thing this script must never get wrong: a patch
# left applied silently poisons every result after it, and the run still reads as a
# pass. So every revert is checked, and anything left behind stops the run.
restore() {
  if [ -n "$current" ]; then
    git apply -R "$current" 2>/dev/null
    current=""
  fi
  left=$(git status --porcelain -- src/)
  if [ -n "$left" ]; then
    echo "mutants: src/ is not back the way it was found, every result after this is worthless" >&2
    echo "$left" >&2
    return 1
  fi
  return 0
}

current=""
trap 'restore >/dev/null 2>&1 || true' EXIT INT TERM

only=${1:-}
caught=0
survived=0
skipped=0
broken=0

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
  if ! cargo build --profile mutants --tests >/dev/null 2>&1; then
    echo "BROKEN   $name (does not compile, proves nothing)"
    broken=$((broken + 1))
    if ! restore; then
      exit 2
    fi
    continue
  fi
  if ! cargo test --profile mutants >/dev/null 2>&1; then
    echo "caught   $name"
    caught=$((caught + 1))
  else
    case " $EQUIVALENT " in
      *" $name "*) echo "same     $name (changes no behaviour)" ;;
      *)
        case " $UNCOVERED " in
          *" $name "*) echo "open     $name (a real fault with no test yet)" ;;
          *)
            echo "SURVIVED $name"
            survived=$((survived + 1))
            ;;
        esac
        ;;
    esac
  fi
  if ! restore; then
    echo "mutants: stopped after $name" >&2
    exit 2
  fi
done

echo "$caught caught, $survived survived, $broken broken, $skipped skipped"
[ "$survived" -eq 0 ] || exit 1
