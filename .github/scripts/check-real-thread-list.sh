#!/usr/bin/env bash
# Checks .github/real-thread-tests.txt against the tests that exist (docs/agents/ci.md).
#
#   unmatched  every line matches at least one test of the client binary. A line that matches none
#              is a test that was renamed or removed: its line is stale and guards nothing.
#   outside    no line matches a test outside the client binary. The gating jobs skip a line in the
#              whole workspace but the real-thread job runs it in the client binary only, so a test
#              of another crate that a line matches would run nowhere.
#   all        both (the default).
#
# Usage: check-real-thread-list.sh [unmatched|outside|all] [list-file]
# Run it where the workspace builds: it lists the tests with `cargo test -- --list`.
set -euo pipefail

mode=${1:-all}
list=${2:-.github/real-thread-tests.txt}
case "$mode" in unmatched | outside | all) ;; *)
  echo "usage: $0 [unmatched|outside|all] [list-file]" >&2
  exit 2
  ;;
esac

mapfile -t filters < <(tr -d '\r' <"$list" | grep -vE '^\s*(#|$)')
if [ "${#filters[@]}" -eq 0 ]; then
  echo "::error::$list has no test filters"
  exit 1
fi

# libtest prints `name: test` for a test (an ignored one too), on stdout; what it says of the
# binaries it runs goes to stderr.
test_names() { sed -n 's/: test$//p'; }
client=$(cargo test -p client --bin client -- --list 2>/dev/null | test_names)
workspace=$(cargo test --workspace -- --list 2>/dev/null | test_names)

# How many of the names in $2 contain the filter $1, which libtest matches as a substring.
matches() { grep -cF -- "$1" <<<"$2" || true; }

failed=0
for filter in "${filters[@]}"; do
  in_client=$(matches "$filter" "$client")
  in_workspace=$(matches "$filter" "$workspace")
  if [[ $mode != outside && $in_client -eq 0 ]]; then
    echo "::error file=$list::'$filter' matches no test of the client binary (a test that was renamed or removed?)"
    failed=1
  fi
  if [[ $mode != unmatched && $in_workspace -gt $in_client ]]; then
    echo "::error file=$list::'$filter' matches $((in_workspace - in_client)) test(s) outside the client binary, which would run in no job"
    failed=1
  fi
done

# Every test runs in one place: the real-thread job runs the client's that a line matches, and the
# gating jobs run all the rest.
printf '%s\n' "${filters[@]}" >"${TMPDIR:-/tmp}/real-thread-filters.$$"
real=$(grep -cFf "${TMPDIR:-/tmp}/real-thread-filters.$$" <<<"$client" || true)
rm -f "${TMPDIR:-/tmp}/real-thread-filters.$$"
total=$(grep -c . <<<"$workspace" || true)
echo "$total tests in the workspace: $((total - real)) gate, $real run in the real-thread job"

exit "$failed"
