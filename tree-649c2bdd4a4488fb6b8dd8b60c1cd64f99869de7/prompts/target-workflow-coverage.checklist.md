# Checklist: Target Workflow Coverage

Goal: [target-workflow-coverage.md](target-workflow-coverage.md). Work
top-to-bottom. Prove every fix against the fixture (never edit the fixture).

Behavior truth: <https://github.com/actions/runner> + each action's **latest**
upstream source. Registry (source links):
`docs/reference/target-action-registry.md` — open the row and read the upstream
source before touching an adapter; Velnor tracks latest only (no historical
versions). Only cover features the consumers (Jackin, ChainArgos) actually use.
Contract: `docs/native-action-adapter-contract.md`.

Primary files:
`crates/velnor-runner/src/action.rs` (`NativeActionAdapter` enum + router at
`112-157`), `executor.rs` (`native_*` impls, dispatch `1037-1102`),
`checkout.rs`, `container.rs`, `runtime_env.rs`, `command_files.rs`,
`script_step.rs`, `plan.rs`.

> Status legend per the latest inventory: ✅ full · 🟡 partial · ❌ missing.
> Re-verify each against the current code before assuming.

---

## 0. Orientation

- [ ] List the `NativeActionAdapter` variants (`action.rs:112-132`) and the
  router (`action.rs:134-157`). Confirm every target family maps to a variant.
- [ ] Confirm the dispatch catch-all (`executor.rs:1067-1073`) is unreachable
  (no declared-but-unimplemented adapter).
- [ ] For each family below, open its `native_*` impl and compare against the
  **latest** upstream source (registry `Source` link), not just docs.

## 1. Checkout & SCM

- [ ] `actions/checkout` (`checkout.rs`, `ExecutableStep::Checkout`) — `path`,
  `ref`, `fetch-depth`, `token` + token masking (`checkout.rs:1257-1282`),
  external-repo checkout used by targets. **`submodules`, sparse checkout, LFS
  are out of scope** (contract §132) — confirm no consumer workflow needs them;
  if one does, the audit fails first and the feature is added as a capability.
- [ ] Verify multi-repo / selected-repo checkout patterns the targets use.

## 2. Cache family

- [ ] `actions/cache` ✅ (`native_cache` `executor.rs:1755`, save `1868`) — key,
  restore-keys prefix, exact/partial hit, `fail-on-cache-miss`, `lookup-only`,
  outputs (`cache-hit`, `cache-primary-key`, `cache-matched-key`).
- [ ] `Swatinem/rust-cache` ✅ (`native_rust_cache` `1834`, save `1884`) —
  shared-key, cache-directories, cache-on-failure, outputs.
- [ ] **Transport model review:** local shared-workdir store
  (`_velnor_caches`, `executor.rs:2358-2400`). Confirm cross-run/slot isolation
  is correct and caches do not bleed between unrelated runs. Decide whether
  GitHub service-backed cache transport is needed for Phase 0 (roadmap open
  question 7) — document the decision.
- [ ] Confirm `hashFiles()` keying matches GitHub.

## 3. Artifact family

- [ ] `actions/upload-artifact` ✅ (`native_upload_artifact` `2085`) — glob,
  `if-no-files-found`, `include-hidden-files`, `retention-days`, outputs
  (`artifact-id`, `artifact-url`, `artifact-digest`).
- [ ] `actions/download-artifact` ✅ (`2171`) — pattern/name, `merge-multiple`,
  `download-path`.
- [ ] `actions/upload-pages-artifact` ✅ (`2221`).
- [ ] `actions/deploy-pages` 🟡 (`2244`) — currently returns a synthetic
  `page_url`, no real deployment. Confirm the fixture `pages.yml` +
  `check-deployed-docs` passes with current behavior; improve if it depends on
  more.
- [ ] **Transport model review:** artifact store
  (`_velnor_artifacts/run-…`); confirm fan-in/fan-out across jobs in the same
  run works (multi-arch `push-by-digest` + `merge-multiple`).
- [ ] Confirm artifact overwrite/re-run behavior is acceptable
  (`executor.rs:2099-2101`).

## 4. Setup / tool actions

- [ ] `jdx/mise-action` ✅ (`native_mise` `1104`) — install, `install_args`,
  working-directory, `MISE_*` env, PATH injection. Targets use `mise` as the
  main installer — verify Python-via-mise (ChainArgos `ansible.yml`).
- [ ] `extractions/setup-just` ✅ (`native_setup_just` `1170`).
- [ ] `rui314/setup-mold` 🟡 (`native_setup_mold` `1160`) — apt/error install;
  confirm sufficient for targets.
- [ ] `mozilla-actions/sccache-action` 🟡 (`native_sccache` `1145`) — server
  start + env injection; confirm soft-fail gates and stats output match targets.
- [ ] `crazy-max/ghaction-github-runtime` ✅ (`native_github_runtime_export`
  `2302`) — exports `ACTIONS_*`.

## 5. Path filtering & aggregation

- [ ] `dorny/paths-filter` ✅ (`native_paths_filter` `1403`) — YAML rules, glob,
  per-rule boolean + `_count` + `_files` + `changes` outputs, git-diff source.
  Confirm downstream gating works.
- [ ] `aggregate-needs` local composite — `needs.*.result` and `toJSON(needs)`
  correctness for required aggregate jobs.
- [ ] Confirm runtime expression fallback like
  `steps.dispatch.outputs.x || steps.filter.outputs.x` evaluates correctly.

