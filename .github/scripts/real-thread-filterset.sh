#!/usr/bin/env bash
# Prints .github/real-thread-tests.txt as a cargo-nextest filterset that matches the tests it lists
# (docs/agents/ci.md). Each line is a substring of the test's name, as libtest's filter is, so
# `test(~line)`. The gating jobs run `-E "not ($(this script))"`, and the real-thread job runs
# `cargo test -p client --bin client -- <lines>`: libtest and nextest read a line the same way, and
# check-real-thread-list.sh holds the two to the same set of tests.
#
# Usage: real-thread-filterset.sh [list-file]
set -euo pipefail

list=${1:-.github/real-thread-tests.txt}
mapfile -t filters < <(tr -d '\r' <"$list" | grep -vE '^\s*(#|$)')
if [ "${#filters[@]}" -eq 0 ]; then
  echo "::error::$list has no test filters" >&2
  exit 1
fi

expr=""
for f in "${filters[@]}"; do
  # Unquoted, because nextest takes `~"..."` as no match at all (as of the pinned nextest). Anything outside this set
  # could be read as filterset syntax, so a line with one is refused.
  if [[ ! $f =~ ^[A-Za-z0-9_:.]+$ ]]; then
    echo "::error::'$f' has a character that is not a letter, a digit, _, : or ., which the filterset cannot take unquoted" >&2
    exit 1
  fi
  expr+="${expr:+ | }test(~$f)"
done
printf '%s\n' "$expr"
