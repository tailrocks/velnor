#!/usr/bin/env bash
# Self-test for scripts/check-release-feature-boundary.sh.
#
# Drives the boundary check against fake `cargo` and `mbx` binaries and proves:
#   - a successful release build of the test feature fails the check,
#   - an unrelated failure (missing dependency, broken toolchain) fails the
#     check instead of passing it,
#   - only the expected compiler guard passes the check,
#   - the guard is recognized among unrelated output noise,
#   - Mr. Boxington is used when present, with the exact release-check
#     arguments, and plain cargo is the fallback.
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

make_fake() {
  local dir="$1" name="$2" output="$3" status="$4"
  {
    echo '#!/usr/bin/env bash'
    echo 'dir="$(dirname -- "$0")"'
    echo 'printf "%s\n" "$(basename -- "$0")" >"$dir/binary"'
    echo 'printf "%s\n" "$@" >"$dir/args"'
    echo "cat <<'BOUNDARY_FAKE_OUTPUT'"
    echo "$output"
    echo 'BOUNDARY_FAKE_OUTPUT'
    echo "exit $status"
  } >"$dir/$name"
  chmod +x "$dir/$name"
}

# run_scenario <name> <tool> <output> <status> <want_success> <want_message>
# <tool> is the fake binary on PATH ("cargo" or "mbx"). A cargo scenario strips
# the host PATH so the fallback route cannot be shadowed by a real mbx.
run_scenario() {
  local name="$1" tool="$2" fake_output="$3" fake_status="$4" want_success="$5" want_message="$6"
  local dir out err rc scenario_path
  dir="$(mktemp -d)"
  case "$tool" in
    cargo)
      make_fake "$dir" cargo "$fake_output" "$fake_status"
      scenario_path="$dir:/usr/bin:/bin"
      ;;
    mbx)
      make_fake "$dir" mbx "$fake_output" "$fake_status"
      scenario_path="$dir:$PATH"
      ;;
  esac
  out="$(PATH="$scenario_path" bash "$script" 2>"$dir/stderr")" && rc=0 || rc=$?
  err="$(cat "$dir/stderr")"
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
  rm -rf "$dir"
  pass "$name"
}

# check_args <name> <tool> <want_binary> <want_args>: prove which binary the
# check invoked and with which arguments. The route itself is pinned, not just
# the argument vector a shadowing host tool could have produced.
check_args() {
  local name="$1" tool="$2" want_binary="$3" want_args="$4"
  local dir args binary scenario_path
  dir="$(mktemp -d)"
  case "$tool" in
    cargo)
      make_fake "$dir" cargo "" 101
      scenario_path="$dir:/usr/bin:/bin"
      ;;
    mbx)
      make_fake "$dir" mbx "" 101
      scenario_path="$dir:$PATH"
      ;;
  esac
  PATH="$scenario_path" bash "$script" >/dev/null 2>&1 || true
  args="$(tr '\n' ' ' <"$dir/args")"
  args="${args% }"
  binary="$(cat "$dir/binary")"
  rm -rf "$dir"
  if [[ "$binary" != "$want_binary" ]]; then
    fail "$name" "expected the check to invoke \"$want_binary\", got \"$binary\""
    return
  fi
  if [[ "$args" == "$want_args" ]]; then
    pass "$name"
  else
    fail "$name" "expected arguments \"$want_args\", got \"$args\""
  fi
}

# Fallback: with no mbx anywhere on PATH the check compiles with plain cargo.
check_args \
  "without mbx the check runs plain cargo with the release-check arguments" \
  cargo \
  cargo \
  "check -p velnor-runner --release --locked --features test-support"

run_scenario \
  "boundary violation fails the check" \
  cargo \
  "    Checking velnor-runner v0.1.0" \
  0 no "boundary violation: test-support compiled in a release profile"

run_scenario \
  "missing dependency fails the check instead of passing it" \
  cargo \
  "$unrelated" \
  101 no "release boundary check failed for an unrelated reason"

run_scenario \
  "expected compiler guard passes the check" \
  cargo \
  "error: $expected
 --> crates/velnor-runner/src/lib.rs:12:3" \
  101 yes "release boundary guard fired: $expected"

run_scenario \
  "guard is matched among unrelated noise" \
  cargo \
  "warning: unused import: \`std::fmt\`
error: could not compile \`boundary-noise-dep\` due to a broken linker
cargo was terminated by signal 9
error: $expected
 --> crates/velnor-runner/src/lib.rs:12:3" \
  101 yes ""

# With Mr. Boxington first on PATH the check routes through it.
check_args \
  "with mbx the check routes through Mr. Boxington with the release-check arguments" \
  mbx \
  mbx \
  "check -p velnor-runner --release --locked --features test-support"

run_scenario \
  "mbx boundary violation fails the check" \
  mbx \
  "    Checking velnor-runner v0.1.0" \
  0 no "boundary violation: test-support compiled in a release profile"

run_scenario \
  "mbx unrelated failure fails the check instead of passing it" \
  mbx \
  "$unrelated" \
  101 no "release boundary check failed for an unrelated reason"

run_scenario \
  "mbx expected compiler guard passes the check" \
  mbx \
  "error: $expected
 --> crates/velnor-runner/src/lib.rs:12:3" \
  101 yes "release boundary guard fired: $expected"

# The unrelated-failure case must surface the underlying error, not swallow it.
noise_dir="$(mktemp -d)"
make_fake "$noise_dir" cargo "$unrelated" 101
err="$(PATH="$noise_dir:/usr/bin:/bin" bash "$script" 2>&1 >/dev/null || true)"
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