## 6. Docker family

- [ ] `docker/setup-buildx-action` ✅ (`1251`).
- [ ] `docker/login-action` ✅ (`1300`) — stdin password, default registry.
- [ ] `docker/metadata-action` 🟡 (`2269`) — **tag templates missing**
  (`type=semver`, `type=ref`, `type=sha`, pep440, custom). Implement the tag
  template grammar the targets use; confirm `tags`/`labels`/`json` outputs.
- [ ] `docker/build-push-action` ✅ (`1327`) — file, platforms, tags, labels,
  cache-from/to, push/load.
- [ ] `docker/bake-action` ✅ (`1371`) — files, set, push.
- [ ] Confirm Docker socket mount + per-job network + service-container aliases
  work inside the Linux job container.

## 7. Renovate

- [ ] `renovatebot/github-action` 🟡 (`native_renovate` `1218`) — runs as Docker
  image; token masking; `RENOVATE_*` env. Confirm target `renovate.yml` /
  `renovate-validate.yml` shapes pass.

## 8. Composite actions

- [ ] Local composite expansion (`action.rs:308-339`, `composite_action_invocations`
  `656-675`) — input defaults, expression rendering, step-id prefixing.
- [ ] Repository composite expansion (`341-372`).
- [ ] **Known gaps** (`action.rs:882-885`): nested local composite uses and
  nested Docker composite uses are *not implemented*. Determine whether the
  target/fixture surface needs them; implement if so, else document as
  out-of-scope with a clear error.
- [ ] `check-deployed-docs`, `check-fixture-output`, `aggregate-needs` all work.

## 9. Reusable workflows

- [ ] GitHub-expanded reusable workflow jobs (ChainArgos `rust-docker-build.yml`,
  Jackin `reuse-caller.yml` shape) execute correctly — `workflow_call`, inputs,
  `secrets: inherit`, `toJSON(needs)`.

## 10. Runtime features

- [ ] Command files (`command_files.rs`): `GITHUB_ENV`, `GITHUB_OUTPUT`,
  `GITHUB_PATH`, `GITHUB_STATE`, `GITHUB_STEP_SUMMARY` (incl. heredoc).
- [ ] Job outputs / step outputs across action↔composite chains
  (`evaluate_job_outputs` `executor.rs:854`).
- [ ] Expression evaluation: `lit`/`expr`/`format`, contexts
  (`github`/`env`/`secrets`/`inputs`/`steps`/`needs`/`runner`/`job`), `contains()`,
  `fromJSON()`, `toJSON()`.
- [ ] `defaults.run` (shell + working-directory), per-step `working-directory`.
- [ ] Matrix expansion (handled pre-adapter in `runner.rs`) — confirm target
  matrices (`${{ matrix.runner }}`, package matrices) expand correctly.
- [ ] Runtime/cache/OIDC env injection (`runtime_env.rs:4-188`): `ACTIONS_RUNTIME_*`,
  `ACTIONS_CACHE_URL`, `ACTIONS_ID_TOKEN_REQUEST_*`, `ACTIONS_RESULTS_URL`,
  `CACHE_SERVICE_V2`.
- [ ] Secret masking at adapter-output level (inventory flagged this as a gap —
  confirm adapter-produced outputs are masked, not just script output).

## 11. No dead adapters

- [ ] Confirm removed families (SetupPython, RustToolchain, CargoInstall) have no
  lingering variants/impls/tests/reference entries (roadmap §2). Remove any.

## 11b. Performance & Rust-first optimization (mission)

Coverage must also be *fast and Rust-optimal* (see `../docs/mission.md`), not merely
correct. For each relevant adapter:

- [ ] **Parallelism:** exploit the beefy self-hosted host where the job graph
  allows (e.g. parallel cache restore, concurrent independent setup). Document
  what runs in parallel vs GitHub's serial defaults.
- [ ] **Aggressive cache tactics:** cargo registry/git cache, `target/` dir,
  sccache, and rust-cache should be warmer and broader than GitHub defaults
  because Velnor owns the host/storage. Verify warm-rebuild speedups.
- [ ] **Latest Rust pre-cache:** the job image (`docker/job-ubuntu.Dockerfile`)
  and tool installs should favor the **latest** toolchain/tools, pre-cached so
  setup is near-instant. Confirm mise/rustup resolve to current versions fast.
- [ ] **Cheaper than JS:** confirm native adapters launch faster than the
  marketplace JS they replace; record the delta where measurable.
- [ ] **Surface the wins:** emit cache hit/miss, sccache stats, and time-saved
  into the step log (ties into the *GitHub UI parity* prompt).

## 12. Verification gates

- [ ] `cargo fmt --check`
- [ ] `cargo test -q` (adapter tests in `executor.rs:4650+`, `action.rs`)
- [ ] `cargo run -q -p velnor-tools -- target-audit --check-target-mvp …`
- [ ] `cargo run -q -p velnor-tools -- target-verify`
- [ ] `cargo run -q -p velnor-tools -- fixture-audit`
- [ ] Fixture lanes exercising each family pass (see *Fixture proof* prompt).

## 13. Definition of done

- [ ] Every target family `IMPLEMENTED`; `PARTIAL`/`MISSING` items either fixed
  or documented as not-needed-for-Phase-0 with rationale.
- [ ] Runtime features behave per `actions/runner`.
- [ ] No dead adapter code.
- [ ] All gates in §12 green.
