# Build-path audit: velnor-workflow consumer compiles, candidate-once, D19 fail-closed

Scope: READ-ONLY. Repo `/Users/donbeave/Projects/tailrocks/velnor-project/velnor-bootstrap`
(`.../velnor` never touched). Every body cited below was opened and read.
Method: exhaustive `rg` for `cargo build | cargo install | mbx build | cargo run | mbx run |
--pin-build | --plain | --check` over `.github/workflows/`, `.github/actions/`,
`crates/velnor-workflow/src/`, plus full reads of each hit's enclosing block.

## 1. No cargo install/build of velnor-workflow on the consumer path — PASS

### 1a. Generated YAML: every build/install line (exhaustive)

Builds OF velnor-workflow (3 sites, 0 on the consumer path):

| # | Site | Command | Verdict |
|---|------|---------|---------|
| 1 | `.github/workflows/ci-unit-rust.yml:530` | `cargo build --locked -p velnor-workflow --manifest-path "$worktree/..."` (debug, head worktree, slow path only) | NECESSARY candidate build (§2) |
| 2 | `.github/workflows/ci-runtime-products.yml:142` (header comment `:8`) | `cargo build --locked --no-default-features --release --package velnor-workflow --bin velnor-workflow` | NECESSARY producer build (Stage-0 immutable products; hermetic: isolated `CARGO_HOME`, clean-tree/RUSTFLAGS guards `:130-141`, per-platform matrix, closure proof `:143-166`, attestation, never-overwrite publish) |
| 3 | `.github/workflows/release.yml:3121`, job `image-platform`, step "Build the workflow binary for the image" | `mbx build --locked --release --package velnor-workflow` → `release-binaries/<arch>/velnor-workflow` | PRODUCER build (release artifact: `docker/job-ubuntu.Dockerfile:278` COPYs it into the published GHCR job image). NOT a consumer runtime acquisition. Divergence noted, not a violation: default features (vs producer `--no-default-features`), no closure proof/attestation — Dockerfile only `--help` smoke-tests (`:279-285`). One build per matrix arch (amd64/arm64), matching §2's per-platform identity. |

Not velnor-workflow (enumerated for completeness, all release payload/tooling):
- `mbx build --package velnor-runner...` — preview.yml:389,582,742,758; release.yml:2916,3387,3543,3806.
- `mbx run --package velnor-runner --bin velnor-guest-image` — preview.yml:449,856; release.yml:3603,3898.
- `cargo install cargo-deb --version 3.7.0 --locked` — preview.yml:729 (debian job, step "Install cargo-deb" `:723-730`), release.yml:3756 (same step `:3750-3757`). Tool install only; the packaging itself runs `velnor-workflow release package-deb ... --no-build true` (preview.yml:870, release.yml:3912), i.e. explicitly NOT building.
- Zero hits in `maintenance.yml`, `nightly.yml`, `ci-release-package-signer.yml`, `ci-main.yml`, `ci-pr.yml`, `ci-policy.yml`, `ci-unit-{bun,docker,docs,opentofu}.yml`, and both `.github/actions/*`.

Consumer acquisition is download-only: `.github/actions/setup-velnor-workflow/action.yml` (full read) contains zero `cargo` invocations — resolve closure → `gh release download` → dual attestation verify → manifest/digest/self-report verify → PATH. Its header states "This action NEVER compiles".

Adjacent (not a YAML cargo build, disclosed): the generator's own unit checks dispatched via
`velnor-workflow run` (`mbx run/nextest/clippy`, `.github/ci/project.toml:228-231`) compile the
crate under test — inherent to testing the generator, not runtime provisioning. The candidate
fast path reuses that `target/debug/velnor-workflow` (zero builds, ci-unit-rust.yml:520-524).

### 1b. Generator Rust: every cargo build/install string (exhaustive)

LIVE emission templates (3, matching the 3 YAML sites above):
- `crates/velnor-workflow/src/primitives/runtime_products.rs:338` — producer template (header comment `:233-234`); test `:882` pins the exact string. → YAML site 2.
- `crates/velnor-workflow/src/primitives/ir.rs:1520` — candidate template; test `:632-647` pins `matches("cargo build").count()==1`, locked `-p velnor-workflow`, no `--pin-build`-adjacent flags, no `cargo install`. → YAML site 1.
- `crates/velnor-workflow/src/primitives/release.rs:1221` — image template (`{cargo_cmd} build ... {workflow_package}`, `IMAGE_WORKFLOW_PACKAGE="velnor-workflow"` at `:51-53`); test `:3952-3957` pins it. → YAML site 3.
- `release.rs:1071` — LIVE but tooling: `cargo install cargo-deb` template → preview.yml:729 / release.yml:3756 (not the workflow runtime).
- `release.rs:1664/2041` (+`cargo→mbx` rewrites `:1734/:2074`) — LIVE release/preview BINARY templates for the release package placeholder (`--package {}`, rendered as `velnor-runner`), not velnor-workflow.

