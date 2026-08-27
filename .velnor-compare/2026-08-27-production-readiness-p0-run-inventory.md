# Production-readiness P0 run inventory

Captured 2026-08-27 with `gh run list` and `gh run view`. This is an inventory,
not a verification dispatch; no runs or runners were mutated.

## Current non-green/stuck evidence

| repository | run | SHA | branch/event | state | job/step evidence |
|---|---:|---|---|---|---|
| `tailrocks/velnor` | [33010337075](https://github.com/tailrocks/velnor/actions/runs/33010337075) | `0ab33b6` | `fix/watchdog-registration-deadline` / PR | pending | no job started; pending since 2026-08-26T20:25:35Z |
| `tailrocks/velnor` | [33009178017](https://github.com/tailrocks/velnor/actions/runs/33009178017) | `35fe729` | `v0.1.217` / push | in_progress | Release run still active since 20:11:56Z |
| `tailrocks/velnor` | [33009178083](https://github.com/tailrocks/velnor/actions/runs/33009178083) | `35fe729` | `v0.1.217` / push | failure | `tailrocks / github lane` → `test gate`; aggregate lane verdict; `ci-required` |
| `tailrocks/velnor` | [32988760007](https://github.com/tailrocks/velnor/actions/runs/32988760007) | `d913679` | `ci/revalidate-gh-d913` / dispatch | failure | `tailrocks / github lane` → `lint gate`; aggregate lane verdict; `ci-required` |
| `ChainArgos/java-monorepo` | [33010149626](https://github.com/ChainArgos/java-monorepo/actions/runs/33010149626) | `cc3d17e` | `chore/merge-yolo-parity-pr-1974` / PR | in_progress | `Build and publish Docker images (Velnor)` in progress since 20:23:25Z |
| `jackin-project/jackin` | [32965105931](https://github.com/jackin-project/jackin/actions/runs/32965105931) | `4d43da4` | `main` / schedule | failure | `Criterion bench run`, `cargo-mutants pure crates`, `dylint render purity`, `Miri jackin-config`, `Miri jackin-term` |
| `jackin-project/jackin` | [32962624253](https://github.com/jackin-project/jackin/actions/runs/32962624253) | `dbbcbd9` | Renovate PR / PR | failure | `jackin-project / velnor lane` failed; aggregate lane verdict; `ci-required` |
| `jackin-project/jackin` | [32958445805](https://github.com/jackin-project/jackin/actions/runs/32958445805) | `cce015e` | Renovate PR / PR | failure | `validate-version-bump` → `Run ./.github/actions/cache-cargo-registry` |

## Cancellation/stuck frequency in recent window

Recent 30-run API listings on each repository were filtered for failed,
cancelled, or non-completed runs. Velnor shows repeated cancellation around
the same PR/dispatch attempts (including runs 33009154338, 33008471942,
33008369844, 33008022594, 32992379079, 32991375615, 32990561143,
32990547630, 32988554199, 32987576072, 32987475594, and 32987349382), plus
two currently non-completed runs above. ChainArgos shows repeated cancellation
of the same parity PR across CI, Rust Docker, and Ansible jobs (for example
33009766601/33009765838/33009765804 and earlier SHA attempts). Jackin shows
one queued Renovate run, two recent failed Renovate/scheduled runs, and one
failed Velnor-lane contract run in the captured window.

## Initial classification

- Velnor pending/in-progress and repeated cancellations: lifecycle/runner
  capacity or registration investigation required before any new dispatch.
- Velnor lane failures: workflow/test and workflow/lint failures; inspect logs
  before classifying runner protocol.
- ChainArgos long-running Docker publish: executor/container or registry
  lifecycle; do not call it successful while active.
- Jackin lane failure: Velnor lane/contract path; scheduled failures are
  repository-specific advisory gates until their first failing step is traced.

Next required action: before verification, cancel only prior non-completed
runs, remove only stale validation-owned runner registrations, prove clean
state, then dispatch once and monitor only the new run ID.
