# Independent source review: PR #963 at `fb78d85`

Verdict: **REJECT source admission at this exact head.** The tree is reviewable and its hosted checks are green, but five current Codex inline findings remain valid generic-generator defects. No merge approval follows from this report.

## Exact revision and feedback census

- PR: `963`
- base: `b5a4b4afaa6ca807927cacc03659b570a895dd5c`
- head: `fb78d85d464fd5082e5c161922afd7942380fabc`
- merge-base: the exact base above; 17 commits ahead
- replacement tree comparison: `git diff --quiet 5b9a16a620951b65bbfe0a5cf7b1ffe04a317303 fb78d85...` passed (same tree as the prior PR head)

I fetched the exact PR head into a detached review worktree. GitHub API pagination found one Codex review summary, five inline Codex findings at this exact head, and one issue summary comment. There are no reply comments that resolve those findings. Current `gh pr checks 963` reports 22/22 non-skipped checks successful (other matrix lanes are skipped); this does not test the generic counterexamples below.

## Findings still present

### P1 — `CARGO_MANIFEST_DIR` resolves against the wrong package

`crates/velnor-workflow/src/scan/rust.rs:738-760` scans every Rust source when `package_root == "."` and passes the source parent plus the root package to `resolve_include_path`. `crates/velnor-workflow/src/rust_include.rs:301-322` canonicalizes `canonical_root.join(package_root)`, so a workspace root that is also a package resolves a member source's `env!("CARGO_MANIFEST_DIR")` to the repository root. A member include can therefore be reported missing or watch the wrong root file. The existing manifest-dir test only exercises non-root `crates/app` (`scan/rust.rs:1114-1161`); no root-package-plus-member regression exists.

The scanner must associate each source with its owning manifest or keep nested member sources out of the root package's include pass. A generic workspace can currently fail generation or record an incorrect closure/watch set.

### P1 — Skills verification hard-codes Bun 1.4.0

`crates/velnor-workflow/src/s2/mod.rs:226-230` defines one global `SKILLS_BUN_VERSION = "1.4.0"`. `crates/velnor-workflow/src/s2/scan/skills.rs:45-53` emits both checks with `test "$(bun --version)" = "1.4.0"`; `:122-130` stamps the same version into the Skills unit and detected evidence. No target-owned Skills runtime/config is read. A valid target that pins another compatible Bun version is emitted an incompatible workflow and fails before its checks run. The fixture assertions at `:1709-1718` merely codify the hard-coded value; the ordinary Bun control at `:1938-1940` is not a Skills-runtime test.

The version must be derived from target-owned configuration or required as an explicit target contract, not from this generator repository.

### P1 — Any provider marker requires unrelated provider manifests

`crates/velnor-workflow/src/s2/scan/skills.rs:21-27` lists all five manifests as required. `has_plugin_marker` at `:137-146` activates on any one provider marker, but `validate_plugin_manifests` at `:384-390` then requires `plugin.json`, Codex, Kimi, Claude, and Claude marketplace files before validating the provider that was actually present. A valid provider-specific repository containing only `.codex-plugin/plugin.json` is therefore rejected for lacking unrelated Kimi/Claude files. The test at `:1865-1887` explicitly preserves this false requirement, so the current suite does not protect generic provider discovery.

Detection must validate each provider manifest that exists and use target-owned shared metadata where needed; it cannot impose the estate's full provider set on every target.

### P1 — Template boundaries depend on prose substrings

`crates/velnor-workflow/src/s2/scan/skills.rs:566-568` adds a Skill to the template set only when `has_template_context` succeeds. That helper at `:797-803` recognizes only three exact textual forms. `template_files` at `:805-815` then hides `skills/<name>/templates/**` only for those names. A catalogued Skill with an unlinked `templates/` directory, or a valid Markdown reference written in another form, leaks template `Cargo.toml`/`package.json` into generic detectors and creates false Rust/Bun units. The fixture at `:1340-1351` uses the exact `](templates/)` spelling, and tests at `:1695-1720` therefore cover only the heuristic-positive path.

Once the catalog establishes `skills/<name>/templates/` as a boundary, exclusion must be structural and independent of prose/link spelling; helper template boundaries need the same explicit structural contract.

### P2 — `--watch` limits before selecting successful runs

`crates/velnor-tools/src/lane_compare.rs:529-553` invokes `gh run list` with `--limit` but no success/status filter. `lane_compare_watch` validates `completed` + `success` only after receiving that truncated list (`:457-477`). A newest queued, in-progress, or failed run can consume the limit and make the watch abort even when enough older successful runs exist. The current code must filter at the API (`--status success`) or page until the requested number of eligible successful completed runs is collected.

## Test/gate interpretation

The author-reported local and hosted passes establish that the submitted tree builds and its existing fixtures pass; they do not cover these five generic counterexamples. The findings are source-level correctness blockers, not flaky-test requests. No source edits were made by this review.

