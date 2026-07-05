# Checklist: ChainArgos Runner Parity

> **Superseded — shipped.** This checklist is archived with the completed
> real-target parity prompt; do not work unchecked items top-to-bottom. Current
> production status and follow-ups live in
> [`../docs/master-plan.md`](../docs/master-plan.md) and
> [`../docs/comparison.md`](../docs/comparison.md).

Work top-to-bottom. Phase A first, fully green and merged, before Phase B.
Goal + context: [chainargos-runner-parity.md](chainargos-runner-parity.md).
All work happens against `ChainArgos/java-monorepo` (Phase A/B workflow edits)
and the Velnor repo (Phase B adapter/runtime fixes).

Conventions:
- Branch in ChainArgos, open a PR, let CI run, iterate, merge. **Never push to
  `main` directly** — this is a self-imposed rule and holds even though `main` is
  not branch-protected (the lack of protection is not permission to push to it).
- Never weaken a check to make it pass (no skipped tests, no blanket lint
  allows, no deleted assertions). Fix the real cause.
- Use `gh run watch` / `gh run view --log-failed` to read failures; do not guess.

---

## Phase 0 — Setup & access

- [ ] Confirm `gh auth status` has `repo` + `workflow` scopes for `donbeave`.
- [ ] Clone ChainArgos: `gh repo clone ChainArgos/java-monorepo`.
- [ ] Docker Hub auth is **already satisfied by org-level secrets** (visible to
      all repos): `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN`, plus `GH_READONLY_TOKEN`,
      `GITLEAKS_LICENSE`, `RENOVATE_TOKEN`. `secrets.DOCKERHUB_*` resolves at
      runtime on `ChainArgos/java-monorepo`. (A `repo`-scoped `gh secret list`
      does **not** show org secrets — do not mistake that for "absent".) No secret
      needs adding for Phase A.
- [ ] Phase A needs no further access. The **only** operator-provided item is the
      Phase B Velnor-host connect command + JIT-registration token (goal
      "Authentication"); Phase B is gated on Velnor being fixture-green anyway, so
      that is not needed to start or finish Phase A.
- [ ] Record the current (pre-change) failing run URLs for `Rust` as a "before"
      reference: `gh run list --repo ChainArgos/java-monorepo`.

---

## Phase A — GitHub-hosted green baseline

### A1. Repoint runners (all 19 occurrences → `ubuntu-latest`)

- [ ] `rust.yml`: 12 jobs — `warm-sccache`, `changes`, `check`,
      `test-bitcoin-processor`, `test-eth-processor`, `test-coingecko-pricing`,
      `test-tron-processor`, `test-blockchain-explorer`, `test-eth-grpc-server`,
      `test-tron-grpc-server`, `test-legacy-grpc-server`, `rust-required`.
- [ ] `rust-docker.yml`: `changes`, `docker-bake`, `docker-required`.
- [ ] `rust-docker-build.yml`: `build` (orphan reusable — swap for consistency).
- [ ] `ansible.yml`: `syntax-check`.
- [ ] `renovate.yml`: `renovate`.
- [ ] `kestra-build-image.yml`: `build`.
- [ ] Verify nothing references `hetzner-sentry-ci` anymore:
      `grep -rn hetzner-sentry-ci .github/`.

### A2. `rust.yml` — toolchain provisioning (the biggest blocker)

On self-hosted, `check` and all `test-*` jobs only run
`echo "$HOME/.local/share/mise/shims" >> "$GITHUB_PATH"` — they assume mise +
the toolchain are already installed on the box. GitHub-hosted images have none of
that.

- [ ] Add a `jdx/mise-action` step (pinned SHA, matching `warm-sccache`) to
      `check` and every `test-*` job, before the steps that need `cargo`/
      `nextest`/`protoc`. Keep the existing shim-PATH line or let mise-action set
      PATH — verify `cargo`, `cargo nextest`, `protoc`, `cargo fmt`, `cargo clippy`
      all resolve.
