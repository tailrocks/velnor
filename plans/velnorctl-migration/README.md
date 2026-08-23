# Velnorctl migration plans

Generated from repository evidence at Velnor commit `35d5bb7` on 2026-08-24
and fixture commit `dc4204ca055c3138cf78d666b4dd1c5adfddc963`.

## Goal

Replace the complete `velnor-runner` product surface with `velnorctl`. Final
state has no `velnor-runner` crate, binary, Debian package, Docker entrypoint,
systemd invocation, active documentation, script call, release artifact, or
backward-compatible command alias.

Target ownership:

```text
velnorctl       operator CLI and service-only `daemon` entrypoint
velnor-model    versioned resources, conditions, events, and API DTOs
velnor-control  daemon state, operational store, services, and API server
velnor-client   Unix and later authenticated remote API clients
velnor-render   table, wide, JSON, YAML, JSONL, and name output
velnor-tools    maintainer/development utilities only
```

## Research corrections required by Velnor architecture

- Research retains `velnor-runner` as worker/systemd daemon. Requested terminal
  state forbids that. [C075](commands/C075-daemon.md) makes `velnorctl daemon`
  the service entrypoint; Plan 079 removes the old executable/package.
- Research suggests temporary `cache` compatibility aliases. User explicitly
  waived backward compatibility. Only `velnorctl storage ...` remains.
- Old worker `run` cannot coexist with GitHub workflow `velnorctl run ...`.
  Service-only `velnorctl daemon --once` replaces old single-job execution.
- Current doctor mutates stale jobs and queued runs. Plan 072 extracts those
  repairs; C021 is read-only and C023–C026 perform explicit reconciliation.
- `instance install` means materializing an already signed-apt-installed systemd
  instance. It never installs Velnor package or bypasses apt.
- The later operator decision removes the complete CLI release namespace and
  all custom release resources, state, activation, and rollback services.
  Debian `apt`/`dpkg` are the sole installed-version authority. Maintainer CI
  may still build and publish signed Debian packages, but Velnor exposes no
  package-version management command.
- Diagnostics archive path uses `--archive`, because global `-o/--output` already
  owns render format; the research's duplicate `--output` spelling is not a
  valid unambiguous Clap surface.

No other research command or behavior is removed. Full mapping and the five
operator-directed command removals live in [research coverage](RESEARCH_COVERAGE.md).

## Task structure

Command granularity is normative:

- [75 leaf-command tasks](commands/README.md): one task file per executable
  command, including its parser, handler, output, tests, and fixture validation.
- Plans 063–078: shared architecture/services only. They do not own CLI commands.
- Plan 079: final package/binary contraction after every command task is green.
- Plan 080: later authenticated remote transport and multi-host extension using
  the same command/API models.

## Shared architecture plans

| Plan | Title | Priority | Effort | Depends on | Status |
|---|---|---:|---:|---|---|
| [063](063-record-direction-and-fixture-contract.md) | Record direction and fixture control contract | P1 | M | — | TODO |
| [064](064-scaffold-workspace-and-cli.md) | Scaffold workspace and CLI seams | P1 | L | 063 | TODO |
| [065](065-resource-model-rendering-cli-conventions.md) | Define resources, rendering, and global conventions | P1 | L | 064 | TODO |
| [066](066-operational-history-and-events.md) | Persist sanitized operational history and events | P1 | L | 065 | TODO |
| [067](067-unix-control-api.md) | Serve versioned Unix-socket control API | P1 | L | 066 | TODO |
| [068](068-configuration-auth-instance-services.md) | Extract configuration, authentication, and instance services | P1 | L | 064–067 | TODO |
| [069](069-resource-query-and-description-services.md) | Build resource query and description services | P1 | L | 065–068, 074–075 | TODO |
| [070](070-log-access-services.md) | Build active and completed log services | P1 | L | 065–069, 074 | TODO |
| [071](071-observation-metrics-and-wait-services.md) | Build event, metrics, and wait services | P2 | L | 066, 067, 069, 075 | TODO |
| [072](072-health-preflight-reconciliation-services.md) | Separate health, preflight, and reconciliation services | P1 | L | 066–071, 074–075 | TODO |
| [073](073-daemon-lifecycle-engine.md) | Build daemon and lifecycle state engine | P1 | XL | 066–072 | TODO |
| [074](074-github-workflow-run-client.md) | Build canonical GitHub Actions client and run merge service | P1 | L | 065, 066, 068 | TODO |
| [075](075-storage-control-services.md) | Build storage control services | P1 | L | 066, 067 | TODO |
| [076](076-debian-native-package-management.md) | Prepare the Debian-native package transition | P1 | L | 064, 068 | TODO |
| [077](077-capability-adapter-workflow-services.md) | Build capability, adapter, and workflow-check services | P2 | L | 064–069 | TODO |
| [078](078-diagnostics-collection-service.md) | Build sanitized diagnostics collection service | P2 | M | 066–077 | TODO |
| [079](079-final-package-and-binary-cutover.md) | Cut over package and remove `velnor-runner` | P1 | XL | 063–078, C001–C075 | TODO |
| [080](080-remote-contexts-and-fleet-views.md) | Add authenticated remote transport and fleet views | P3 | L | 079 | TODO |

