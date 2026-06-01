#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/fixture_audit.py"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p \
  "$tmp_dir/.github/workflows" \
  "$tmp_dir/.github/actions/aggregate-needs" \
  "$tmp_dir/.github/actions/check-fixture-output"

cat >"$tmp_dir/.github/workflows/compat.yml" <<'EOF'
jobs:
  compat-github:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        package: [app-a, app-b]
    steps:
      - uses: dorny/paths-filter@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: extractions/setup-just@v4
      - uses: actions/cache@v5
      - run: echo "x=y" >> "$GITHUB_ENV"; echo "x=y" >> "$GITHUB_OUTPUT"; echo "$HOME/bin" >> "$GITHUB_PATH"; echo hi >> "$GITHUB_STEP_SUMMARY"
      - uses: ./.github/actions/check-fixture-output
      - uses: actions/upload-artifact@v7
  compat-velnor:
    runs-on: [self-hosted, velnor-target-mvp]
  compare-results:
    needs: [compat-github, compat-velnor]
    if: needs.compat-velnor.result == 'success'
    steps:
      - uses: actions/download-artifact@v8
      - uses: ./.github/actions/aggregate-needs
EOF

cat >"$tmp_dir/.github/workflows/docker.yml" <<'EOF'
jobs:
  docker-github:
    runs-on: ubuntu-latest
    steps:
      - uses: docker/setup-buildx-action@v4
      - uses: docker/build-push-action@v7
        with:
          push: false
          load: true
      - run: docker run --rm image
  docker-velnor:
    runs-on: [self-hosted, velnor-target-mvp]
    steps:
      - uses: docker/setup-buildx-action@v4
EOF

printf 'runs:\n  using: composite\n' >"$tmp_dir/.github/actions/aggregate-needs/action.yml"
printf 'runs:\n  using: composite\n' >"$tmp_dir/.github/actions/check-fixture-output/action.yml"

"$SCRIPT" --fixture-root "$tmp_dir" >/dev/null

rm "$tmp_dir/.github/workflows/docker.yml"
if "$SCRIPT" --fixture-root "$tmp_dir" >/dev/null 2>&1; then
  echo "fixture audit should fail when required workflow is missing" >&2
  exit 1
fi

echo "fixture audit self-test passed"
