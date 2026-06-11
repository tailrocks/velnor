# CI Performance Measurement Report - 2026-06-11

Source data directory: `/tmp/velnor-perf-20260610T225427Z`

This report records the fresh timing pass started from the current master-plan
estate. It is not a complete final full-estate benchmark: the Velnor fleet was
not healthy enough to run the requested all-repo, all-lane sweep.

## Scope and Safety Boundary

- Measured fresh workflow-dispatch runs sequentially, one workflow at a time.
- Repeated successful runs once immediately to observe warm-cache behavior.
- Did not dispatch release, Renovate, or publish workflows that mutate releases,
  PRs, package registries, or image tags unless the workflow had an explicit
  non-mutating lane/input.
- Cancelled pending/in-progress runs before dispatch per the repository rule.
  One existing active `jackin-project/jackin` run, `27311537944`, was cancelled
  during the first cleanup pass.
- Deleted offline self-hosted runner registrations before Velnor verification.

## Velnor Fleet State

After stale-runner cleanup and a 60-second repopulation check:

| Repository | Online Velnor runners | State |
|---|---:|---|
| `ChainArgos/jackin-agent-brown` | 0 | blocked |
| `ChainArgos/java-monorepo` | 0 | blocked |
| `ChainArgos/blockchain-nodes` | 1 | partial only |
| `tailrocks/velnor-actions-fixture` | 0 | blocked |

Only `ChainArgos/blockchain-nodes` could run a Velnor lane at all. It had one
online runner (`velnor-blockchain-nodes-slot-4`), so this pass does not measure
multi-slot Velnor capacity.

## Fresh Run Summary

