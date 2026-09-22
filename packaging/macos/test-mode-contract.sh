#!/bin/sh
# Static mode-contract gate. It never starts launchd, Docker, gh, or Velnor.
set -eu

root=$(CDPATH="" cd -- "$(dirname -- "$0")/../.." && pwd -P)
formula="$root/Formula/velnorctl.rb"
template="$root/Formula/velnorctl.rb.template"
launcher="$root/packaging/macos/velnor-runner-launch"
plist="$root/packaging/macos/com.tailrocks.velnor.runner.plist"

test -f "$formula"
test -f "$template"
test -x "$launcher"
test -f "$plist"

sh -n "$launcher"
plutil -lint "$plist" >/dev/null
ruby -c "$formula" >/dev/null
ruby -c "$template" >/dev/null

for file in "$formula" "$template"; do
  grep -F 'HOST_MODES = %w[native-only scale-set-only both]' "$file" >/dev/null
  grep -F 'SCALE_HOST_MODES = %w[scale-set-only both]' "$file" >/dev/null
  grep -F 'MACOS_QUALIFICATION_ORDER = %w[scale-set-only native-only both]' "$file" >/dev/null
  grep -F 'VELNOR_HOST_MODE=' "$file" >/dev/null
  grep -F '"requested"' "$file" >/dev/null
  grep -F '"effective"' "$file" >/dev/null
  if grep -nE 'VELNOR_HOST_MODE[[:space:]]*=[[:space:]]*native-only|VELNOR_HOST_MODE:[[:space:]]*"' "$file"; then
    printf '%s\n' "formula silently selects native-only" >&2
    exit 1
  fi
done

if grep -nE '<key>VELNOR_HOST_MODE</key>|<string>native-only</string>' "$plist"; then
  printf '%s\n' "launchd plist silently selects native-only" >&2
  exit 1
fi

grep -F 'native-only|scale-set-only|both)' "$launcher" >/dev/null
grep -F 'scale-set mode requires VELNOR_SCALE_SET_CONFIG' "$launcher" >/dev/null
grep -F 'require_positive_integer VELNOR_MAX_JOBS' "$launcher" >/dev/null
grep -F 'scale-set config ledger_path must equal package permit ledger' "$launcher" >/dev/null
grep -F 'admission_key in owner repository ref source workflow event' "$launcher" >/dev/null
grep -F 'requires exactly one [auth.app] or [auth.pat]' "$launcher" >/dev/null
grep -F 'native-only mode forbids VELNOR_SCALE_SET_CONFIG' "$launcher" >/dev/null
grep -F 'official gh CLI lacks worker verifier flag' "$launcher" >/dev/null
grep -F 'Docker is required for Velnor container execution' "$launcher" >/dev/null
grep -F 'brew services stop velnorctl; edit env; brew services start velnorctl' "$launcher" >/dev/null

grep -F '<key>VELNOR_MODE_STATE</key>' "$plist" >/dev/null
grep -F '__VELNOR_MODE_STATE__' "$plist" >/dev/null
grep -F 'VELNOR_MODE_STATE' "$formula" >/dev/null
grep -F 'VELNOR_MODE_STATE' "$template" >/dev/null

printf '%s\n' "macOS mode-contract static gate: PASS"
