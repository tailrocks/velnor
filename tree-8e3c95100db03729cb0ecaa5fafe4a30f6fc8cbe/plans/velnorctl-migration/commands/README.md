# Velnorctl leaf-command task index

This index is normative for command granularity. Each retained executable leaf
command from the approved research has exactly one implementation task. The
later operator decision removes all five release commands rather than migrating
them. One additional service-only command, `velnorctl daemon`, is required
because final architecture removes `velnor-runner` completely.

## Per-command execution contract

Every linked task is self-contained and owns only its named command. Shared
domain/API work lives in Plans 063–078. Sibling commands may depend on the same
service, but may not be combined into one implementation task.

Every task must:

- implement typed Clap syntax, thin handler, versioned output, stable errors,
  help/completion metadata, and focused tests;
- use versioned control/GitHub services, never parse internal files or spawn
  `velnor-runner`;
- run `rtk cargo nextest run` and `rtk mise run check`;
- validate on an exact pinned `tailrocks/velnor-actions-fixture` commit;
- cancel old runs and remove stale validation-owned registrations before each
  dispatch, monitor only the new run ID, check within 60 seconds, and diagnose
  unchanged/queued state before two minutes;
- preserve sanitized non-HTML evidence only;
- retain no backward-compatible alias after command migration.

All commands use Plan 065's one numeric `ExitClass` mapping: 0 success, 1
observed failed/degraded condition, 2 usage, 3 authorization, 4 unavailable/not
found, 5 timeout, 6 conflict/precondition, 7 transport/rate-limit/ambiguous
upstream, 8 definite operation failure, and 130 local SIGINT. A task may define
stable reason codes, never another numeric taxonomy.

## Commands

