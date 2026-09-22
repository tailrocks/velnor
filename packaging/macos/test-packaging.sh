#!/bin/sh
# Static macOS packaging gate. It must not start launchd or the runner.
set -eu

root=$(CDPATH="" cd -- "$(dirname -- "$0")/../.." && pwd -P)
formula="$root/Formula/velnorctl.rb"
template="$root/Formula/velnorctl.rb.template"
launcher="$root/packaging/macos/velnor-runner-launch"
plist="$root/packaging/macos/com.tailrocks.velnor.runner.plist"
mode_contract="$root/packaging/macos/test-mode-contract.sh"
provider_fixtures="$root/packaging/macos/test-provider-discovery.sh"

test -f "$formula"
test -f "$template"
test -x "$launcher"
test -x "$mode_contract"
test -x "$provider_fixtures"
test -f "$plist"

sh -n "$launcher"
sh -n "$mode_contract"
sh -n "$provider_fixtures"
plutil -lint "$plist" >/dev/null
ruby -c "$formula" >/dev/null
ruby -c "$template" >/dev/null
sh "$mode_contract"

grep -F 'SOURCE_COMMIT = "796c46110274a474572de100b12dadc4aa50f47b"' "$formula" >/dev/null
grep -F 'SOURCE_ARCHIVE_SHA256 = "e61990d897dc714e018cfe4c1563391e8d602f3badf8cd483016814111c60b04"' "$formula" >/dev/null
grep -F 'PACKAGING_COMMIT = "8ff29a5c3dd5e9a428ddca393da2ec75d28253bd"' "$formula" >/dev/null
grep -F 'LAUNCH_SHA256 = "4a725baeec24c2905281b1debb2b493f52411ff53018b07d1c82e431adbda7ee"' "$formula" >/dev/null
grep -F 'PLIST_SHA256 = "c7d3f69454e8d04a7913dfcfa0b7c0ea36b37c6339118cff580f1d4eb31493f5"' "$formula" >/dev/null
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
grep -F 'context inspect' "$launcher" >/dev/null
grep -F -- '--host' "$launcher" >/dev/null
grep -F 'OperatingSystem' "$launcher" >/dev/null
grep -F 'cannot discover the selected Docker context' "$launcher" >/dev/null
if grep -nF 'docker context use' "$launcher"; then
  printf '%s\n' "launcher must never mutate Docker's selected context" >&2
  exit 1
fi
grep -F 'brew services stop velnorctl; edit env; brew services start velnorctl' "$launcher" >/dev/null
grep -F '<key>PATH</key>' "$plist" >/dev/null
grep -F '<key>VELNOR_PATH</key>' "$plist" >/dev/null
grep -F '<key>VELNOR_WORKER_VERIFIER</key>' "$plist" >/dev/null
grep -F '<key>VELNOR_WORKER_VERIFIER_CONTRACT</key>' "$plist" >/dev/null
grep -F 'attestation verify --help' "$plist" >/dev/null
grep -F '<key>VELNOR_MODE_STATE</key>' "$plist" >/dev/null
grep -F '__VELNOR_MODE_STATE__' "$plist" >/dev/null

provider_marker=$(printf 'orb%s' 'stack')
provider_socket_marker=$(printf '.%s%s' 'orb' 'stack')
for packaging_file in "$formula" "$template" "$launcher" "$plist"; do
  if grep -niF "$provider_marker" "$packaging_file" || \
    grep -niF "$provider_socket_marker" "$packaging_file"; then
    printf '%s\n' "packaging must not hardcode a provider context or socket" >&2
    exit 1
  fi
done
if grep -nE 'VELNOR_DOCKER_CONTEXT[[:space:]]*:|VELNOR_DOCKER_HOST[[:space:]]*:|DOCKER_HOST[[:space:]]*:|__VELNOR_DOCKER_HOST__' \
  "$formula" "$template" "$plist"; then
  printf '%s\n' "packaging must not force Docker host/context" >&2
  exit 1
fi

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

sh "$provider_fixtures"

printf '%s\n' "macOS packaging static gate: PASS"
