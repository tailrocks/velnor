# Velnor rearchitecture plan

Status: investigation wave active. No implementation work package is accepted
until its invariant, proof, and independent review are recorded here.

## Branch and evidence identity

- Primary repository: `tailrocks/velnor`
- Integration branch: `perf/docker-rust-mbx`
- Starting/verified target SHA: `2858e92df0eb78df4f1a6fe2ad4cbf86f1d56355`
- Verifier repository: `tailrocks/velnor-actions-fixture`
- Verifier branch: `codex/verifier-completion-fixes`
- Starting/verified target SHA: `5c8b57aa64dcbfd8fe6b2f6edae625ae344fc496`
- Upstream branch heads were fetched before planning; neither target branch had
  advanced at that check.
- Shared-branch coordination: all edits originate in isolated worktrees;
  fetch and reconcile the remote target before every commit/push. Keep commits
  small, signed off with `git commit -s`, and include
  `Co-authored-by: Codex <codex@openai.com>`.

## Baseline

Environment: macOS arm64, Rust toolchain `1.98.0`, exact Velnor target SHA.

- `rtk cargo test --workspace --all-targets --no-fail-fast`: failed after
  1325 passing tests. Known failures:
  - `crates/velnor-runner/src/cache.rs:1668`,
    `cache::tests::reclaim_stops_at_target_and_skips_in_use_scope`: expected
    one reclaimed item, observed zero.
  - `crates/velnor-tools/src/audit_ci.rs:4118`,
    `audit_ci::tests::test_repos_get_distinct_atomic_directories_under_concurrency`:
    isolated repository initialization returned permission denied.
- Fixture `rtk cargo test --workspace --all-targets --no-fail-fast`: 20 tests,
  15 suites, all passed. This is only local verifier-unit evidence, not live
  dual-lane readiness.
- No end-to-end runner, Docker, differential, fault, soak, or comparative
  benchmark baseline has yet been accepted.

## Current architecture to revalidate

The branch is a Rust workspace containing model, protocol/client, CAS,
action-journal, runner, control/render/CLI, and tooling crates. The runner
still concentrates broad ownership in `executor.rs`, `runner.rs`,
`protocol.rs`, `docker_lease.rs`, and related execution modules. Docker job
execution uses CLI/subprocess paths and a lease/socket-proxy boundary. Docker
Rust jobs mount a persistent Mr. Boxington store by default; explicit sccache
is intended to be mutually exclusive. These are observations to verify, not
design commitments.

## Target architecture invariants

1. Every accepted job has one explicit lifecycle owner and a deterministic,
   observable state transition; invalid phase operations are rejected by types
   or narrow APIs.
2. Slot registration/polling, local claim, durable admission, capacity,
   preparation, execution, cancellation, publication, completion, teardown,
   and cleanup are distinct stages with bounded waits.
3. Completion is idempotent/replayable and crash-recoverable; stale process
   generations cannot publish as current owners.
4. Cancellation propagates through the current process tree, Docker/BuildKit,
   post actions, publication, and teardown with graceful escalation and no
   terminal orphan.
5. Docker resources have one owner, one label/lease identity, and one bounded
   cleanup contract. Shared persistent builders/caches have an explicit trust
   and generation model.
6. The default Rust path is ordinary Cargo with one acceleration architecture;
   explicit sccache and acceleration opt-out are separate deterministic modes.
7. Cache reuse is content/trust safe. Untrusted workloads cannot write state
   consumed by trusted workloads.
8. Velnor, Cargo, mbx, Docker, BuildKit, and services use one capacity model;
   independent subsystems do not silently oversubscribe or serialize jobs.
9. Every idle interval, retry, timeout, and resource wait has cheap structured
   evidence identifying its owner and reason.
10. Claimed GitHub semantics are source-matched to `actions/runner` and
    differentially verified; unsupported inputs fail early and explicitly.
11. Storage ownership and GC are bounded under a global host budget; disk
    pressure cannot create an indefinite admission wait.