- [ ] Confirm `mise.toml` tools install on hosted: `oracle-graalvm-25.0.1` (Java),
      `node 24.16.0`, `python 3.14`, `just latest`, `cargo:cargo-nextest`,
      `protoc`, and Rust from `rust-toolchain.toml` (read live versions). If a tool
      fails to install on the clean image (e.g. GraalVM download), scope mise
      installs per-job via `install_args:` so jobs only pull what they use (the
      Rust jobs do not need GraalVM/Node/Python) — decide autonomously, do not block.

### A3. `rust.yml` — sccache (host bind-mount → portable)

`env.RUSTC_WRAPPER: sccache` + `SCCACHE_DIR: /var/cache/sccache` is a host path
that does not exist / is not writable on hosted runners.

- [ ] Replace the host-dir sccache with `mozilla-actions/sccache-action` +
      `SCCACHE_GHA_ENABLED: "true"` (GitHub Actions cache backend), matching the
      fixture. Remove the hardcoded `SCCACHE_DIR: /var/cache/sccache`. Keep
      `RUSTC_WRAPPER: sccache`. Add the action step to jobs that compile (`check`,
      `test-*`, `warm-sccache`).
- [ ] Keep `warm-sccache` (push-to-main) — it still pre-warms, now into the GHA
      cache. Verify it does not race the per-PR jobs (different cache scope/key).
- [ ] If sccache + GHA backend proves flaky on first pass, mark sccache
      `continue-on-error` for the wrapper setup (fixture does this) — never let
      cache infra fail the build.

### A4. `rust.yml` — mold linker

`env.RUSTFLAGS: "-C link-arg=-fuse-ld=mold"` needs mold installed.

- [ ] Add `rui314/setup-mold` to every compiling job, or confirm the mise/job
      image provides mold. Keep the `RUSTFLAGS` mold linker — it is part of the
      intended (and Velnor-relevant) config; only drop it as a last resort if mold
      genuinely cannot be provided on hosted, and record why.
- [ ] Sanity-check `CARGO_BUILD_JOBS: "6"` on a 4-vCPU hosted runner — it is a cap,
      safe to leave; lower only if OOM appears.

### A5. `rust.yml` — tests actually run (project fixes live here)

The per-crate `nextest --profile ci` jobs use testcontainers (DB startup, see
`.config/nextest.toml` 120s slow-timeout, `test-threads = 2`).

- [ ] Confirm Docker is available on the hosted image (it is) so testcontainers
      can start. Watch for image-pull time pushing tests past timeouts on a cold
      runner; raise timeouts only if the slowness is real and infra-bound, not a
      hung test.
- [ ] For every failing crate test, diagnose: is it (a) infra (mise/sccache/mold/
      docker not ready) or (b) a real bug the self-hosted box was masking? Fix
      (a) in the workflow; fix (b) in `backend-rust/<crate>` source/tests per what
      the code is meant to do. **No skips, no ignore, no deleted asserts.**
- [ ] `cargo fmt --all -- --check` and `cargo clippy ... -D warnings` must pass on
      hosted — fix any real fmt/clippy findings in source.

### A6. `rust-docker.yml` + `rust-docker-build.yml` — Docker/Buildx/Bake

- [ ] buildx (`docker/setup-buildx-action`, `driver: docker-container`) and
      `docker/bake-action` work on `ubuntu-latest` out of the box — verify.
- [ ] Docker Hub login (`docker/login-action`) uses org secrets
      `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` (present, all repos) — they resolve
      at runtime, so login works. Note: PRs from forks do **not** receive secrets;
      these workflows are same-repo (`pull_request` on internal branches), so
      secrets are available. If a no-secret context is ever hit, gate login +
      `*.push` so the build still runs credential-free (PRs already set
      `PUSH=false` / `*.push=false`).
- [ ] `docker-bake` builds 8 images from `backend-rust/docker-bake.hcl` /
      `backend-rust/Dockerfile` on one runner. Confirm it fits time/disk on
      hosted; rely on `type=gha` cache (`scope=rust-workspace`) for repeat speed.
      If the first cold build is too heavy (time/disk), the `changes`+bake design
      already builds only affected targets; do not change *what* gets built. Decide
      autonomously on any infra tuning (timeouts, disk cleanup) — do not block.
