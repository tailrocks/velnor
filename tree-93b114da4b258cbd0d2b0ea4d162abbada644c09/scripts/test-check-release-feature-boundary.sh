#!/usr/bin/env bash
# Self-test for scripts/check-release-feature-boundary.sh.
#
# Drives the boundary check against a fake `cargo` and proves:
#   - a successful release build of the test feature fails the check,
#   - an unrelated failure (missing dependency, broken toolchain) fails the
#     check instead of passing it,
#   - only the expected compiler guard passes the check,
#   - the guard is recognized among unrelated output noise.
# Also pins the mise.toml wiring and the build recipe so drift breaks CI.
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
script="$root/scripts/check-release-feature-boundary.sh"
mise_config="$root/mise.toml"
expected="test-support is forbidden in release-profile builds"
unrelated="error: no matching package named \`boundary-fake-dep\` found"
failures=0

pass() {
  echo "ok $1"
}

fail() {
  echo "FAIL $1: $2" >&2
  failures=$((failures + 1))
}

run_scenario() {
  local name="$1" fake_output="$2" fake_status="$3" want_success="$4" want_message="$5"
  local dir out err rc
  dir="$(mktemp -d)"
  {
    echo '#!/usr/bin/env bash'
    echo "cat <<'BOUNDARY_FAKE_CARGO_OUTPUT'"
    echo "$fake_output"
    echo 'BOUNDARY_FAKE_CARGO_OUTPUT'
    echo "exit $fake_status"
  } >"$dir/cargo"
  chmod +x "$dir/cargo"
  out="$(PATH="$dir:$PATH" bash "$script" 2>"$dir/stderr")" && rc=0 || rc=$?
  err="$(cat "$dir/stderr")"
  rm -rf "$dir"
  if [[ "$want_success" == yes && $rc -ne 0 ]]; then
    fail "$name" "expected success, got exit $rc; stderr: $err"
    return
  fi
  if [[ "$want_success" == no && $rc -eq 0 ]]; then
    fail "$name" "expected nonzero exit, got success; stderr: $err"
    return
  fi
  if [[ -n "$want_message" ]]; then
    if [[ "$want_success" == yes ]]; then
      [[ "$out" == *"$want_message"* ]] || {
        fail "$name" "expected stdout to contain \"$want_message\"; stdout: $out"
        return
      }
    else
      [[ "$err" == *"$want_message"* ]] || {
        fail "$name" "expected stderr to contain \"$want_message\"; stderr: $err"
        return
      }
    fi
  fi
  pass "$name"
}

run_scenario \
  "boundary violation fails the check" \
  "    Checking velnor-runner v0.1.0" \
  0 no "boundary violation: test-support compiled in a release profile"

run_scenario \
  "missing dependency fails the check instead of passing it" \
  "$unrelated" \
  101 no "release boundary check failed for an unrelated reason"

run_scenario \
  "expected compiler guard passes the check" \
  "error: $expected
 --> crates/velnor-runner/src/lib.rs:12:3" \
  101 yes "release boundary guard fired: $expected"

run_scenario \
  "guard is matched among unrelated noise" \
  "warning: unused import: \`std::fmt\`
error: could not compile \`boundary-noise-dep\` due to a broken linker
cargo was terminated by signal 9
error: $expected
 --> crates/velnor-runner/src/lib.rs:12:3" \
  101 yes ""

# The unrelated-failure case must surface the underlying error, not swallow it.
noise_dir="$(mktemp -d)"
printf '%s\n' '#!/usr/bin/env bash' "cat <<'BOUNDARY_FAKE_CARGO_OUTPUT'" "$unrelated" 'BOUNDARY_FAKE_CARGO_OUTPUT' 'exit 101' >"$noise_dir/cargo"
chmod +x "$noise_dir/cargo"
err="$(PATH="$noise_dir:$PATH" bash "$script" 2>&1 >/dev/null || true)"
rm -rf "$noise_dir"
if [[ "$err" == *"$unrelated"* ]]; then
  pass "underlying failure is surfaced on stderr"
else
  fail "underlying failure is surfaced on stderr" "stderr did not contain the dependency error: $err"
fi

mise_body="$(sed -n '/^\[tasks\.test-release-feature-boundary\]/,/^\[/p' "$mise_config")"
if [[ "$mise_body" == *'bash scripts/check-release-feature-boundary.sh'* ]]; then
  pass "wiring pins: mise task invokes the boundary script"
else
  fail "wiring pins: mise task invokes the boundary script" "mise.toml task body: $mise_body"
fi
for needle in "$expected" 'failed for an unrelated reason' '--release --locked --features test-support'; do
  if [[ "$(cat "$script")" == *"$needle"* ]]; then
    pass "wiring pins: script contains $needle"
  else
    fail "wiring pins: script contains $needle" "boundary script drifted"
  fi
done

if [[ $failures -gt 0 ]]; then
  echo "$failures scenario(s) failed" >&2
  exit 1
fi
