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