- [ ] If `backend-rust/Dockerfile` fails to build on a clean runner, fix the
      Dockerfile correctly (real build break, not infra).
- [ ] `rust-docker-build.yml` has no caller — confirm it never triggers on its own;
      it cannot be red. Leave it building-correct for future use.

### A7. `ansible.yml`

- [ ] mise installs Python; `pip install ansible-core` then galaxy collections,
      then `ansible-playbook --syntax-check`. **Risk:** `python 3.14` (mise.toml)
      may be too new for `ansible-core`. If install/syntax fails on 3.14, fix by
      pinning a supported Python for this job (e.g. mise `install_args` or a job
      env) — do not remove the syntax check.
- [ ] Confirm `requirements.yaml` collections install on hosted.

### A8. `kestra-build-publish.yml` + `kestra-build-image.yml`

- [ ] Triggers only on `kestra-docker-containers/**` path changes — only verify
      green if those paths are exercised; otherwise confirm it stays neutral.
- [ ] Needs mise `just` + `cargo:rust-script`, rust-cache, `actions/cache`,
      Docker Hub login. `just exists <pkg>` likely queries Docker Hub → uses the
      org `DOCKERHUB_*` secrets (present, same as A6); no extra creds needed.
- [ ] buildx works on hosted; verify `just build-<package>` for `jvm-base`,
      `kestra-backup`, `kestra-playwright`.

### A9. `renovate.yml`

- [ ] Uses `RENOVATE_TOKEN` (present) and runs renovatebot in Docker — works on
      hosted. Triggers on schedule/push-main/merge_group/dispatch, not PR; confirm
      the runner swap does not break it. No project change expected.

### A10. Phase A verification gate

- [ ] Open PR(s); for each, `gh run watch` until all triggered workflows are green.
- [ ] Re-read any flake; confirm green is stable (re-run once).
- [ ] Merge to `main`. Confirm a green post-merge run for `Rust` and
      `Rust Docker`.
- [ ] Record all green run URLs in `.velnor-live-evidence/` (the evidence
      location used by `docs/comparison.md`). Phase A done.

---

## Phase B — GitHub + Velnor dual-lane parity

> Edit **only** the runner-selection plumbing in the workflows. Every other line
> stays identical to the Phase-A-green version. Every Velnor-lane failure is a
> Velnor bug — fix Velnor, add a test, never touch the job.

### B1. Pre-flight: Velnor must be fixture-green first

- [ ] Confirm the three Velnor prompts are done: target-workflow-coverage,
      github-ui-parity, fixture-proof — fixture lanes green on Sentry.
- [ ] Confirm operator has provided Velnor host access (connect command + JIT
      token). **Velnor-lane runner label:** use the single value defined in the
      goal "Ground truth" (`["self-hosted","velnor-target-mvp"]`) unless the
      operator overrides it; do not redefine the label elsewhere — reference that
      one source.

### B2. Coverage analysis — every ChainArgos action/feature vs Velnor (the plan)

For each, confirm a Velnor native adapter / runtime feature exists and matches
the latest upstream behavior the ChainArgos workflow relies on. Mark
IMPLEMENTED / GAP; open a Velnor fix item for each GAP. Velnor routes by action
*family* and tracks **latest** (repo convention) — match families below, not the
specific `@ref` a workflow happens to pin; read the live workflow at execution.

- [ ] `actions/checkout` — repo checkout (default depth, single repo).
- [ ] `dorny/paths-filter` — `changes` job outputs gate downstream jobs
      (`needs.changes.outputs.*`). Must produce identical boolean outputs.
- [ ] `jdx/mise-action` — installs Rust, `cargo-nextest`, `protoc`, `just`, and
      (where used) GraalVM/Node/Python from `mise.toml` + `rust-toolchain.toml`
      (live versions). Verify Velnor adapter installs the same versions and sets
      PATH/shims identically.
- [ ] `mozilla-actions/sccache-action` + `SCCACHE_GHA_ENABLED` — Velnor must honor
      the GHA-style sccache backend (or its local equivalent) so `RUSTC_WRAPPER`
      works the same.
