#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source="$root/.github-gen/sources/actions/setup-velnor-workflow/action.yml"
generated="$root/.github/actions/setup-velnor-workflow/action.yml"
failures=0

pass() {
  printf 'ok %s\n' "$1"
}

fail() {
  printf 'FAIL %s: %s\n' "$1" "$2" >&2
  failures=$((failures + 1))
}

assert_contains() {
  local file="$1"
  local needle="$2"
  local label="$3"
  if grep -Fq "$needle" "$file"; then
    pass "$label"
  else
    fail "$label" "missing: $needle"
  fi
}

assert_absent() {
  local file="$1"
  local needle="$2"
  local label="$3"
  if grep -Fq "$needle" "$file"; then
    fail "$label" "forbidden text found: $needle"
  else
    pass "$label"
  fi
}

if cmp -s "$source" "$generated"; then
  pass 'generated action matches its source'
else
  fail 'generated action matches its source' 'source and generated action differ'
fi

for action in "$source" "$generated"; do
  relative="${action#"$root/"}"
  assert_contains "$action" 'if command -v sha256sum >/dev/null 2>&1; then' "$relative keeps Linux sha256sum first"
  assert_contains "$action" 'elif command -v shasum >/dev/null 2>&1; then' "$relative supports macOS shasum"
  assert_contains "$action" 'shasum -a 256 "$1" | awk' "$relative uses SHA-256 on macOS"
  assert_contains "$action" 'no SHA-256 checksum tool available (sha256sum or shasum)' "$relative fails closed without a checksum tool"
  assert_contains "$action" 'mkdir -p "$(dirname "$destination")"' "$relative creates copy destinations portably"
  assert_contains "$action" 'cp "$source" "$destination"' "$relative copies without install -D"
  assert_contains "$action" 'chmod "$mode" "$destination"' "$relative preserves explicit file modes"
  assert_contains "$action" 'copy_with_mode' "$relative uses the portable copy helper"
  assert_absent "$action" 'install -D' "$relative has no GNU install -D"
  assert_absent "$action" 'rm -rf --' "$relative has no non-portable rm option form"
done

assert_contains "$source" '          "$temporary/runtime/velnor-workflow"' 'prebuilt binary has a portable destination'
assert_contains "$source" '          "$temporary/runtime/manifest.json"' 'prebuilt manifest has a portable destination'
assert_contains "$source" '          "$HOME/.cargo/bin/velnor-workflow"' 'PATH install has a portable destination'
assert_contains "$source" '          0755' 'binary copies retain executable mode'
assert_contains "$source" '          0644' 'manifest copies retain readable mode'

if [[ $failures -gt 0 ]]; then
  printf '%d action portability assertion(s) failed\n' "$failures" >&2
  exit 1
fi