Status values: `TODO`, `IN PROGRESS`, `DONE`, `BLOCKED` with reason, or
`REJECTED` with rationale.

## Command coverage summary

| Family | Tasks | Count |
|---|---|---:|
| Utility | C001–C005 | 5 |
| `get` | C006–C015 | 10 |
| `describe`, `logs`, `events`, `top`, `wait` | C016–C020 | 5 |
| Health/preflight/reconcile | C021–C026 | 6 |
| Lifecycle | C027–C033 | 7 |
| `run` | C034–C042 | 9 |
| `storage` | C043–C050 | 8 |
| `config` | C051–C054 | 4 |
| `context` | C055–C059 | 5 |
| `auth` | C060–C061 | 2 |
| `instance` | C062–C065 | 4 |
| `capability` | C066–C069 | 4 |
| `adapter` | C070–C072 | 3 |
| `workflow`, `diagnostics`, service daemon | C073–C075 | 3 |
| **Total** | **C001–C075** | **75** |

## Execution sequence

1. Plan 063 records direction and extends fixture with success, failure, hold,
   queue, cancellation, logs, artifacts, and controlled state scenarios.
2. Plans 064–067 establish crates, resource/output contracts, durable history,
   and local API.
3. Plans 068, 074, and 075 establish config/GitHub/storage authority; then 069–
   073 build projections, logs, observation, reconciliation, and lifecycle.
   Plans 076–078 prepare packaging and remaining capability/diagnostic services.
   No handler may duplicate their logic.
4. Execute command tasks by dependency and priority. Each task is independently
   reviewable, fixture-validated, and marked `DONE` in `commands/README.md`.
5. C075 proves the source-built daemon. Plan 079 alone performs the live package/
   systemd cutover, real signed-package A/B/A acceptance, and removal of every
   old product/package/runtime surface.
6. Plan 080 adds optional remote contexts and fleet aggregation without changing
   command semantics or creating a scheduler.

## Invariants for every plan and command task

- Do not change GitHub Actions protocol behavior unless exact task requires it;
  consult current `actions/runner` first.
- Do not expand strict capabilities. Stop and request explicit approval when
  needed.
- Never weaken `tailrocks/velnor-actions-fixture`; extend it when coverage lacks
  a required observable state.
- Run Rust tests only with `rtk cargo nextest run`, normally via repository mise.
- Before fixture dispatch: cancel old pending/in-progress runs, delete only stale
  validation-owned registrations, prove clean, capture new run ID, and monitor
  only it. Check within 60 seconds; diagnose stasis before two minutes.
- Preserve sanitized `.json`, `.jsonl`, `.log`, `.md`, and archives only. Never
  save rendered GitHub HTML.
- Destructive commands are dry-run/plan-first where specified, explicit,
  ownership-safe, idempotent, audited, and rollback-aware.
- Commits use Conventional Commits, `git commit -s`, and trailer
  `Co-authored-by: Codex <codex@openai.com>`.

## Explicit exclusions

- No Velnor-native workflow scheduler/language.
- No capability marketplace or unknown-action fallback.
- No macOS runner support.
- No web UI or TUI.
- No unrestricted Docker pruning.
- No local Docker kill presented as GitHub cancellation.
- No Velnor release resource, release API, version-history store, activation
  pointer, rollback mechanism, or CLI release namespace. Signed apt/dpkg owns
  install, upgrade, downgrade, rollback, file verification, and version history.
- No `velnor-runner` alias, shim, package, or parallel daemon after Plan 079.