12. Velnor runtime, verifier core, evidence normalization, and benchmark logic
    remain Rust; shell/YAML are thin fixture glue only.

## Initial investigation wave

Twenty-eight independent bounded investigations are required. Each must cite
exact paths, lines, symbols, upstream references where relevant, root cause,
invariant, priority, and proof design.

1. slot lifecycle — delegated
2. broker/run-service protocol — delegated
3. workflow semantic model — delegated
4. expressions, conditions, outcomes, conclusions — delegated
5. cancellation and timeouts — delegated
6. completion durability and crash recovery — delegated
7. Docker runtime architecture — delegated
8. Docker Engine API versus CLI — delegated
9. Docker lease/proxy/resource ownership — pending next wave
10. BuildKit/buildx lifecycle — pending next wave
11. Rust compilation architecture — pending next wave
12. Mr. Boxington — pending next wave
13. Cargo dependency/source persistence — pending next wave
14. filesystem/worktree/checkout performance — pending next wave
15. resource scheduling/concurrency — pending next wave
16. cache architecture/trust — pending next wave
17. disk ownership/GC — pending next wave
18. action resolution and JS/Docker/composite execution — pending next wave
19. artifacts/cache/results/timeline/log publishing — pending next wave
20. async/threading/blocking I/O — pending next wave
21. crate/module/API organization — pending next wave
22. dependency/API freshness — pending next wave
23. security and cross-job isolation — pending next wave
24. observability/performance instrumentation — pending next wave
25. fixture coverage architecture — pending next wave
26. benchmark methodology — pending next wave
27. fault injection and soak testing — pending next wave
28. production operability — pending next wave

After this wave: a fresh architecture synthesizer must inspect all reports and
source; a separate red-team review must challenge serialization, invalid state,
cache poisoning, lost completion, leaks, unsupported semantics, unnecessary
abstraction, and performance regressions before broad implementation.

## Work-package template

Each accepted package records:

- ID and priority (P0/P1/P2/P3)
- observable defect and root architectural cause
- desired invariant and ownership boundary
- upstream semantic reference, if applicable
- affected modules/crates and disjoint write set
- dependencies and forbidden compatibility shims
- tests, fault cases, and differential proof
- benchmark hypothesis, baseline, after result, environment, and sample method
- Sonnet implementation owner, independent Opus review, corrections
- implementation commit(s), final status, and remaining blockers

## Dependency graph and status

- P0-A: baseline failures + exact branch/evidence identity → active.
- P0-B: semantic/cancellation/completion/trust investigations → blocked on
  reports, then architecture synthesis.
- P1-A: Docker lifecycle/API/ownership/BuildKit → blocked on reports and
  target architecture.
- P1-B: Rust/mbx/Cargo/checkout/resource scheduling → blocked on reports and
  target architecture.
- P1-C: fixture capability/evidence/differential redesign → blocked on exact
  Velnor capability model.
- P2-A: storage/GC/disk pressure and observability → depends on ownership model.
- P2-B: module decomposition → only after ownership/state boundaries are
  proven; no cosmetic file splitting.
- P3: documentation and small optimizations → after P0/P1/P2 proof.

## Required proof gates

Before final completion, rerun exact-commit source and image identity checks;
format, Clippy with warnings denied, workspace/integration/Docker tests;
default mbx, explicit sccache, and opt-out Rust lanes; action/semantic,
cancellation, fault, soak, and verifier suites; and reproducible end-to-end
benchmarks. Record image digest, Velnor and fixture commits, environment,
sample counts, p50/p95/p99 where valid, process/API counts, cache/disk bytes,
and official `actions/runner` comparison. No stale or single-lane evidence is
readiness proof.

## Commits and review log

- Plan bootstrap: pending commit/push after remote reconciliation.
- No code package accepted yet.
- Review findings: pending initial reports.
- Remaining blockers: investigation reports, upstream release/source audit,
  exact image/live fixture access, and baseline fault/benchmark evidence.
