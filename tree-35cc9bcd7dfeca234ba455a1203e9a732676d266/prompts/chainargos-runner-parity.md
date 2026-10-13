# Goal: ChainArgos Runner Parity (GitHub-hosted green, then GitHub + Velnor in parallel)

> **Mission:** see [`../docs/mission.md`](../docs/mission.md). Velnor must be a
> **drop-in** runner: the *same* workflow YAML that is green on GitHub-hosted
> runners must be green on Velnor — **faster, cheaper, nicer**, never by
> weakening the job. This goal proves that on a real target repo, not the fixture.

> **Direction (source of truth):** [`../docs/`](../docs/) —
> [vision](../docs/vision.md), [roadmap/plan](../docs/roadmap.md),
> [comparison](../docs/comparison.md),
> [native action adapter contract](../docs/native-action-adapter-contract.md).
> If this prompt and the docs disagree, the docs win.
>
> **Run order: 4 of 4** (real-target). Phase A has no Velnor dependency (it is
> pure GitHub-hosted CI hygiene) and may run any time. Phase B depends on the
> three Velnor prompts (target-workflow-coverage, github-ui-parity, fixture-proof)
> being green on the fixture first. See [`README.md`](README.md).

> **Operator-authorized real-target work.** The standing hard rule "agents stop at
> the public fixture — never run the real ChainArgos / Jackin repos" is **lifted
> for this goal by explicit operator instruction** (2026-06-04). The agent MAY
> edit, open PRs against, and merge into `ChainArgos/java-monorepo`. The agent
> still MUST NOT set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` or run the Velnor
> lane against the real repo until the operator has provided runner-host access
> (see "Authentication" below) and Phase A is green. See the AGENTS.md direction
> log entry for 2026-06-04.

## Target repository

`ChainArgos/java-monorepo` (private, default branch `main`). A Rust workspace
monorepo (12 crates under `backend-rust/`) plus Java model crates, Ansible
configs, and Kestra Docker containers.

> **Initial state at authoring time (2026-06-04) — verify live before acting,
> these are snapshots that will change:** the repo was **not** branch-protected;
> **all 19 `runs-on:` entries across 7 workflows targeted the self-hosted label
> `hetzner-sentry-ci`**; the `Rust` workflow was failing on that self-hosted
> infra; there was no GitHub-hosted baseline. The workflow inventory, job names,
> action versions, and `mise.toml`/`rust-toolchain.toml` tool versions below are
> likewise authoring-time snapshots — the executing agent must read the live repo
> as the source of truth (per the repo's "track latest only" convention).

Workflows (`.github/workflows/`):

| File | Triggers | Jobs (runner count) | Notes |
|------|----------|---------------------|-------|
| `rust.yml` | push main / PR / dispatch (path-filtered) | `warm-sccache`, `changes`, `check`, 8× `test-*`, `rust-required` (12) | fmt + clippy + per-crate `nextest`; sccache + mold env; **`check`/`test-*` jobs never run `mise-action`** (rely on a pre-provisioned self-hosted mise) |
| `rust-docker.yml` | push main / PR / dispatch | `changes`, `docker-bake`, `docker-required` (3) | `docker/setup-buildx` + `docker/bake-action` building 8 images; Docker Hub login |
| `rust-docker-build.yml` | `workflow_call` | `build` (1) | **orphan** — no caller in repo; single-image reusable template |
| `ansible.yml` | push main / PR / dispatch (path-filtered) | `syntax-check` (1) | mise → `pip install ansible-core` → galaxy collections → syntax check |
| `renovate.yml` | schedule / push main / merge_group / dispatch | `renovate` (1) | self-hosted Renovate; needs `RENOVATE_TOKEN` (present) |
| `kestra-build-image.yml` | `workflow_call` | `build` (1) | reusable; mise `cargo:rust-script`, rust-cache, `just build-*`, Docker Hub |
| `kestra-build-publish.yml` | push main / dispatch (path-filtered) | 3× calls into `kestra-build-image.yml` | jvm-base → backup + playwright |

## Objective

Two phases, one goal. Do not start Phase B until Phase A is green and merged.

### Phase A — Make every ChainArgos pipeline green on **GitHub-hosted** runners

1. Repoint every `runs-on: hetzner-sentry-ci` to a GitHub-hosted runner
   (`ubuntu-latest`).
2. Run each workflow on a PR and find every failure.
3. Fix the **pipeline** where it relied on self-hosted assumptions (preinstalled
   mise, host sccache bind-mount, host mold, host secrets) so it runs on a clean
   GitHub-hosted image.
4. If a failure is in the **project itself** (a broken test, a real
   compile/clippy error, a Dockerfile that does not build), fix the project
   correctly — according to what that code is for and how it is meant to work —
   not by deleting or skipping the check. Self-hosted infra was masking real
   breakage; surface and fix it.
5. Land it via PR(s) and merge. The agent is authorized to PR + merge without
   per-step confirmation.

### Phase B — Run the **same** pipeline on GitHub **and** Velnor in parallel

Mirror the proven fixture dual-lane pattern (`tailrocks/velnor-actions-fixture`
`compat.yml`): a matrix dimension parameterizes **only `runs-on`**; every step is
**byte-for-byte identical** across lanes. Both lanes run at the same time on
different runners.

**The hard constraint (operator, highest priority):** never change the job to
make Velnor pass. The job logic, steps, commands, env, and action versions stay
exactly as they are green on GitHub-hosted. The *only* permitted workflow edit is
the runner-selection plumbing (the lane matrix + `runs-on`). If the Velnor lane
fails on a step the GitHub lane passes, **the bug is in Velnor** — fix the Velnor
adapter or runtime so the identical config passes. This is the
"fixture-is-contract" rule applied to a real repo: ChainArgos's GitHub-green
workflow *is* the contract; Velnor must meet it.

**Behavior parity (must hold for every job):**

- Every step that runs on the GitHub lane runs on the Velnor lane, in the same
  order, with the same inputs/env.
- **Every test that passes on the GitHub runner passes on the velnor-runner** —
  same pass/fail set, same job conclusions, same outputs, same artifacts, same
  cache hit/miss semantics.
- Each step's result and output is verified equal between lanes — not just the
  final job conclusion.

**UI / API output parity (must hold, equal-or-better — never less):**

> **Known-broken at authoring time — treat as a real defect to fix, not an
> assumption.** Velnor's current GitHub UI output is observed to be broken /
> incompletely displayed / inaccurate vs the GitHub-hosted lane (e.g. missing or
> empty step logs, wrong/zero step numbers, epoch timestamps, absent synthetic
> steps, summaries not surfacing). Phase B must actively verify each field against
> the GitHub-hosted twin and **fix Velnor** until the ChainArgos lane renders at
> least as completely and accurately as GitHub. This is a first-class deliverable
> for ChainArgos, not optional polish.

- Compare the two lanes' **GitHub UI and REST output** for each job, not just
  the commands' results: step list + names + order, step numbers, per-step
  expandable logs, per-line timestamps, `::group::` sections, ANSI color,
  annotations, step summaries, "Set up job"/"Complete job" content, and final
  conclusions.
- Velnor must provide **at least as much information** as the GitHub-hosted lane —
  every step expandable with a real log, real timestamps, real durations, correct
  numbering. Missing/empty logs, epoch timestamps, or absent synthetic steps are
  defects.
- Per Velnor's own mission, Velnor **authors its own log stream** and should be
  *nicer and more informative* than GitHub where it can (cache hit/miss + bytes,
  sccache stats, parallelism used, time saved) — but **never less**. This goal
  must not regress Velnor's mission; it advances it on a real repo. See
  [`../docs/comparison.md`](../docs/comparison.md) and the
  [github-ui-parity](github-ui-parity.md) prompt.

**Performance parity (must hold, verified per step):**

- The velnor-runner must be **no slower than** the GitHub-hosted runner — equal
  or faster, never slower. Per the mission, Velnor should beat GitHub on
  wall-clock (warm caches, native adapters, no queue wait); the floor is
  "not slower".
- Measure and compare **per step and per job**: queue→first-log latency, each
  step's duration, total job runtime, warm-cache rebuild time, cache/sccache
  hit rates. Capture both lanes' timings and diff them.
- If any step is slower on Velnor, treat it as a defect: find the cause
  (cold cache, adapter overhead, container startup, serialization) and fix it in
  Velnor until Velnor is ≤ GitHub for that step.

Deliver a detailed, per-action/per-feature plan (in the checklist) of how Velnor
satisfies each thing ChainArgos's workflows do, close every gap in Velnor, and
prove behavior + performance parity step by step with recorded evidence.

## Why

This is the strongest possible Phase 0 proof: a real, non-trivial Rust monorepo
whose own CI is green on stock GitHub-hosted runners, then shown to be **equally
green on Velnor with zero job changes**, both lanes side by side with comparison
evidence (logs, outputs, conclusions, timing, cache) per
[`../docs/comparison.md`](../docs/comparison.md). Phase A also gives ChainArgos a
portable, vendor-neutral CI baseline independent of the Hetzner self-hosted box.

## Ground truth

- Self-hosted assumptions to unwind (Phase A): preinstalled `mise` + shims on
  `$HOME/.local/share/mise/shims`, host sccache at `/var/cache/sccache`, host
  `mold`, host-level Docker Hub credentials.
- Dual-lane reference (Phase B): fixture `compat.yml` `matrix-setup` →
  `config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}`,
  `runs-on: ${{ fromJSON(matrix.config.runner) }}`; github lane `"ubuntu-latest"`,
  velnor lane `["self-hosted","velnor-target-mvp"]`.
- Velnor protocol/adapter truth: <https://github.com/actions/runner>,
  [`../docs/native-action-adapter-contract.md`](../docs/native-action-adapter-contract.md),
  [`../docs/reference/target-action-registry.md`](../docs/reference/target-action-registry.md).
- Action families ChainArgos uses: `actions/checkout`, `actions/cache`,
  `dorny/paths-filter`, `jdx/mise-action`, `Swatinem/rust-cache`,
  `mozilla-actions/sccache-action` (to be introduced in Phase A),
  `rui314/setup-mold` (to be introduced in Phase A), `docker/login-action`,
  `docker/setup-buildx-action`, `docker/metadata-action`,
  `docker/build-push-action`, `docker/bake-action`, `renovatebot/github-action`,
  plus mise-managed tools (`oracle-graalvm` Java, Node, Python, `just`,
  `cargo-nextest`, `protoc`, `cargo:rust-script`).

## Authentication

- **GitHub** — the `gh` token has `repo` + `workflow` scopes (push/PR/merge OK).
  Sufficient for all of Phase A.
- **Docker Hub** — **satisfied.** `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` exist
  as **org-level** secrets on `ChainArgos` (visibility: all repositories),
  alongside `GH_READONLY_TOKEN`, `GITLEAKS_LICENSE`, `RENOVATE_TOKEN`. They
  resolve at runtime on `ChainArgos/java-monorepo`. (A `repo`-scoped
  `gh secret list --repo …` shows only repo secrets, not org ones — do not
  mistake that for "absent".) No secret needs adding for Phase A.
- **Velnor runner host (Phase B only)** — the Velnor daemon must run on the host
  that registers the velnor-lane JIT runner. This is the **one** item that needs
  the operator: the exact connect command (e.g. `ssh sentry`) and the intended
  GitHub token for JIT registration. Phase B is gated on Velnor being
  fixture-green regardless, so this is not needed to start Phase A.

## In scope

- **Phase A:** all 7 ChainArgos workflows repointed to GitHub-hosted runners and
  green (or provably neutral, for the callerless orphan `rust-docker-build.yml`).
- **Phase B dual-lane targets, in priority order:** `rust.yml` is the **primary**
  dual-lane target (fmt/clippy/per-crate nextest — the core Rust CI). `rust-docker.yml`
  is **secondary** (Buildx/Bake). `ansible.yml`, `kestra-*`, and `renovate.yml`
  are **optional** dual-lane targets, added only after the primary/secondary lanes
  hold parity. (The executing agent decides how far to extend Phase B based on
  parity results; `rust.yml` parity is the minimum bar.)

## Out of scope

- macOS / Windows runner labels (ChainArgos CI is Linux-only; Velnor rejects
  macOS).
- Rewriting ChainArgos workflow *logic* to suit Velnor (forbidden — Velnor adapts).
- Executing marketplace JS/TS as Velnor's product path (adapters stay native).

## Definition of done

**Phase A**

- Every workflow targets a GitHub-hosted runner; no `hetzner-sentry-ci` remains.
- On a PR, every triggered workflow is green: `rust` (fmt, clippy, all `test-*`,
  `rust-required`), `rust-docker` (`docker-bake`, `docker-required`), `ansible`,
  `kestra-build-publish` (when its paths change), `renovate` unaffected.
- `rust-docker-build.yml` is repointed but, being callerless, is not expected to
  trigger — verified neutral (no `hetzner-sentry-ci`, not red).
- Real project breakage surfaced by the move is fixed correctly (no skipped
  tests, no `-A` blanket lint allows, no deleted assertions).
- Changes merged to `main`; a green post-merge run exists. Evidence (run URLs)
  recorded.

**Phase B**

- A lane matrix runs each workflow on `ubuntu-latest` **and** the velnor label in
  parallel, with identical steps (only `runs-on` plumbing differs).
- The velnor lane reaches the same conclusion as the github lane on **every job
  and every step**, with **zero** job-logic edits.
- **Every test passes on the velnor-runner exactly as it does on GitHub** — same
  pass set, outputs, artifacts, cache behavior.
- **Velnor is no slower than GitHub on every step and job** (equal or faster),
  proven with recorded per-step/per-job timing for both lanes.
- **Velnor's GitHub UI + API output is equal-or-better, never less detailed**
  than the GitHub lane: every step expandable with a real log, per-line
  timestamps, grouping, ANSI, annotations, summaries, correct numbering, and
  faithful "Set up job"/"Complete job" — verified field-by-field via the API.
- Every Velnor gap found (behavior, performance, or output) is fixed in Velnor
  (adapter/runtime), with a test.
- Comparison evidence captured (logs, outputs, conclusions, per-step timing,
  cache/sccache hit rates, UI/API field diff) per `docs/comparison.md`.
- `cargo fmt --check` and `cargo test -q` pass in the Velnor repo.

## Work through

➡ **[chainargos-runner-parity.checklist.md](chainargos-runner-parity.checklist.md)**