- [ ] `Swatinem/rust-cache` — cargo registry/git/target restore-save.
- [ ] `rui314/setup-mold` — mold present so `-fuse-ld=mold` links.
- [ ] `actions/cache` — generic cache (kestra rust-script path) with
      `hashFiles` keys + `restore-keys`.
- [ ] `docker/setup-buildx-action` + `docker/bake-action` + `docker/login-action`
      + `docker/metadata-action` + `docker/build-push-action` — buildx/bake from
      inside the Velnor Linux job container via mounted Docker socket; `type=gha`
      cache scope behavior; registry login.
- [ ] `renovatebot/github-action` — Docker-based action under Velnor.
- [ ] Runtime features: `concurrency`, path-filtered `on:`, `workflow_dispatch`
      inputs, `needs.*.result` in `rust-required` / `docker-required` aggregates,
      `if:` expressions, `defaults.run.working-directory` (ansible/kestra),
      `GITHUB_OUTPUT`/`GITHUB_PATH`, matrix expansion, reusable `workflow_call`
      (kestra), `secrets: inherit`.
- [ ] Write this table into `docs/` (e.g. extend the action registry or a new
      `docs/reference/chainargos-coverage.md`) as the authoritative plan.

### B3. Add the lane matrix (runner plumbing only)

For each workflow that should run on both runners, introduce the fixture pattern:

- [ ] Add a `matrix-setup` job (or inline matrix dimension) emitting lane configs:
      github → `"ubuntu-latest"`, velnor → the velnor-lane label JSON (the single
      value from the goal "Ground truth").
- [ ] Set `runs-on: ${{ fromJSON(matrix.config.runner) }}` (and `fail-fast: false`).
- [ ] Keep all existing steps byte-for-byte. Diff the job before/after: the only
      changes are the matrix/`runs-on` lines.
- [ ] Workflow scope follows the goal "In scope" priority: `rust.yml` primary
      (minimum bar), `rust-docker.yml` secondary, ansible/kestra/renovate optional.
      Decide how far to extend autonomously based on parity results.
- [ ] Add a `workflow_dispatch` `lanes` input (all / github-only / velnor-only)
      like the fixture, so lanes can be run independently while iterating.

### B4. Run both lanes, fix Velnor for every behavior divergence

- [ ] Start the Velnor daemon on the operator host with JIT slots carrying the
      velnor-lane label. Confirm it registers and is idle.
- [ ] Dispatch the workflow; watch **both** lanes (cancel stale runs first, per
      AGENTS.md HARD RULEs on verification sequence).
- [ ] For each velnor-lane job that diverges from its github twin (failure,
      wrong output, wrong conclusion, missing log/step, different test result),
      diagnose in Velnor and fix the adapter/runtime in the Velnor repo. Add a
      regression test. Re-run. **Never edit the ChainArgos job.**
- [ ] Verify **step by step**, not just final conclusion: for each step compare
      result, stdout/stderr-relevant outputs, `GITHUB_OUTPUT`/env effects,
      artifacts produced, and cache hit/miss between the two lanes.
- [ ] Confirm the **test set is identical**: every test that passes on GitHub
      passes on Velnor (same count, same names, same pass/fail). No test missing,
      skipped, or differently-resulted on the Velnor lane.
- [ ] Iterate until every velnor-lane job and step matches its github twin.

### B5. Performance parity — verify per step, fix until Velnor ≥ GitHub

> Requirement: velnor-runner must be **no slower than** GitHub on every step and
> job (equal or faster). Mission target is faster; the hard floor is not-slower.

- [ ] Capture per-step and per-job timing for **both** lanes from the GitHub API
      (`started_at`/`completed_at` per step; job durations) and from Velnor's own
      step timing output. Prefer a `velnor-tools` subcommand to fetch + diff both
      lanes (Rust-first automation).
- [ ] Build a per-step comparison table: github ms vs velnor ms, delta, for every
      step of every job.
- [ ] Also capture: queue→first-log latency, total job runtime, warm-cache
      rebuild runtime (second run), cargo/sccache/rust-cache hit rates, Docker
      Buildx cache hit/miss.