| Task | Command | Priority | Effort | Depends on | Status |
|---|---|---:|---:|---|---|
| [C001](C001-version.md) | `velnorctl version` | P1 | S | Plans 064–065, 067 | TODO |
| [C002](C002-api-resources.md) | `velnorctl api-resources` | P2 | S | Plans 065, 067 | TODO |
| [C003](C003-explain.md) | `velnorctl explain <resource-or-field>` | P2 | S | Plans 065, 067 | TODO |
| [C004](C004-completion.md) | `velnorctl completion <shell>` | P1 | S | Plans 064–069 | TODO |
| [C005](C005-man.md) | `velnorctl man` | P2 | S | Plans 064–065 | TODO |
| [C006](C006-get-hosts.md) | `velnorctl get hosts` | P3 | M | Plans 065, 067, 069 | TODO |
| [C007](C007-get-instances.md) | `velnorctl get instances` | P1 | M | Plans 065–069 | TODO |
| [C008](C008-get-slots.md) | `velnorctl get slots` | P1 | M | Plans 065–069, 073 | TODO |
| [C009](C009-get-runners.md) | `velnorctl get runners` | P1 | M | Plans 065–069 | TODO |
| [C010](C010-get-jobs.md) | `velnorctl get jobs` | P1 | M | Plans 065–069 | TODO |
| [C011](C011-get-runs.md) | `velnorctl get runs` | P2 | M | Plans 065–069, 074 | TODO |
| [C012](C012-get-queue.md) | `velnorctl get queue` | P2 | M | Plans 065–069, 073–074 | TODO |
| [C013](C013-get-events.md) | `velnorctl get events` | P2 | S | Plans 065–071 | TODO |
| [C014](C014-get-reservations.md) | `velnorctl get reservations` | P2 | S | Plans 065–069, 075 | TODO |
| [C015](C015-get-leases.md) | `velnorctl get leases` | P2 | S | Plans 065–069, 075 | TODO |
| [C016](C016-describe.md) | `velnorctl describe <resource>/<name>` | P1 | M | Plans 065–071, 074–075, 077 | TODO |
| [C017](C017-logs.md) | `velnorctl logs <resource>/<name>` | P1 | L | Plans 067, 069, 070 | TODO |
| [C018](C018-events.md) | `velnorctl events` | P2 | M | Plans 066, 067, 071 | TODO |
| [C019](C019-top.md) | `velnorctl top <host|instances|slots|jobs|storage>` | P2 | M | Plans 067, 069, 071 | TODO |
| [C020](C020-wait.md) | `velnorctl wait <resource>/<name> --for <condition>` | P1 | M | Plans 067, 069, 071, 073 | TODO |
| [C021](C021-doctor.md) | `velnorctl doctor [<target>]` | P1 | L | Plans 067, 068, 069, 072 | TODO |
| [C022](C022-preflight.md) | `velnorctl preflight` | P1 | M | Plans 067, 068, 072 | TODO |
| [C023](C023-reconcile-runners.md) | `velnorctl reconcile runners` | P1 | L | Plans 067, 068, 072, 074 | TODO |
| [C024](C024-reconcile-jobs.md) | `velnorctl reconcile jobs` | P1 | L | Plans 066, 067, 072, 074 | TODO |
| [C025](C025-reconcile-docker.md) | `velnorctl reconcile docker` | P1 | L | Plans 067, 072 | TODO |
| [C026](C026-reconcile-storage.md) | `velnorctl reconcile storage` | P1 | L | Plans 066, 067, 072, 075 | TODO |
| [C027](C027-cordon.md) | `velnorctl cordon instance/<name>` | P2 | M | Plans 067, 073 | TODO |
| [C028](C028-uncordon.md) | `velnorctl uncordon instance/<name>` | P2 | S | Plans 067, 073 | TODO |
| [C029](C029-drain.md) | `velnorctl drain instance/<name>` | P1 | L | Plans 067, 073 | TODO |
| [C030](C030-resume.md) | `velnorctl resume instance/<name>` | P1 | M | Plans 067, 068, 073 | TODO |
| [C031](C031-restart.md) | `velnorctl restart instance/<name>` | P1 | M | Plans 067–068, 073 | TODO |
| [C032](C032-recycle.md) | `velnorctl recycle <slot/...|runner/...>` | P1 | M | Plans 067, 073 | TODO |
| [C033](C033-scale.md) | `velnorctl scale instance/<name> --slots <count>` | P2 | L | Plans 067, 068, 073 | TODO |
| [C034](C034-run-list.md) | `velnorctl run list` | P1 | M | Plans 068, 069, 074 | TODO |
| [C035](C035-run-view.md) | `velnorctl run view <run-id>` | P1 | M | Plans 069–071, 074 | TODO |
| [C036](C036-run-watch.md) | `velnorctl run watch <run-id>` | P1 | M | Plans 069–071, 074 | TODO |
| [C037](C037-run-cancel.md) | `velnorctl run cancel <run-id>` | P1 | M | Plans 070, 073, 074 | TODO |
| [C038](C038-run-rerun.md) | `velnorctl run rerun <run-id>` | P1 | M | Plan 074 | TODO |
| [C039](C039-run-logs.md) | `velnorctl run logs <run-id>` | P1 | M | Plans 070, 074 | TODO |
| [C040](C040-run-download.md) | `velnorctl run download <run-id>` | P1 | M | Plan 074 | TODO |
| [C041](C041-run-dispatch.md) | `velnorctl run dispatch <workflow>` | P1 | M | Plans 068, 074 | TODO |
| [C042](C042-run-open.md) | `velnorctl run open <run-id>` | P2 | S | Plan 074 | TODO |
| [C043](C043-storage-status.md) | `velnorctl storage status` | P1 | M | Plans 067, 075 | TODO |
| [C044](C044-storage-paths.md) | `velnorctl storage paths` | P1 | S | Plans 067, 075 | TODO |
| [C045](C045-storage-du.md) | `velnorctl storage du` | P1 | M | Plans 067, 075 | TODO |
| [C046](C046-storage-gc.md) | `velnorctl storage gc` | P1 | L | Plans 067, 075 | TODO |
| [C047](C047-storage-history.md) | `velnorctl storage history` | P2 | S | Plans 066, 067, 075 | TODO |
| [C048](C048-storage-reservations.md) | `velnorctl storage reservations` | P1 | S | Plans 067, 075 | TODO |
| [C049](C049-storage-leases.md) | `velnorctl storage leases` | P1 | S | Plans 067, 075 | TODO |
| [C050](C050-storage-explain-pressure.md) | `velnorctl storage explain-pressure` | P1 | M | Plans 067, 075 | TODO |
| [C051](C051-config-view.md) | `velnorctl config view` | P1 | M | Plans 065, 067, 068 | TODO |
| [C052](C052-config-validate.md) | `velnorctl config validate` | P1 | M | Plans 067, 068, 072 | TODO |
| [C053](C053-config-diff.md) | `velnorctl config diff` | P2 | M | Plans 067–069 | TODO |
| [C054](C054-config-sources.md) | `velnorctl config sources` | P2 | S | Plans 065, 068 | TODO |
| [C055](C055-context-list.md) | `velnorctl context list` | P1 | S | Plans 064, 065, 068 | TODO |
| [C056](C056-context-current.md) | `velnorctl context current` | P1 | S | Plans 064, 065, 068 | TODO |
| [C057](C057-context-use.md) | `velnorctl context use <name>` | P1 | S | Plan 068 | TODO |
| [C058](C058-context-set.md) | `velnorctl context set <name> --endpoint <uri>` | P1 | M | Plans 065, 068 | TODO |
| [C059](C059-context-delete.md) | `velnorctl context delete <name>` | P2 | S | Plan 068 | TODO |
| [C060](C060-auth-status.md) | `velnorctl auth status` | P1 | S | Plans 065, 066, 068 | TODO |
| [C061](C061-auth-check.md) | `velnorctl auth check` | P1 | M | Plans 067, 068, 074 | TODO |
| [C062](C062-instance-init.md) | `velnorctl instance init <name>` | P1 | M | Plan 068 | TODO |
| [C063](C063-instance-install.md) | `velnorctl instance install <name>` | P1 | L | Plans 068, 076 | TODO |
| [C064](C064-instance-apply.md) | `velnorctl instance apply <name>` | P1 | L | Plans 068, 072, 073 | TODO |
| [C065](C065-instance-delete.md) | `velnorctl instance delete <name>` | P1 | L | Plans 068, 072, 073 | TODO |
| [C066](C066-capability-list.md) | `velnorctl capability list` | P2 | M | Plans 065, 067, 077 | TODO |
| [C067](C067-capability-explain.md) | `velnorctl capability explain <feature-or-action>` | P2 | M | Plans 065, 067, 077 | TODO |
| [C068](C068-capability-check.md) | `velnorctl capability check --job-dump <path>` | P1 | M | Plans 065, 077 | TODO |
| [C069](C069-capability-export.md) | `velnorctl capability export` | P2 | S | Plans 065, 077 | TODO |
| [C070](C070-adapter-list.md) | `velnorctl adapter list` | P2 | S | Plans 065, 077 | TODO |
| [C071](C071-adapter-describe.md) | `velnorctl adapter describe <action>` | P2 | M | Plans 065, 077 | TODO |
| [C072](C072-adapter-check.md) | `velnorctl adapter check <action@ref>` | P2 | M | Plans 065, 077 | TODO |
| [C073](C073-workflow-check.md) | `velnorctl workflow check --repo <owner/repo> --ref <ref> --workflow <path-or-name>` | P2 | L | Plans 068, 074, 077 | TODO |
| [C074](C074-diagnostics-bundle.md) | `velnorctl diagnostics bundle` | P2 | L | Plans 066–078 | TODO |
| [C075](C075-daemon.md) | `velnorctl daemon` | P1 | XL | Plans 067, 068, 072, 073 | TODO |

Status values: `TODO`, `IN PROGRESS`, `DONE`, `BLOCKED` with reason, or
`REJECTED` with rationale.

## Coverage rule

A command is complete only when its row is `DONE`, its focused test and full
repository gate pass, and its fresh fixture evidence is recorded. Plan 079
cannot remove the old crate/binary/package until all C001–C075 rows are `DONE`.

No task exists for a `cache` alias, old `capabilities` spelling, old `status`,
old `configure`, old `remove`, or old worker `run`; final product intentionally
has no compatibility surface. No task exists for a Velnor release namespace;
apt/dpkg are the native package/version interfaces.
