#!/usr/bin/env bash
# Checks .github/real-thread-tests.txt against the tests that exist (docs/agents/ci.md).
#
#   unmatched  every line matches at least one test of the client binary. A line that matches none
#              is a test that was renamed or removed: its line is stale and guards nothing.
#   outside    no line matches a test outside the client binary. The gating jobs skip a line in the
#              whole workspace but the real-thread job runs it in the client binary only, so a test
#              of another crate that a line matches would run nowhere.
#   agree      the nextest filterset made from the list (real-thread-filterset.sh), which the gating
#              jobs run as `not (...)`, selects the same tests as the libtest filters, which the
#              real-thread job runs, and nextest sees the same tests in the workspace as libtest
#              does. Counts of what each side runs are printed, per crate.
#   all        all three (the default).
#
# Usage: check-real-thread-list.sh [unmatched|outside|agree|all] [list-file]
# Run it where the workspace builds: it lists the tests with `cargo test -- --list`, and with
# `cargo nextest list` for `agree` (which needs cargo-nextest and jq). Doctests are left out of both.
set -euo pipefail

mode=${1:-all}
list=${2:-.github/real-thread-tests.txt}
case "$mode" in unmatched | outside | agree | all) ;; *)
  echo "usage: $0 [unmatched|outside|agree|all] [list-file]" >&2
  exit 2
  ;;
esac

mapfile -t filters < <(tr -d '\r' <"$list" | grep -vE '^\s*(#|$)')
if [ "${#filters[@]}" -eq 0 ]; then
  echo "::error::$list has no test filters"
  exit 1
fi

# libtest prints `name: test` for a test (an ignored one too), on stdout; what it says of the
# binaries it runs goes to stderr. A doctest is listed too (`src/lib.rs - item (line 3): test`),
# which nextest never runs or lists and the real-thread list is not about, so it is left out.
test_names() { sed -n 's/: test$//p' | grep -vE ' - .*\(line [0-9]+\)' || true; }
client=$(cargo test -p client --bin client -- --list 2>/dev/null | test_names)
workspace=$(cargo test --workspace -- --list 2>/dev/null | test_names)

# How many of the names in $2 contain the filter $1, which libtest matches as a substring.
matches() { grep -cF -- "$1" <<<"$2" || true; }

failed=0
for filter in "${filters[@]}"; do
  in_client=$(matches "$filter" "$client")
  in_workspace=$(matches "$filter" "$workspace")
  if [[ $mode != outside && $mode != agree && $in_client -eq 0 ]]; then
    echo "::error file=$list::'$filter' matches no test of the client binary (a test that was renamed or removed?)"
    failed=1
  fi
  if [[ $mode != unmatched && $mode != agree && $in_workspace -gt $in_client ]]; then
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

if [[ $mode == agree || $mode == all ]]; then
  here=$(dirname "${BASH_SOURCE[0]}")
  filterset=$("$here/real-thread-filterset.sh" "$list")
  # `--run-ignored all` makes nextest list the `#[ignore]`d tests too, as libtest's `--list` does,
  # and judge each against the filterset. The names are `<binary id> <test name>`.
  listing() {
    # nextest's own messages (a compile error, a bad filterset) go to stderr, which stays in the log.
    cargo nextest list --workspace --run-ignored all --message-format json -E "$1" |
      jq -r '."rust-suites" | to_entries[] | .key as $b | .value.testcases | to_entries[] |
             select(.value."filter-match".status == "matches") | "\($b) \(.key)"'
  }
  all_nextest=$(listing 'all()')
  real_nextest=$(listing "$filterset")
  gate_nextest=$(listing "not ($filterset)")
  n_all=$(grep -c . <<<"$all_nextest" || true)
  n_real=$(grep -c . <<<"$real_nextest" || true)
  n_gate=$(grep -c . <<<"$gate_nextest" || true)
  echo "nextest: $n_all tests in the workspace: $n_gate gate, $n_real run in the real-thread job"
  if [[ $n_all -ne $total || $n_real -ne $real || $n_gate -ne $((total - real)) ]]; then
    echo "::error file=$list::nextest and libtest disagree on the tests ($n_all/$n_gate/$n_real against $total/$((total - real))/$real: all/gate/real-thread)"
    failed=1
  fi
  # The real-thread job runs the client binary only: nextest's selection must be inside it, and be
  # the very tests libtest's filters select there.
  if grep -v '^client::bin/client ' <<<"$real_nextest" | grep -q .; then
    echo "::error file=$list::the filterset selects a test outside the client binary:"
    grep -v '^client::bin/client ' <<<"$real_nextest" >&2
    failed=1
  fi
  libtest_real=$(grep -F -- "$(tr -d '\r' <"$list" | grep -vE '^\s*(#|$)')" <<<"$client" | sort || true)
  nextest_real=$(sed -n 's/^client::bin\/client //p' <<<"$real_nextest" | sort)
  if [[ $libtest_real != "$nextest_real" ]]; then
    echo "::error file=$list::the filterset and the libtest filters select different tests of the client binary:"
    diff <(echo "$libtest_real") <(echo "$nextest_real") >&2 || true
    failed=1
  fi
  # How many tests of each crate gate (the crate is what comes before `::` or ` ` of the binary id).
  echo "gating tests per crate:"
  sed 's/[: ].*//' <<<"$gate_nextest" | sort | uniq -c | sed 's/^/  /'
fi

exit "$failed"