LIVE resolver (not CI-reachable as a build): `policy.rs:1292-1300` `build_pinned_binary`
(`cargo install --locked --no-default-features`, local clone for owner / `--git` for consumer).
Reachable ONLY with explicit `--pin-build` AND without `CARGO_NET_OFFLINE=true`
(`policy.rs:1089-1091`); no generated file passes `--pin-build` (test `lib.rs:12766-12782`;
YAML grep: zero hits). Docs at `policy.rs:28-30,101,204-207,538,682,1128-1129`; the fail-closed
error naming the hatch is `policy.rs:1186-1192`.

LIVE validators (scan rendered output, emit nothing): `lib.rs:2917-3045`
(`validate_workflow_runtime_install_roots`, `workflow_runtime_installs`); the
`VELNOR_WORKFLOW_INSTALL_GIT_URL` const (`lib.rs:80`) is referenced ONLY by test fixtures.

FIXTURE or TEST-ONLY (all `cargo-install` strings in `lib.rs` fall here — no live renderer emits one):
- `lib.rs:10664`, `lib.rs:10768`: `cargo install --locked --git ... velnor-workflow` inside `#[test]` fixture closures (`runtime_install_root_validator_refuses_shared_destinations` at `:10652`, `..._copies_and_judges_actions` at `:10756`).
- `lib.rs:10561` (`run: cargo build` comment-fixture), `lib.rs:10221-10235` (mbxify test + template), `lib.rs:10810` (`guest_seed_job` fixture), `lib.rs:16721,16733` (Dockerfile fixtures), `lib.rs:15811` (test `Cli` literal), `ir.rs:323` (`CHECK` const in `#[test] velnor_lane_check_implies_policy_runtime`), `lib.rs:6886-6908` (CLI `--pin-build` parse tests).
- Negative asserts (prove absence): `lib.rs:7076,7290,7434-7445,11530,11757-11758,12706-12750,12873-12874`; `policy/tests.rs:206,221,693,1189,1206`.

Backstop test: `lib.rs:10579-10609` (`every_runtime_install_owns_a_revision_addressed_root`) asserts
`installs.is_empty()` — "No generated consumer compiles the runtime."

**Item verdict: PASS.** Consumer path (setup action, unit jobs, policy jobs) compiles nothing;
the only velnor-workflow compiles are the candidate, producer, and release-image producer builds.

## 2. Candidate built at most once per platform/source identity — PASS

- Exactly one `cargo build` literal exists in the candidate template (`ir.rs:1520`) and one in
  rendered YAML (`ci-unit-rust.yml:530`); test pins count==1 (`ir.rs:632-642`).
- Emission gates (all must hold): GitHub lane AND owner repo AND structural crate ownership
  (`ir.rs:3958-3962`, `ir.rs:1442-1444` kind+root match, never unit id); rendered only into the
  github collapsed job (`ir.rs:4256-4264`, no `always()`); test pins exactly-one owning unit and
  foreign-never (`ir.rs:255-300`).
- Callers: only `github-rust-velnor-workflow` passes `candidate_publish: true`
  (`ci-pr.yml:976`, `ci-main.yml:1109`); Velnor callers pass `policy_runtime: true` instead
  (`ci-pr.yml:993`, `ci-main.yml:1126`); both inputs default false (`ci-unit-rust.yml:78-84`).
- Runtime gates in `verify-github` (`ci-unit-rust.yml:154-587`): same-repo PR (`:489`, publish `:547`);
  skip+exit-0 when head closure == base closure (`:513-518`, zero builds); fast path reuses the
  checks' own `target/debug/velnor-workflow` when merge==head (`:520-524`, zero builds, else loud
  `::error::`); slow path builds ONCE in the same step (`:525-532`); publish step is upload-only
  (`:546-552`). No second leg: `verify-velnor` (`:588+`, fully read) has no candidate steps.
  No matrix/strategy, no retry wrapper, no `continue-on-error` in the file (only `curl --retry`
  for the mold download, `:373`).
- Identity: artifact `velnor-workflow-candidate-<closure16>-<os>-<arch>` (`:545`) + manifest
  (`profile/platform/repository/run_id/revision/build_revision/binary_sha256`, `:544`); the
  policy consumer re-verifies manifest closure == pin candidate closure in full
  (`ci-main.yml:176-214`, `ci-policy.yml:74-112`).

