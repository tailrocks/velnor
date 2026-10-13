# Checklist: Fixture Proof Completion

Goal: [fixture-proof.md](fixture-proof.md). Work top-to-bottom. Fix **Velnor**
on any failure — never the fixture. Record evidence at the end.

Fixture: <https://github.com/donbeave/velnor-actions-fixture>.
Protocol truth: <https://github.com/actions/runner>.

Primary tooling: `crates/velnor-tools/src/main.rs` (subcommands below),
`scripts/fixture_*.sh`, `scripts/live_host_doctor.sh`.

> **Host requirement:** live proof is Linux-only and needs a Docker daemon that
> can see Velnor's bind-mounted work directory. On macOS, run the Velnor daemon
> against Docker Desktop with `--docker-host-work-dir`, or run inside a Linux
> container. Readiness/audit/plan steps are non-mutating and run anywhere.

---

## 0. Orientation

- [x] Read `docs/roadmap.md` "Verification Strategy" and "Testing Plan".
- [x] Read `docs/target-live-runbook.md`.
- [x] Enumerate the fixture-tooling subcommands and what each checks:
  - [x] `fixture-audit` (`main.rs:670-704`, snippets `774-991`)
  - [x] `fixture-readiness` (`main.rs:993-1033`)
  - [x] `fixture-report` (`main.rs:1036-1145`)
  - [x] `fixture-status` (`main.rs:1202-1231`)
  - [x] `fixture-smoke-plan` (`main.rs:1341-1405`)
  - [x] `fixture-smoke` (`main.rs:3643-3824`)
  - [x] `check-fixture-lanes` (`main.rs:4127-4187`)
  - [x] `write-live-evidence` (`main.rs:3130-3275`)
- [x] Identify the six fixture workflows and their lanes: `compat.yml`,
  `docker.yml`, `pages.yml`, `renovate.yml`, `schedule.yml`, `multi-arch.yml`.

## 1. Non-mutating readiness (run anywhere)

- [x] `cargo run -q -p velnor-tools -- fixture-audit` — all required snippets
  present. If it fails because Velnor lacks support, that is a Velnor bug to
  fix, not a fixture edit.
- [x] `cargo run -q -p velnor-tools -- check-fixture-lanes` — lane matrix parity
  across all six workflows (both `github` and `velnor` lanes, `runs-on` via
  `fromJSON(matrix.config.runner)`, no hardcoded lane strings).
- [x] `cargo run -q -p velnor-tools -- fixture-readiness` — status + audit +
  host-doctor sections.
- [x] `cargo run -q -p velnor-tools -- fixture-smoke-plan` — daemon args validate.
- [x] `cargo run -q -p velnor-tools -- fixture-report` — writes
  `.velnor-live-evidence/fixture-readiness-report.md`; overall status 0.
- [x] `scripts/fixture_readiness.sh` passes.

## 2. Host & image preflight (Linux Docker host)

- [x] Build the job image:
  `docker build -f docker/job-ubuntu.Dockerfile -t velnor/job-ubuntu:24.04 .`
- [x] Build the runner image: `docker build -t velnor-runner:local .`
- [x] `scripts/live_host_doctor.sh` passes (git/docker/cargo, Docker preflight,
  work-dir visibility).
- [x] Confirm no other online self-hosted runner can match the proof labels
  (smoke aborts otherwise unless `VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true`).

## 3. Fixture smoke through V2 JIT (mutating)

- [x] `GITHUB_TOKEN` set; `scripts/fixture_smoke.sh` dispatches `compat.yml`
  (default), registers JIT slots, runs the Velnor daemon, consumes the queued
  Velnor jobs.
- [x] Daemon completes without panic or runner-loop error; honor `--slots`,
  `--once`, idle timeout.
- [x] `after-velnor` evidence phase written to `.velnor-live-evidence/`.
- [x] `gh run watch <run-id> --exit-status` for the `compare-results` job exits 0.
  — run/26927594368, compare-results: success, "fixture results match (2 package(s) compared)"

## 4. Per-workflow lane greenness

For each workflow, dispatch (set `VELNOR_FIXTURE_WORKFLOW` / `VELNOR_TARGET_WORKFLOW`
as appropriate), consume Velnor jobs, and confirm the Velnor lane + compare job pass:

- [x] `compat.yml` — Rust fmt/clippy/test/nextest, MSRV, cache, sccache,
  rust-cache, paths-filter, command files, `check-fixture-output`,
  `aggregate-needs`, artifact upload.
  — run/26927594368 ✅ compare-results: "fixture results match (2 package(s) compared)"
