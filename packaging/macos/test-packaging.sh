#!/bin/sh
# Static macOS packaging gate. It must not start launchd or the runner.
set -eu

root=$(CDPATH="" cd -- "$(dirname -- "$0")/../.." && pwd -P)
formula="$root/Formula/velnorctl.rb"
template="$root/Formula/velnorctl.rb.template"
launcher="$root/packaging/macos/velnor-runner-launch"
plist="$root/packaging/macos/com.tailrocks.velnor.runner.plist"
mode_contract="$root/packaging/macos/test-mode-contract.sh"

test -f "$formula"
test -f "$template"
test -x "$launcher"
test -x "$mode_contract"
test -f "$plist"

sh -n "$launcher"
sh -n "$mode_contract"
plutil -lint "$plist" >/dev/null
ruby -c "$formula" >/dev/null
ruby -c "$template" >/dev/null
sh "$mode_contract"

grep -F 'SOURCE_COMMIT = "c76d2b932dc1a4f12eee3650f690d50b13816d48"' "$formula" >/dev/null
grep -F 'version "0.1.277"' "$formula" >/dev/null
grep -F 'sha256 SOURCE_ARCHIVE_SHA256' "$formula" >/dev/null
grep -F 'depends_on "gh"' "$formula" >/dev/null
grep -F 'depends_on "gh"' "$template" >/dev/null
grep -F 'GH_CLI_REQUIRED_FLAGS' "$formula" >/dev/null
grep -F 'GH_CLI_REQUIRED_FLAGS' "$template" >/dev/null
grep -F 'HOST_MODES = %w[native-only scale-set-only both]' "$formula" >/dev/null
grep -F 'HOST_MODES = %w[native-only scale-set-only both]' "$template" >/dev/null
grep -F 'SCALE_HOST_MODES = %w[scale-set-only both]' "$formula" >/dev/null
grep -F 'SCALE_HOST_MODES = %w[scale-set-only both]' "$template" >/dev/null
grep -F 'runtime_dependencies' "$formula" >/dev/null
grep -F 'runtime_dependencies' "$template" >/dev/null
grep -F 'worker_verifier' "$formula" >/dev/null
grep -F 'worker_verifier' "$template" >/dev/null
grep -F 'VELNOR_HOST_MODE=' "$formula" >/dev/null
grep -F 'VELNOR_HOST_MODE=' "$template" >/dev/null
grep -F 'requested' "$formula" >/dev/null
grep -F 'effective' "$formula" >/dev/null
grep -F 'requested' "$template" >/dev/null
grep -F 'effective' "$template" >/dev/null
if grep -nE '<key>VELNOR_HOST_MODE</key>|<string>native-only</string>' "$plist"; then
  printf '%s\n' "launchd plist must not select or force a host mode" >&2
  exit 1
fi
grep -F 'VELNOR_ENV_FILE' "$launcher" >/dev/null
grep -F 'VELNOR_STATE_DB' "$launcher" >/dev/null
grep -F 'VELNOR_PERMIT_LEDGER' "$launcher" >/dev/null
grep -F 'VELNOR_PATH' "$launcher" >/dev/null
grep -F 'VELNOR_WORKER_VERIFIER' "$launcher" >/dev/null
grep -F 'attestation verify --help' "$launcher" >/dev/null
grep -F -- '--deny-self-hosted-runners' "$launcher" >/dev/null
grep -F 'native-only mode forbids VELNOR_SCALE_SET_CONFIG' "$launcher" >/dev/null
grep -F 'VELNOR_HOST_MODE is required' "$launcher" >/dev/null
grep -F 'scale-set mode requires VELNOR_SCALE_SET_CONFIG' "$launcher" >/dev/null
grep -F 'scale-set config ledger_path must equal package permit ledger' "$launcher" >/dev/null
grep -F 'admission_key in owner repository ref source workflow event' "$launcher" >/dev/null
grep -F 'requires exactly one [auth.app] or [auth.pat]' "$launcher" >/dev/null
grep -F 'require_positive_integer VELNOR_MAX_JOBS' "$launcher" >/dev/null
grep -F 'Docker is required' "$launcher" >/dev/null
grep -F 'brew services stop velnorctl; edit env; brew services start velnorctl' "$launcher" >/dev/null
grep -F '<key>PATH</key>' "$plist" >/dev/null
grep -F '<key>VELNOR_PATH</key>' "$plist" >/dev/null
grep -F '<key>VELNOR_WORKER_VERIFIER</key>' "$plist" >/dev/null
grep -F '<key>VELNOR_WORKER_VERIFIER_CONTRACT</key>' "$plist" >/dev/null
grep -F 'attestation verify --help' "$plist" >/dev/null
grep -F '<key>VELNOR_MODE_STATE</key>' "$plist" >/dev/null
grep -F '__VELNOR_MODE_STATE__' "$plist" >/dev/null

for binary in velnorctl velnor-runner velnor-workflow; do
  grep -E "\"$binary\"[[:space:]]+=>[[:space:]]+\"crates/$binary\"" "$formula" >/dev/null
  grep -E "\"$binary\"[[:space:]]+=>[[:space:]]+\"crates/$binary\"" "$template" >/dev/null
done

if grep -nE 'disable!|refs/heads|/releases/latest|runner-only' "$formula" "$template" "$launcher" "$plist"; then
  printf '%s\n' "forbidden floating/disabled/runner-only packaging marker" >&2
  exit 1
fi

if grep -nE 'pending-packaging-commit|pending-launch-sha256|pending-plist-sha256' "$formula"; then
  printf '%s\n' "formula has unresolved packaging pins" >&2
  exit 1
fi

if grep -nE '__VELNOR_[A-Z_]+__' "$launcher"; then
  printf '%s\n' "launcher contains unresolved plist placeholders" >&2
  exit 1
fi

printf '%s\n' "macOS packaging static gate: PASS"