- [ ] For **any** step where Velnor is slower than GitHub, find the cause (cold
      cache, adapter overhead, container/daemon startup, serialization, missing
      parallelism) and fix it in Velnor. Re-measure. Repeat until Velnor ≤ GitHub
      for that step.
- [ ] Record the final timing tables (both lanes) as evidence.

### B6. UI / API output parity — equal-or-better, never less detailed

> Velnor authors its own log stream. Its GitHub UI + API output must be at least
> as informative as the GitHub-hosted lane, and ideally nicer (mission). Verify
> field-by-field via the API, mirroring `docs/comparison.md`'s method and the
> github-ui-parity prompt — do not regress that work; extend it to this repo.
>
> **Current state is known-broken (operator-reported): Velnor UI output is not
> displaying correctly / not accurate vs GitHub-hosted.** Do not assume it works
> — start B6 by capturing the ChainArgos velnor-lane job via API and listing every
> field that is missing/empty/wrong vs the GitHub twin, then fix each in Velnor.

- [ ] For each twin job, fetch both lanes' job + steps via the GitHub API
      (`gh api repos/ChainArgos/java-monorepo/actions/jobs/{id}` and `/logs`).
- [ ] Diff field-by-field per step: `name`, `number` (sequential, not 0),
      `started_at`/`completed_at` (real RFC3339, not epoch), conclusion,
      **expandable log present** (real `data-log-url` / fetchable `/logs/{n}`),
      per-line timestamps, `::group::` sections, ANSI color, annotations, step
      summaries.
- [ ] Verify synthetic steps exist on the Velnor lane and are populated:
      "Set up job" (runner version, OS, image, permissions, action list) and
      "Complete job" (cleanup) — not empty, not missing.
- [ ] For every field where Velnor is **less** detailed than GitHub (missing log,
      epoch timestamp, absent step, empty group), fix it in Velnor with a test.
- [ ] Confirm Velnor is **equal-or-better**, and that mission extras appear where
      relevant (cache hit/miss + bytes saved, sccache stats, parallelism, time
      saved) — without ever being less than GitHub.
- [ ] Keep **user-command** stdout/stderr verbatim (only framed), per mission.

### B7. Comparison evidence (per docs/comparison.md)

- [ ] Capture both lanes via the GitHub API (`gh api .../actions/jobs/{id}`,
      `/logs`) — step names, numbers, conclusions, timestamps, expandable logs,
      grouping, ANSI, outputs, cache hit/miss, timing.
- [ ] Confirm velnor-lane output is **equal-or-better**: same conclusions, logs at
      least as informative, plus Velnor's speed/cache wins surfaced.
- [ ] Record evidence + run URLs + behavior table (B4) + timing tables (B5) +
      UI/API field diff (B6) in `.velnor-live-evidence/`.

### B8. Phase B verification gate

- [ ] Both lanes green in parallel on the dual-lane workflow(s), zero job-logic
      edits in ChainArgos.
- [ ] Every step and job conclusion matches between lanes; identical test set
      passes on the velnor-runner (B4).
- [ ] Velnor is **no slower than GitHub on every step and job**, proven by the
      recorded timing tables (B5).
- [ ] Velnor's **GitHub UI + API output is equal-or-better, never less detailed**
      than the GitHub lane, proven by the field-by-field diff (B6).
- [ ] Every Velnor gap (behavior, performance, or output) fixed in Velnor with a
      test.
- [ ] `cargo fmt --check` and `cargo test -q` pass in the Velnor repo.
- [ ] Evidence captured. Update `docs/comparison.md` and the AGENTS.md direction
      log with the result.

---

## Reconciliation (keep direction consistent — AGENTS.md rule)

- [ ] Add the 2026-06-04 direction-log entry in `AGENTS.md` (real-target work on
      ChainArgos now authorized; the "stop at fixture" guard is scoped to
      *unattended Velnor execution*, not to GitHub-hosted CI hygiene).
- [ ] Verify `prompts/README.md` lists this prompt in the completed sequence
      archive — **already added**; keep it accurate if the run order changes.
- [ ] Note the prerequisite baseline in `docs/roadmap.md` if it affects the plan.
