# Jackin migration execution draft (scratch; ground truth in ledger + memos)

Source pins: Jackin main `0be3fcf9`, Velnor base `33688938`, PR #994 OPEN
head `ab5b0c4` / base `92f347ac` (63 files, 27 `.github-gen/sources/**`).
PR-only bodies fetched to `/tmp/jd-*.yml`. Disposition: CLOSE #994 UNMERGED
after migration green (retires all 27 copies at once; never pin 541d8926).

## Commit 1: pin + FFI edge + Q4 capabilities (first, independent)

Precondition: Velnor slices committed to velnor main; new verified rev `<PIN>`
(all gates green at `<PIN>`; no-disturbance re-render proven).

```toml
[generator]
revision = "<PIN>"   # was b9c3156c

[[units]]  # swift-package-native row: add
capabilities = ["xcframework"]
depends_on = ["rust-jackin-usage-ffi"]

[[units]]  # scanned prototypes unit row (id confirmed in main project.toml:583)
id = "swift-package-native-design-prototypes-unifiedagentusage"
capabilities = ["xcode"]
```

Verify: regen → `ci-unit-swift.yml` macos-26 (L1 fix); FFI touch selects
swift-package-native (F2); byte-stable regen; policy green.

## Commit 2: declarations + regen (the big adopt)

### check_profiles (G + G-followup)

- desktop: `schedule = "41 4 * * 1"`, runner macos (PR copy: macos-26 both
  jobs, confirmed), tasks `desktop-merge`, `desktop-scheduled` (exist in main).
  File ALSO needs push-main trigger → `events = ["push"]` on its declare row.
- hygiene: 16 jobs → profiles, `schedule = "23 11 * * *"`:
  cache-usage, scheduled-hygiene, native-macos, bench-run,
  dhat-allocation, cold-start-bench, rust-analyzer-clean,
  build-time-measure, beta-clippy-canary, coverage, miri, mutants,
  hakari-timing, dylint-advisory, dind-chaos, health-trend.
  REQUIRES FIRST: extract 22 inline `run: |` shells → named mise tasks
  (main has zero fuzz/miri/mutants/dhat/deny/hakari tasks today).
  Statuses: advisory vs required per copy (`continue-on-error` survey at
  migration). native-macos → runner macos; dind-chaos → needs DinD notes.
- reuse: cron-less evented profile — NO `schedule`, declare row
  `events = ["push", "pull_request"]`, task `reuse-lint` (new tiny task:
  `reuse lint`). Requires G-config follow-through (schedule-less rows pass
  config validation).
- All scheduled files: `lanes_input = true` once Q6 lands (PR copies all
  carry the lanes dispatch input).

### docs (F; Q3 resolved from jd-docs.yml)

```toml
[docs]
enabled = true
site_url = "https://jackin.tailrocks.com"
site_dir = "docs/.output/public"
schedule = "17 4 * * *"   # external live-link lane
build_commands = ["bun run build"]  # docs/package.json: gen-crate-pages + vite build + prerender-static
source_link_commands = ["bun run check:repo-links"]  # cargo xtask docs repo-links
site_link_commands = ["bun run check:links"]  # lychee + lychee.toml remaps over .output/public
spell_commands = [...]   # survey docs workflow spelling step at migration
verify_commands = [...]  # deployed-verify step at migration
```

Delete `scripts/ci/{docs-lychee-contract,codebook-contract,construct-result-contract}.sh`
(Q5 CONFIRMED 2026-09-17: zero callers in main for all three).

### release declares (D + E + E-2 + Q7/Q8)

- construct: `kind = "docker"`, image `projectjackin/construct`,
  dockerfile `docker/construct/Dockerfile`, context (verify: repo root?),
  platforms `[linux/amd64, linux/arm64]`, Q8 registry auth
  (Docker Hub + `DOCKERHUB_USERNAME`/`DOCKERHUB_TOKEN` secret refs).
- preview: tarball/native kind per product + `producer_workflow = "CI"`,
  `producer_conclusion = "success"`, `modes = [validate, build, rehearse]`.
- release: stable kind + `tag_pattern = "v[0-9]*"` (Q7) +
  `[[release.credential]]` keychain setup/teardown pairs +
  archive members (4 target tarballs + capsule tarball + SBOMs + symbols) +
  capsule signing orchestration (schema/verifiers stay Jackin).
- jackin-dev: `kind = "versioned-tool"`, file `jackin-dev.yml`,
  `version_manifest = "crates/jackin-dev/Cargo.toml"`,
  `version_prefix = "jackin-dev-v"`, `publish_group = "homebrew-tap-publish"`,
  `version_gate_tasks = [...]` (extracted from ab5b0c4 validate-version-bump:
  paths-filter classify + cargo-tree closure + manifest compare),
  `assert_tasks = [...]` (release-exists + formula check),
  `build_tasks = [...]` (zigbuild matrix cells),
  push/PR path lists from the copy's `on:` block.

### renovate (H8a)

`[renovate]`: enabled, schedule `0 6 * * *`, dedicated PAT secret
(uppercase name, NOT GITHUB_TOKEN), config `renovate.json`, validate=true,
`lanes` as needed. Upstream-content checks (customManagers, mise versions)
stay Jackin named tasks.

### prepared tools (B)

```toml
[[declare]]
primitive = "prepared-tool"
units = [...]   # consumers of ci-xtask + codebook
[declare.args.tools]
ci-xtask = [...]
codebook = [...]
```

### close #994

Only after: migration renders green + policy passes at `<PIN>` + every
ledger L-row verified against the render + hygiene/docs bodies mined.
Close unmerged with a superseded-by comment linking the migration commit(s).

## Commit 3 (if needed): leftovers

Q6 lanes_input declares (if Q6 lands after commit 2); any matrix-finding
fallout; perf measurements (cold/warm critical path per §6).
