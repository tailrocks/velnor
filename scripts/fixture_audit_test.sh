#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE_AUDIT=(cargo run -q -p velnor-tools -- fixture-audit)

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p \
  "$tmp_dir/.github/workflows" \
  "$tmp_dir/.github/actions/aggregate-needs" \
  "$tmp_dir/.github/actions/check-fixture-output" \
  "$tmp_dir/.github/actions/check-deployed-docs"

cat >"$tmp_dir/.github/workflows/compat.yml" <<'EOF'
on:
  merge_group:
  schedule:
    - cron: '0 4 * * 0'
  workflow_dispatch:
    inputs:
      packages:
        description: 'Packages filter'
        type: string
        default: ''
concurrency:
  group: compat-${{ github.event_name }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
defaults:
  run:
    shell: bash
jobs:
  matrix-setup:
    runs-on: ubuntu-latest
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","runner":"\"ubuntu-latest\""},{"lane":"velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  compat:
    needs: [matrix-setup]
    if: github.event_name == 'schedule' || contains(inputs.packages, 'app-a') || true
    env:
      SCCACHE_GHA_ENABLED: "true"
    permissions:
      contents: read
    strategy:
      matrix:
        package: [app-a, app-b]
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: dorny/paths-filter@v4
        with:
          filters: |
            rust:
              - 'crates/**'
            tools:
              - 'justfile'
      - uses: jdx/mise-action@v4
      - uses: extractions/setup-just@v4
      - uses: rui314/setup-mold@v1
      - uses: mozilla-actions/sccache-action@v0.0.10
        continue-on-error: true
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: fixture-${{ matrix.config.lane }}-linux
      - uses: actions/cache@v5
        with:
          restore-keys: fixture-cargo-bin-${{ matrix.config.lane }}-
      - run: |
          echo "x=y" >> "$GITHUB_ENV"
          echo "x=y" >> "$GITHUB_OUTPUT"
          echo "s=complete" >> "$GITHUB_STATE"
          echo "$HOME/bin" >> "$GITHUB_PATH"
          echo "### ${{ matrix.config.lane }} ${{ matrix.package }}" >> "$GITHUB_STEP_SUMMARY"
          printf 'pkgs<<EOF\napp-a\napp-b\nEOF\n' >> "$GITHUB_OUTPUT"
      - run: sccache --show-stats >> "$GITHUB_STEP_SUMMARY"
      - run: just fmt-check
      - run: just clippy "$PACKAGE"
      - run: just test "$PACKAGE"
      - run: just nextest "$PACKAGE"
      - name: Check MSRV
        env:
          RUSTUP_TOOLCHAIN: stable
        run: cargo check -p "${PACKAGE}" --locked
      - uses: ./.github/actions/check-fixture-output
      - uses: actions/upload-artifact@v7
  compat-required:
    if: always()
    needs: [changes, compat]
    steps:
      - uses: ./.github/actions/aggregate-needs
        with:
          needs-json: ${{ toJSON(needs) }}
          workflow-label: compat
  compare-results:
    needs: [compat]
    steps:
      - uses: actions/download-artifact@v8
        with:
          merge-multiple: false
      - uses: ./.github/actions/aggregate-needs
EOF

cat >"$tmp_dir/.github/workflows/docker.yml" <<'EOF'
jobs:
  matrix-setup:
    runs-on: ubuntu-latest
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","runner":"\"ubuntu-latest\""},{"lane":"velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  docker:
    needs: [matrix-setup]
    strategy:
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: crazy-max/ghaction-github-runtime@v4
      - uses: docker/login-action@v4
      - uses: docker/metadata-action@v6
      - uses: docker/setup-buildx-action@v4
      - uses: docker/build-push-action@v7
        with:
          push: false
          load: true
          cache-from: type=gha,scope=fixture-build-${{ matrix.config.lane }}
          cache-to: type=gha,scope=fixture-build-${{ matrix.config.lane }},mode=max
      - run: docker run --rm velnor-actions-fixture:${{ matrix.config.lane }}
      - uses: docker/bake-action@v7
        with:
          set: |
            fixture.cache-from=type=gha,scope=fixture-bake-${{ matrix.config.lane }}
            fixture.cache-to=type=gha,scope=fixture-bake-${{ matrix.config.lane }},mode=max
      - run: docker buildx du >> "$GITHUB_STEP_SUMMARY"
  docker-compare:
    needs: [docker]
    steps:
      - uses: ./.github/actions/aggregate-needs
EOF

cat >"$tmp_dir/.github/workflows/pages.yml" <<'EOF'
permissions:
  pages: write
  id-token: write
jobs:
  matrix-setup:
    runs-on: ubuntu-latest
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","runner":"\"ubuntu-latest\""},{"lane":"velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  build:
    needs: [matrix-setup]
    strategy:
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/upload-pages-artifact@v5
        with:
          name: github-pages-${{ matrix.config.lane }}
      - uses: ./.github/actions/check-deployed-docs
  deploy:
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - uses: actions/deploy-pages@v5
EOF

cat >"$tmp_dir/.github/workflows/renovate.yml" <<'EOF'
jobs:
  matrix-setup:
    runs-on: ubuntu-latest
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","runner":"\"ubuntu-latest\""},{"lane":"velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  renovate:
    needs: [matrix-setup]
    strategy:
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: renovatebot/github-action@v46
        env:
          RENOVATE_TOKEN: ${{ secrets.GITHUB_TOKEN }}
EOF

printf 'runs:\n  using: composite\ninputs:\n  needs-json:\n    required: false\n  toJSON: ""\n' >"$tmp_dir/.github/actions/aggregate-needs/action.yml"
printf 'runs:\n  using: composite\n' >"$tmp_dir/.github/actions/check-fixture-output/action.yml"
printf 'runs:\n  using: composite\n' >"$tmp_dir/.github/actions/check-deployed-docs/action.yml"

cat >"$tmp_dir/.github/workflows/fixture-rust-check.yml" <<'EOF'
on:
  workflow_call:
    inputs:
      package:
        required: true
        type: string
      runner:
        required: false
        type: string
    secrets:
      github-token:
        required: false
jobs:
  rust-check:
    runs-on: ubuntu-latest
    steps:
      - run: cargo check -p "${FIXTURE_PACKAGE}"
EOF

cat >"$tmp_dir/.github/workflows/reuse-caller.yml" <<'EOF'
jobs:
  matrix-setup:
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","runner":"\"ubuntu-latest\""},{"lane":"velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  call-app-a:
    needs: [matrix-setup]
    strategy:
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    uses: ./.github/workflows/fixture-rust-check.yml
    secrets: inherit
    with:
      package: app-a
      runner: ${{ matrix.config.runner }}
  call-app-b:
    needs: [matrix-setup]
    uses: ./.github/workflows/fixture-rust-check.yml
    secrets: inherit
    with:
      package: app-b
  reuse-required:
    steps:
      - uses: ./.github/actions/aggregate-needs
        with:
          needs-json: ${{ toJSON(needs) }}
          workflow-label: reuse-caller
concurrency:
  group: reuse-caller-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
EOF

cat >"$tmp_dir/.github/workflows/schedule.yml" <<'EOF'
on:
  schedule:
    - cron: '0 4 * * 0'
  merge_group:
  workflow_dispatch:
concurrency:
  group: schedule-${{ github.event_name }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
jobs:
  matrix-setup:
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","runner":"\"ubuntu-latest\""},{"lane":"velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  scheduled-check:
    needs: [matrix-setup]
    strategy:
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - run: cargo check --workspace
  schedule-required:
    steps:
      - uses: ./.github/actions/aggregate-needs
        with:
          needs-json: ${{ toJSON(needs) }}
          workflow-label: schedule
EOF

cat >"$tmp_dir/.github/workflows/multi-arch.yml" <<'EOF'
on:
  workflow_dispatch:
concurrency:
  group: multi-arch-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
jobs:
  matrix-setup:
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        run: |
          echo 'configs=[{"lane":"github","platform":"linux/amd64","runner":"\"ubuntu-latest\""},{"lane":"velnor","platform":"linux/amd64","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}]' >> "$GITHUB_OUTPUT"
  build:
    needs: [matrix-setup]
    strategy:
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: docker/build-push-action@v7
        with:
          platforms: ${{ matrix.config.platform }}
          push: false
          outputs: type=image,push-by-digest=false,name-canonical=true
      - uses: actions/upload-artifact@v7
        with:
          name: digest-${{ matrix.config.lane }}-${{ matrix.config.platform }}
  merge-manifests:
    needs: [build]
    steps:
      - uses: actions/download-artifact@v8
        with:
          pattern: digest-*
          merge-multiple: true
      - uses: ./.github/actions/aggregate-needs
        with:
          expected: success
          actual: ${{ needs.build.result }}
EOF

cat >"$tmp_dir/mise.toml" <<'EOF'
[tools]
rust = "stable"
"cargo:cargo-nextest" = "latest"
EOF

cat >"$tmp_dir/docker-bake.hcl" <<'EOF'
group "default" {
  targets = ["fixture"]
}
target "fixture" {
  dockerfile = "Dockerfile.fixture"
}
EOF

"${FIXTURE_AUDIT[@]}" --fixture-root "$tmp_dir" >/dev/null

rm "$tmp_dir/.github/workflows/docker.yml"
if "${FIXTURE_AUDIT[@]}" --fixture-root "$tmp_dir" >/dev/null 2>&1; then
  echo "fixture audit should fail when required workflow is missing" >&2
  exit 1
fi

echo "fixture audit self-test passed"