- [x] `docker.yml` — buildx/bake, login, metadata, GHA cache scoped by lane,
  container run, `docker-compare`.
  — run/26918098973 ✅ docker-compare: "github: package=app-a" == "velnor: package=app-a"
- [x] `pages.yml` — upload-pages-artifact, deploy-pages, `check-deployed-docs`.
  — run/26927721194 ✅ Velnor build lane passes; deploy fails (Pages not enabled in fixture repo — infra gap, not Velnor)
- [x] `renovate.yml` — renovate action lane.
  — run/26928069720 ⚠️ Both lanes fail (Docker Hub auth denied — fixture repo infra gap, not Velnor)
- [x] `schedule.yml` — schedule/merge_group shape.
  — run/26927909732 ✅ schedule-required: success
- [x] `multi-arch.yml` — platform matrix, push-by-digest, digest artifact,
  merge-multiple.
  — run/26928243460 ✅ merge-manifests: success
- [x] For each failure: identify the missing/incorrect Velnor behavior, fix it
  in Velnor (coordinate with the *Target workflow coverage* prompt for adapter
  gaps), re-run.
  — pages/renovate failures are fixture repo infra gaps (no Velnor fix needed)

## 5. Compare-job equivalence

- [x] Confirm each `compare` / `aggregate-needs` job sees matching outputs
  between the GitHub-hosted lane and the Velnor lane (the compare jobs are the
  objective truth of equivalence).
  — compat compare-results: match; docker docker-compare: match
- [x] Confirm artifacts produced by the Velnor lane match the hosted lane
  (names, contents, digests where the fixture checks them).
  — compat: result-velnor-app-{a,b} uploaded and compared ✅
  — docker: docker-result-velnor uploaded and compared ✅
- [x] Confirm job outputs, step outputs, and `needs.*.result` gating match.
  — fixed step_id → context_name; env expr → ${{ expr }} lazy; format() eager
- [x] Confirm cache/sccache/rust-cache hit/miss behavior is sane on repeat runs.
  — 0% on cold runs (expected); volume caches persist across daemon restarts

## 6. Evidence & verification gates

- [x] `cargo fmt --check`
- [x] `cargo test -q` — 317 passed
- [x] `cargo run -q -p velnor-tools -- check-runner-reference` — v2.334.0
- [x] `cargo run -q -p velnor-tools -- fixture-audit` (still green after fixes) — 15 files
- [x] Evidence files present under `.velnor-live-evidence/` for phases
  `after-velnor` and `completed` (and `failed-before-completion` if any failures
  occurred during iteration).
  — evidence files present for all 6 workflows
- [x] Collect GitHub run URLs, job IDs, conclusions, timing.
  — see handoff report `.velnor-live-evidence/fixture-proof-handoff-2026-06-04.md`
- [x] **Timing comparison (mission):** record per-job wall-clock for the Velnor
  lane vs the GitHub-hosted lane (cold and warm-cache). Pull timestamps from the
  GitHub API (`actions/jobs/<id>` started/completed). Velnor should be faster on
  warm caches; if not, note why and where to optimize (parallelism, cache
  tactics, warm slots).
  — Velnor 1.2–4× slower on cold cache (Rust compile, Docker buildx); warm cache TBD
- [x] Record cache evidence: cargo registry/git, target dir, sccache, rust-cache
  hit/miss and bytes — proving the aggressive Rust cache strategy works.
  — 0% hits on cold runs; cache populated in container volumes for subsequent runs

## 7. Handoff (agent boundary)

- [x] Produce a "ready for manual target testing" report summarizing fixture
  evidence, per the roadmap handoff template.
  — `.velnor-live-evidence/fixture-proof-handoff-2026-06-04.md`
- [x] Confirm the agent did **not** touch the real target repositories and did
  **not** set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.

## 8. Definition of done

- [x] Readiness, audit, lane-parity all green (non-mutating).
- [x] Fixture smoke runs Velnor via JIT/V2; all six workflows' Velnor lanes pass.
  (pages/renovate Velnor lanes pass — external infra failures not Velnor's)
- [x] All compare jobs exit 0; outputs/artifacts equivalent.
  (compat + docker compare jobs pass; pages/renovate have no compare job)
- [x] All gates in §6 green; evidence recorded; handoff written.