**Item verdict: PASS.** At most one build per run per platform/source identity; usually zero.

## 3. D19 --check fail-closed — PASS

- `--pin-build` over all of `.github/workflows/` + `.github/actions/`: **zero hits**. (`--check`
  hits are all `sha256sum --check`.)
- `--check` is the generator's own flag (`velnor-workflow --plain --check`, `lib.rs:409-414`;
  `--pin-build` at `:425-431`, "Local development only; never emitted into generated CI").
  D19: after `--check` proves the running binary agrees with the tree, `lib.rs:4494-4505` calls
  `policy::verify_declared_pin_renders_tree` (`policy.rs:534-548`) proving the DECLARED PIN
  renders it too.
- Only unit running `--plain --check`: `rust-velnor-workflow` (`.github/ci/project.toml:222-231`;
  4 vectors = github/velnor × pr/full; `rg -c` = 4, no other unit). Every CI `--check` site is a
  dispatch of that unit, and the pin is provisioned at each — all four run `CARGO_NET_OFFLINE=true`:
  1. `ci-unit-rust.yml:478` github lane (via `ci-pr.yml:877`, `ci-main.yml:1010`) — pin via `VELNOR_WORKFLOW_PINNED_BINARY` from the closure-verified runtime artifact (`:188-209`).
  2. `ci-unit-rust.yml:796` velnor lane (via `ci-pr.yml:977`, `ci-main.yml:1110`, `policy_runtime:true`) — pin via "Provision pinned Velnor workflow policy runtime" → `VELNOR_WORKFLOW_PINNED_BINARY` (`:672-722`; generator `lib.rs:4224-4260`, wired only when the unit runs `--plain --check`, `ir.rs:3957`).
  3. `release.yml:1500` `release-github-rust-velnor-workflow` — pin via setup action installing the PIN product on PATH (`:1389-1394`, rev `ad73177...`) + `VELNOR_WORKFLOW_POLICY_REVISION` (`:1395`); "the guard finds it on PATH by closure" (`release.rs:1819-1824`).
  4. `release.yml:2684` `release-velnor-rust-velnor-workflow` — pin via provision step → `VELNOR_WORKFLOW_PINNED_BINARY` (`release.yml:2648`; emitted only for `--plain --check` units, `release.rs:1825-1830`). The `run` binary itself is ambient fleet image (documented residual, `lib.rs:4205-4217`); the guard uses the env slot, never PATH.
- No `--check` in preview/maintenance/nightly/signer (no `run --unit rust-velnor-workflow` there).
  Adjacent same-resolver `policy` invocations (`ci-main.yml:236`, `ci-policy.yml:134`,
  `preview.yml:213`, `release.yml:67`) likewise pass no `--pin-build`; pins via setup action at
  the base pin + manifest-bound candidate slot (`ci-main.yml:152-214`).
- Fail-closed mechanism (`policy.rs:1063-1196`): resolution order running-binary → env slot →
  PATH → previously installed → build; env-slot mismatch is a hard error (`:1163-1170`); build
  requires explicit `--pin-build` AND online (`:1089-1091`, `policy/tests.rs:1187-1209`), else the
  loud "no velnor-workflow renderer ... is provisioned and building one is forbidden here"
  usage error (`:1186-1192`) listing every attempt. No silent path exists.
- `release.yml` self-hosted gate, fully read — LOUD FAIL, no silent build: (a) `admit-runner`
  (`:33-42`) only rejects Velnor-only dispatch with `exit 1`, else no-op; (b) all 21 setup uses
  carry `if: runner.environment == 'github-hosted'` (`:58,:103,:141,:174,:210,:251,:398,:522,:646,:770,:894,:1018,:1142,:1266,:1390,:1514,:1638,:1762,:2899,:3301,:3665`),
  all inside `ubuntu-24.04` jobs where the gate passes; (c) self-hosted jobs
  (`runs-on: [self-hosted, velnor-target-mvp]`, e.g. `:1876`, `:2573`) contain NO setup step —
  `velnor-workflow` is ambient from the fleet image (e.g. `release-velnor-bun-velnor`, `:1872-1909`,
  fully read); (d) exhaustive grep proves zero `cargo install`/`cargo build` fallback on that
  path — a missing binary fails loudly at the shell, a missing pin fails loudly in
  `resolve_pinned_binary`, and compilation is doubly forbidden (no `--pin-build` in YAML +
  `CARGO_NET_OFFLINE=true` in the Rust jobs).

**Item verdict: PASS.** `--pin-build` absent from CI; pin provisioned at all 4 `--check` sites;
unprovisioned pins fail loudly, never silently compile.