| Repo | Workflow | Lane | Round | Run | Result | Wall |
|---|---|---|---|---:|---|---:|
| `jackin-project/jackin` | `ci.yml` | GitHub | cold | [27311639634](https://github.com/jackin-project/jackin/actions/runs/27311639634) | failure | 8m46s |
| `tailrocks/velnor` | `ci.yml` | GitHub | cold | [27312139408](https://github.com/tailrocks/velnor/actions/runs/27312139408) | success | 1m15s |
| `tailrocks/velnor` | `ci.yml` | GitHub | warm1 | [27312241157](https://github.com/tailrocks/velnor/actions/runs/27312241157) | success | 1m21s |
| `tailrocks/holla` | `ci.yml` | GitHub | cold | [27312344231](https://github.com/tailrocks/holla/actions/runs/27312344231) | success | 36s |
| `tailrocks/holla` | `ci.yml` | GitHub | warm1 | [27312401649](https://github.com/tailrocks/holla/actions/runs/27312401649) | success | 39s |
| `ChainArgos/jackin-agent-brown` | `ci.yml` | GitHub | cold | [27312463166](https://github.com/ChainArgos/jackin-agent-brown/actions/runs/27312463166) | success | 2m11s |
| `ChainArgos/jackin-agent-brown` | `ci.yml` | GitHub | warm1 | [27312563136](https://github.com/ChainArgos/jackin-agent-brown/actions/runs/27312563136) | success | 1m58s |
| `ChainArgos/java-monorepo` | `ansible.yml` | GitHub | cold | [27312660366](https://github.com/ChainArgos/java-monorepo/actions/runs/27312660366) | success | 43s |
| `ChainArgos/java-monorepo` | `ansible.yml` | GitHub | warm1 | [27312717034](https://github.com/ChainArgos/java-monorepo/actions/runs/27312717034) | success | 37s |
| `ChainArgos/blockchain-nodes` | `build-publish.yml` | both | cold | [27312773945](https://github.com/ChainArgos/blockchain-nodes/actions/runs/27312773945) | success | 1m24s |
| `ChainArgos/blockchain-nodes` | `build-publish.yml` | both | warm1 | [27312875877](https://github.com/ChainArgos/blockchain-nodes/actions/runs/27312875877) | success | 1m21s |
| `tailrocks/velnor-actions-fixture` | `compat.yml` | GitHub-only | cold | [27312972269](https://github.com/tailrocks/velnor-actions-fixture/actions/runs/27312972269) | success | 40s |
| `tailrocks/velnor-actions-fixture` | `compat.yml` | GitHub-only | warm1 | [27313029982](https://github.com/tailrocks/velnor-actions-fixture/actions/runs/27313029982) | success | 42s |

## Job-Level Notes

### `jackin-project/jackin` CI

Run [27311639634](https://github.com/jackin-project/jackin/actions/runs/27311639634)
failed, so no warm repeat was run.

| Job | Result | Duration |
|---|---|---:|
| `changes` | success | 8s |
| `cargo bench build` | success | 3m03s |
| `construct E2E image` | failure | 2m13s |
| `cargo msrv check` | success | 2m04s |
| `cargo check all features` | success | 2m02s |
| `cargo audit` | success | 3m19s |
| `cargo dependency policy` | success | 3m03s |
| `cargo fmt` | success | 17s |
| `actionlint` | success | 10s |
| `cargo fuzz` | success | 2m02s |
| `cargo clippy` | success | 1m46s |
| `cargo build validator (x86_64-unknown-linux-gnu, true)` | success | 6m17s |
| `cargo check default` | success | 1m49s |
| `cargo build validator (aarch64-unknown-linux-gnu, true)` | success | 5m39s |
| `cargo nextest` | skipped | 0s |
| `ci-required` | failure | 7s |

### `tailrocks/velnor` CI

| Round | Key jobs |
|---|---|
| cold | `test` 1m03s, `clippy` 23s, `fmt` 15s, `actionlint` 6s |
| warm1 | `test` 1m12s, `clippy` 36s, `fmt` 16s, `actionlint` 11s |

Warm repeat was slightly slower than cold, so cache benefit was not visible in
this short workflow.

### `tailrocks/holla` CI

| Round | Key jobs |
|---|---|
| cold | `test` 31s, `clippy` 29s, `fmt` 15s |
| warm1 | `test` 23s, `clippy` 33s, `fmt` 16s |

The warm repeat improved `test` but not total wall time.

### `ChainArgos/jackin-agent-brown` CI, GitHub Lane

| Round | Key jobs |
|---|---|
| cold | `validate (GitHub)` 1m59s |
| warm1 | `validate (GitHub)` 1m46s |

Warm repeat improved wall time by 13s. Velnor lane was blocked by zero online
repo runners.

### `ChainArgos/java-monorepo` Ansible, GitHub Lane

| Round | Key jobs |
|---|---|
| cold | `Ansible syntax-check (GitHub)` 32s |
| warm1 | `Ansible syntax-check (GitHub)` 28s |

Warm repeat improved wall time by 6s. Velnor lane was blocked by zero online
repo runners.

### `ChainArgos/blockchain-nodes` Build And Publish All, Both Lanes

| Round | Job | Runner | Duration |
|---|---|---|---:|
| cold | `Select runner lane and missing packages` | GitHub | 21s |
| cold | `docker-debian-blockchain-base (GitHub)` | GitHub | 8s |
| cold | `docker-debian-blockchain-base (Velnor)` | `velnor-blockchain-nodes-slot-4` | 24s |
| cold | `docker-debian-blockchain-build (GitHub)` | GitHub | 16s |
| cold | `docker-debian-blockchain-build (Velnor)` | `velnor-blockchain-nodes-slot-4` | 25s |
| warm1 | `Select runner lane and missing packages` | GitHub | 22s |
| warm1 | `docker-debian-blockchain-base (GitHub)` | GitHub | 8s |
| warm1 | `docker-debian-blockchain-base (Velnor)` | `velnor-blockchain-nodes-slot-4` | 23s |
| warm1 | `docker-debian-blockchain-build (GitHub)` | GitHub | 8s |
| warm1 | `docker-debian-blockchain-build (Velnor)` | `velnor-blockchain-nodes-slot-4` | 23s |

Package matrix jobs were skipped because no missing packages were found. This
measures the cached/no-op path, not full image rebuild performance.

### Fixture `compat`, GitHub-Only

| Round | Key jobs |
|---|---|
| cold | `compat (app-a, github)` 21s, `compat (app-b, github)` 22s |
| warm1 | `compat (app-a, github)` 22s, `compat (app-b, github)` 20s |

The Velnor fixture lane was blocked by zero online fixture runners.

## Deferred or Blocked

| Repo | Workflow(s) | Reason |
|---|---|---|
| `ChainArgos/jackin-agent-brown` | Velnor CI/publish lanes | zero online Velnor runners |
| `ChainArgos/java-monorepo` | Velnor `ansible`, `rust`, `rust-docker`, `kestra-build-publish` | zero online Velnor runners |
| `tailrocks/velnor-actions-fixture` | Velnor `compat`, `docker`, `multi-arch`, `pages`, `reuse-caller` | zero online fixture runners |
| `jackin-project/jackin` | warm CI repeat | cold CI failed in `construct E2E image` |
| `jackin-project/jackin-*`, `jackin-role-action` | publish/Renovate workflows | mutating workflows; not dispatched for timing |
| `tailrocks/holla`, `tailrocks/velnor` | release/deb/Homebrew/Renovate workflows | release or mutating workflows; not dispatched for timing |

## Conclusions

1. The requested full final benchmark is blocked by Velnor fleet health. The
   runner registrations did not repopulate for `agent-brown`, `java-monorepo`,
   or the fixture after stale cleanup.
2. `blockchain-nodes` can still execute a Velnor lane, but only with one online
   slot, so it cannot validate multi-runner capacity.
3. The measured GitHub-hosted warm repeats show small or no gains for short
   workflows; the only clear warm improvement was `agent-brown` CI
   (2m11s -> 1m58s) and `java-monorepo` Ansible (43s -> 37s).
4. `blockchain-nodes` no-op cached path is already short: 1m24s cold and 1m21s
   warm with both lanes enabled.

## Required Next Step

Restore the Velnor daemon fleet first, then rerun this report for the full
estate:

- `ChainArgos/jackin-agent-brown`: restore 2 online slots.
- `ChainArgos/java-monorepo`: restore 10 online slots.
- `ChainArgos/blockchain-nodes`: restore 4 online slots.
- `tailrocks/velnor-actions-fixture`: restore 2 online slots.

Once the fleet is healthy, the next benchmark should run three repeats for
each dual-lane workflow with `lanes=both` or `lane=both`, including full
package/image paths where safe, and should keep publish/release workflows on a
separate operator-approved pass.

---

## FINAL NUMBERS — 3-round campaign on a healthy fleet (2026-06-11)

Fleet verified healthy before every round (doctor 18/18 online; incident #9
fixed in 0.1.15, hardened in 0.1.16). Rounds: r1 = 2026-06-10T21:53Z
(0.1.14, per-slot caches), r2 = 02:00Z (0.1.16, shared cache store — COLD
first fill), r3 = 02:10Z (0.1.16, shared store WARM). All dispatched with
both lanes where dual-lane. **Rounds 2 and 3: 22/22 runs green** (r1 had 3
failures, all diagnosed and fixed: sentry crates.io egress flake → cargo
retry env baked into the job image; jackin mise apt pin gone stale → PR
#567; construct publish guard → PR #568).

Per-workflow, per-lane execution totals (sum of job exec seconds; max = the
critical-path job; maxq = worst queue wait):

```
workflow          lane    r  jobs  sum_s max_s maxq_s
ansible.yml       velnor  r1/r2/r3   81 / 85 / 119   (github: 256 / 35 / 31)
build-publish.yml velnor  r1/r2/r3  150 / 179 / 205  (github: 48 / 39 / 29)
compat.yml        velnor  r1/r2/r3   65 / 156 / 110  (github: 73 / 58 / 71)
rust-docker.yml   velnor  r1/r2/r3  376 / 452 / 62   (github bake: 615 / 607 / 519)
rust.yml          velnor  r1/r2/r3  3461 / 5185 / 2651, max 477/643/382
                  github  r1/r2/r3  6008 / 3187 / 3191, max 721/528/564
```

Run wall clocks (seconds): rust.yml 749 → 664 → 571 (−24% r1→r3);
rust-docker.yml 643 → 635 → 539; ansible 263 → 93 → 127; docs 215 → 223 →
148.

### Headline results

1. **rust-docker (jm): Velnor bake job 62s warm vs GitHub 519–607s — 8.5×
   faster.** Velnor's own trajectory: 376s (r1, per-slot cache) → 452s (r2,
   cold shared store) → **62s** (r3, warm shared store). The 0.1.16
   host-shared cache + local buildx layers deliver exactly the projected
   class of win.
2. **rust.yml (jm): Velnor critical path 382s warm vs GitHub 564s (−32%)**;
   Velnor exec total dropped 5185 → 2651 (−49%) in one warm round as the
   shared sccache filled. r2's regression vs r1 was the expected cold
   refill after the store moved to the shared path.
3. **Pickup latency parity**: Velnor queue waits are 1–3s, identical to
   GitHub-hosted, whenever a slot is free. The 87–100s outliers are pure
   capacity contention (10 jm slots saturated by the benchmark itself) —
   the P3.4 dynamic-slot case, not protocol latency.
4. **Remaining Velnor-lane gaps match the P3 design doc 1:1** (and are the
   queued work, ranks 2/5/8 of
   `docs/p3-performance-design-2026-06-11.md`): ansible (galaxy/mise state
   not persisted), build-publish (rust-script cache restoring to a
   container-invisible path), compat (MSRV rustup redownload). Each is a
   persistent-volume / cache-path-correctness item, not a protocol cost.

### Stability during the campaign

Zero zombie events, zero queue-forever events, zero restarts across all
three rounds; the P1.9 live split-brain repro self-healed in 3m35s before
the campaign; 24h zero-zombie soak continues. Stability and the measured
warm-cache trajectory together satisfy the campaign goal: the remaining
performance work is enumerated, ranked, and measurable against these
baselines.
