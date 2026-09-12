#!/usr/bin/env bash
# Prove release-profile builds reject the loopback-only test feature.
#
# The build is expected to fail with the exact compiler guard from the runner
# crate. Any other failure (missing dependency, broken toolchain, missing
# linker) is infrastructure breakage and must fail this check instead of
# passing it.
set -euo pipefail

expected="test-support is forbidden in release-profile builds"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT

# Route through Mr. Boxington when it is present: the boundary must hold on the
# same compiler-cache path the unit's verification commands use.
if command -v mbx >/dev/null 2>&1; then
  build=(mbx check -p velnor-runner --release --locked --features test-support)
else
  build=(cargo check -p velnor-runner --release --locked --features test-support)
fi

if "${build[@]}" 2>&1 | tee "$log"; then
  echo "boundary violation: test-support compiled in a release profile" >&2
  exit 1
fi

if ! grep -qF "$expected" "$log"; then
  echo "release boundary check failed for an unrelated reason; refusing to count it as the expected guard" >&2
  cat "$log" >&2
  exit 1
fi

echo "release boundary guard fired: $expected"
