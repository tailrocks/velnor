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

- [ ] Read `docs/roadmap.md` "Verification Strategy" and "Testing Plan".
- [ ] Read `docs/target-live-runbook.md`.
- [ ] Enumerate the fixture-tooling subcommands and what each checks:
  - [ ] `fixture-audit` (`main.rs:670-704`, snippets `774-991`)
  - [ ] `fixture-readiness` (`main.rs:993-1033`)
  - [ ] `fixture-report` (`main.rs:1036-1145`)
  - [ ] `fixture-status` (`main.rs:1202-1231`)
  - [ ] `fixture-smoke-plan` (`main.rs:1341-1405`)
  - [ ] `fixture-smoke` (`main.rs:3643-3824`)
  - [ ] `check-fixture-lanes` (`main.rs:4127-4187`)
  - [ ] `write-live-evidence` (`main.rs:3130-3275`)
- [ ] Identify the six fixture workflows and their lanes: `compat.yml`,
  `docker.yml`, `pages.yml`, `renovate.yml`, `schedule.yml`, `multi-arch.yml`.

## 1. Non-mutating readiness (run anywhere)

- [ ] `cargo run -q -p velnor-tools -- fixture-audit` — all required snippets
  present. If it fails because Velnor lacks support, that is a Velnor bug to
  fix, not a fixture edit.
- [ ] `cargo run -q -p velnor-tools -- check-fixture-lanes` — lane matrix parity
  across all six workflows (both `github` and `velnor` lanes, `runs-on` via
  `fromJSON(matrix.config.runner)`, no hardcoded lane strings).
- [ ] `cargo run -q -p velnor-tools -- fixture-readiness` — status + audit +
  host-doctor sections.
- [ ] `cargo run -q -p velnor-tools -- fixture-smoke-plan` — daemon args validate.
- [ ] `cargo run -q -p velnor-tools -- fixture-report` — writes
  `.velnor-live-evidence/fixture-readiness-report.md`; overall status 0.
- [ ] `scripts/fixture_readiness.sh` passes.

## 2. Host & image preflight (Linux Docker host)

- [ ] Build the job image:
  `docker build -f docker/job-ubuntu.Dockerfile -t velnor/job-ubuntu:24.04 .`
- [ ] Build the runner image: `docker build -t velnor-runner:local .`
- [ ] `scripts/live_host_doctor.sh` passes (git/docker/cargo, Docker preflight,
  work-dir visibility).
- [ ] Confirm no other online self-hosted runner can match the proof labels
  (smoke aborts otherwise unless `VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true`).

## 3. Fixture smoke through V2 JIT (mutating)

- [ ] `GITHUB_TOKEN` set; `scripts/fixture_smoke.sh` dispatches `compat.yml`
  (default), registers JIT slots, runs the Velnor daemon, consumes the queued
  Velnor jobs.
- [ ] Daemon completes without panic or runner-loop error; honor `--slots`,
  `--once`, idle timeout.
- [ ] `after-velnor` evidence phase written to `.velnor-live-evidence/`.
- [ ] `gh run watch <run-id> --exit-status` for the `compare-results` job exits 0.

## 4. Per-workflow lane greenness

For each workflow, dispatch (set `VELNOR_FIXTURE_WORKFLOW` / `VELNOR_TARGET_WORKFLOW`
as appropriate), consume Velnor jobs, and confirm the Velnor lane + compare job pass:

- [ ] `compat.yml` — Rust fmt/clippy/test/nextest, MSRV, cache, sccache,
  rust-cache, paths-filter, command files, `check-fixture-output`,
  `aggregate-needs`, artifact upload.
- [ ] `docker.yml` — buildx/bake, login, metadata, GHA cache scoped by lane,
  container run, `docker-compare`.
- [ ] `pages.yml` — upload-pages-artifact, deploy-pages, `check-deployed-docs`.
- [ ] `renovate.yml` — renovate action lane.
- [ ] `schedule.yml` — schedule/merge_group shape.
- [ ] `multi-arch.yml` — platform matrix, push-by-digest, digest artifact,
  merge-multiple.
- [ ] For each failure: identify the missing/incorrect Velnor behavior, fix it
  in Velnor (coordinate with the *Target workflow coverage* prompt for adapter
  gaps), re-run.

## 5. Compare-job equivalence

- [ ] Confirm each `compare` / `aggregate-needs` job sees matching outputs
  between the GitHub-hosted lane and the Velnor lane (the compare jobs are the
  objective truth of equivalence).
- [ ] Confirm artifacts produced by the Velnor lane match the hosted lane
  (names, contents, digests where the fixture checks them).
- [ ] Confirm job outputs, step outputs, and `needs.*.result` gating match.
- [ ] Confirm cache/sccache/rust-cache hit/miss behavior is sane on repeat runs.

## 6. Evidence & verification gates

- [ ] `cargo fmt --check`
- [ ] `cargo test -q`
- [ ] `cargo run -q -p velnor-tools -- check-runner-reference`
- [ ] `cargo run -q -p velnor-tools -- fixture-audit` (still green after fixes)
- [ ] Evidence files present under `.velnor-live-evidence/` for phases
  `after-velnor` and `completed` (and `failed-before-completion` if any failures
  occurred during iteration).
- [ ] Collect GitHub run URLs, job IDs, conclusions, timing.
- [ ] **Timing comparison (mission):** record per-job wall-clock for the Velnor
  lane vs the GitHub-hosted lane (cold and warm-cache). Pull timestamps from the
  GitHub API (`actions/jobs/<id>` started/completed). Velnor should be faster on
  warm caches; if not, note why and where to optimize (parallelism, cache
  tactics, warm slots).
- [ ] Record cache evidence: cargo registry/git, target dir, sccache, rust-cache
  hit/miss and bytes — proving the aggressive Rust cache strategy works.

## 7. Handoff (agent boundary)

- [ ] Produce a "ready for manual target testing" report summarizing fixture
  evidence, per the roadmap handoff template.
- [ ] Confirm the agent did **not** touch the real target repositories and did
  **not** set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.

## 8. Definition of done

- [ ] Readiness, audit, lane-parity all green (non-mutating).
- [ ] Fixture smoke runs Velnor via JIT/V2; all six workflows' Velnor lanes pass.
- [ ] All compare jobs exit 0; outputs/artifacts equivalent.
- [ ] All gates in §6 green; evidence recorded; handoff written.
