# A1 live-status: CURRENT main + PR 912 CI state (tailrocks/velnor)

Collected: 2026-09-17 UTC via read-only `gh run list` / `gh api` / `gh run view --json jobs` + `--log-failed` greps.
Current main HEAD (live ref): `33688938297eee3933997dbbf368ee20fb1779d4`

## Latest 3 main runs (all = push @ 33688938, 2026-09-16T22:49:36Z)

| Run | Workflow | Conclusion | Head SHA | URL |
|-----|----------|------------|----------|-----|
| 35159519365 | CI / Main | success | 33688938 | https://github.com/tailrocks/velnor/actions/runs/35159519365 |
| 35159519217 | Preview | FAILURE | 33688938 | https://github.com/tailrocks/velnor/actions/runs/35159519217 |
| 35159518917 | Runtime products | success | 33688938 | https://github.com/tailrocks/velnor/actions/runs/35159518917 |

Per-workflow: `CI` workflow has ZERO runs on main. `CI / Main` on main (latest 5): success, failure, failure, failure, failure (see table). `Velnor workflow policy` on main: last runs 2026-09-16T05:25/03:21Z, both success (workflow_dispatch, stale).

## CURRENT live-failure table

| # | Run / scope | Workflow, head, time | Failing jobs | Failure signature |
|---|-------------|----------------------|--------------|-------------------|
| 1 | 35159519217 (CURRENT head 33688938) | Preview, push main, 22:49Z | Guest payload x86_64, Guest payload aarch64 (+ skipped downstream) | `EACCES: permission denied, scandir '.../dist/microvm/work/rootfs-tree/lib/ssl/private'` (Upload guest payload step, both arches). NEW vs A0 list. Main NOT fully green. |
| 2 | 35162700625 (PR 912 head 38ffbfd7, pull_request, 23:31Z) | CI / PR | Rust velnor-control / Velnor, velnor-bench / Velnor, velnor-tools / Velnor (+ ci-required, Required) | `Velnor rejected job (operational_store)` x3 — YES, recurred post-merge. All GitHub-lane jobs, Policy, Planning, DCO = SUCCESS. |
| 3 | 35156192655 (main 73bb3ff9, 22:09Z) | CI / Main | Bun / Velnor, Documentation / Velnor (+ ci-required, Required) | `Velnor rejected job (operational_store)` x2 — YES, recurred on main post-merge. |
| 4 | 35157891206 (main f16cc51a, 22:28Z) | CI / Main | Policy (+ ci-required, Required); Planning SUCCESS | `generated-tree` (8 files: generator-state, ci-main, ci-pr, ci-unit-bun/docker/docs/opentofu/rust; pin a70d5bdb) + `trusted-runners` 6 findings; 11 rules, 2 failed. Recurred, but on superseded main — NOT on current HEAD. |
| 5 | 35158060354 (main 386ccc16, 22:31Z) | CI / Main | Control / Planning, Policy (both fail-closed pre-rule) | `no runtime product for revision f16cc51a (closure 93c8a358...)` — Planning-race signature recurred (cascade: Publish 403 for f16cc51a, next push's pin unbuildable). |
| 6 | 35156585433 (main d714a96b, 22:13Z) | CI / Main | Control / Planning, Policy (both fail-closed pre-rule) | `no runtime product for revision a70d5bdb (closure fd45efac...)` — same race signature. |
| 7 | 35157890866 (main f16cc51a, 22:28Z) | Runtime products | Publish runtime products (builds all success) | `HTTP 403: Resource not accessible by integration (.../releases)`. Transient: later publishes (35158058879, 35159518917) succeeded. Root cause of #5's missing product. |

## A0-list comparison (verdicts)

- Policy generated-tree on CURRENT main? NO. Latest CI/Main @33688938 fully green (Policy passed). Signature DID recur same evening on f16cc51a (#4, 8 files + trusted-runners) — stale-tree class still live on intermediate mains, closed at HEAD.
- operational_store recur post-merge? YES — main #3 (Bun, Docs) and PR 912 latest #2 (control, bench, tools). Latest main green, latest PR red because of it. No `stale-rev` text in any recent log.
- stale-rev (`revision ... is not a commit`) recur? NO — zero hits across all recent failed logs grepped (runs #1–#7, both lanes).
- Planning race (`no runtime product ... builds it after merge`) recur? YES — #5, #6 on main, driven by #7's 403 Publish gap.
- NEW failure not in A0 list: Preview Guest-payload EACCES (#1) fails on CURRENT HEAD — CI/Main green but main-branch overall red.
