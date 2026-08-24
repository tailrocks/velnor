# Task checklist

Executable checkbox mirror of the plan library. One row per leaf; order follows
the dependency graph in [`goal-execution/README.md`](goal-execution/README.md).

Rules for every agent working this list:

1. The authoritative status lives in each item file and its category index.
   This file is a mirror: when you flip an item here, flip it there in the same
   commit, and never mark `[x]` without evidence mapped to the item's done
   criteria.
2. Never implement in primary context. Each leaf goes through fresh
   investigator -> executor -> verifier -> reviewer subagents on the single
   campaign branch (`velnor-estate-standard`).
3. A leaf is DONE only when all gates pass at current HEAD: focused nextest,
   `rtk mise run check`, integration/fixture proof, safety scan, independent
   review, index agreement, commit trailers (`git commit -s`,
   `Co-authored-by: Codex <codex@openai.com>`).
4. `BLOCKED` requires exact evidence and the named external decision. Never
   batch-complete siblings; shared code does not transfer proof.

Status legend: `[ ]` TODO · `[x]` DONE · `[~]` IN PROGRESS · `[-]` BLOCKED(reason)

<<<<<<< HEAD
Progress: 4 / 94 done.
=======
Progress: 3 / 94 done.
>>>>>>> origin/main

## Track A - fleet policy (P0, independent)

- [~] **039** - Reconcile restricted organization JIT fleets (P0) - [fleet-operations/039-org-jit-multi-repo-fleet.md](fleet-operations/039-org-jit-multi-repo-fleet.md)

## Track B - velnorctl migration

### Shared architecture (execute in order)

- [x] **063** - Record direction and fixture control contract (P1) - first; unblocks everything
- [x] **064** - Scaffold workspace and CLI seams (P1, deps 063)
- [x] **065** - Resources, rendering, global conventions (P1, deps 064)
- [ ] **066** - Persist sanitized operational history and events (P1, deps 065)
- [ ] **067** - Versioned Unix-socket control API (P1, deps 066)
- [ ] **074** - GitHub Actions client and run merge service (P1, deps 065 066 068)
- [ ] **075** - Storage control services (P1, deps 066 067)
- [ ] **076** - Debian-native package transition (P1, deps 064 068)
- [ ] **068** - Configuration, auth, instance services (P1, deps 064-067)
- [ ] **069** - Resource query and description services (P1, deps 065-068 074-075)
- [ ] **070** - Active and completed log services (P1, deps 065-069 074)
- [ ] **071** - Event, metrics, wait services (P2, deps 066 067 069 075)
- [ ] **077** - Capability, adapter, workflow-check services (P2, deps 064-069)
- [ ] **072** - Health, preflight, reconciliation services (P1, deps 066-071 074-075)
- [ ] **078** - Sanitized diagnostics collection service (P2, deps 066-077)
- [ ] **073** - Daemon and lifecycle state engine (P1 XL, deps 066-072)

### Leaf commands C001-C075 (only when each row's named deps are DONE; prefer P1, then P2, then P3)

- [ ] **C001** - `version` (P1) - [commands/C001-version.md](velnorctl-migration/commands/C001-version.md)
- [ ] **C002** - `api-resources` (P2) - [commands/C002-api-resources.md](velnorctl-migration/commands/C002-api-resources.md)
- [ ] **C003** - `explain` (P2) - [commands/C003-explain.md](velnorctl-migration/commands/C003-explain.md)
- [ ] **C004** - `completion` (P1) - [commands/C004-completion.md](velnorctl-migration/commands/C004-completion.md)
- [x] **C005** - `man` (P2) - [commands/C005-man.md](velnorctl-migration/commands/C005-man.md)
- [ ] **C007** - `get instances` (P1) - [commands/C007-get-instances.md](velnorctl-migration/commands/C007-get-instances.md)
- [ ] **C009** - `get runners` (P1) - [commands/C009-get-runners.md](velnorctl-migration/commands/C009-get-runners.md)
- [ ] **C010** - `get jobs` (P1) - [commands/C010-get-jobs.md](velnorctl-migration/commands/C010-get-jobs.md)
- [ ] **C044** - `storage paths` (P1) - [commands/C044-storage-paths.md](velnorctl-migration/commands/C044-storage-paths.md)
- [ ] **C054** - `config sources` (P2) - [commands/C054-config-sources.md](velnorctl-migration/commands/C054-config-sources.md)
- [ ] **C055** - `context list` (P1) - [commands/C055-context-list.md](velnorctl-migration/commands/C055-context-list.md)
- [ ] **C056** - `context current` (P1) - [commands/C056-context-current.md](velnorctl-migration/commands/C056-context-current.md)
- [ ] **C057** - `context use` (P1) - [commands/C057-context-use.md](velnorctl-migration/commands/C057-context-use.md)
- [ ] **C059** - `context delete` (P2) - [commands/C059-context-delete.md](velnorctl-migration/commands/C059-context-delete.md)
- [ ] **C060** - `auth status` (P1) - [commands/C060-auth-status.md](velnorctl-migration/commands/C060-auth-status.md)
- [ ] **C062** - `instance init` (P1) - [commands/C062-instance-init.md](velnorctl-migration/commands/C062-instance-init.md)
- [ ] **C066** - `capability list` (P2) - [commands/C066-capability-list.md](velnorctl-migration/commands/C066-capability-list.md)
- [ ] **C067** - `capability explain` (P2) - [commands/C067-capability-explain.md](velnorctl-migration/commands/C067-capability-explain.md)
- [ ] **C068** - `capability check` (P1) - [commands/C068-capability-check.md](velnorctl-migration/commands/C068-capability-check.md)
- [ ] **C069** - `capability export` (P2) - [commands/C069-capability-export.md](velnorctl-migration/commands/C069-capability-export.md)
- [ ] **C070** - `adapter list` (P2) - [commands/C070-adapter-list.md](velnorctl-migration/commands/C070-adapter-list.md)
- [ ] **C071** - `adapter describe` (P2) - [commands/C071-adapter-describe.md](velnorctl-migration/commands/C071-adapter-describe.md)
- [ ] **C072** - `adapter check` (P2) - [commands/C072-adapter-check.md](velnorctl-migration/commands/C072-adapter-check.md)
- [ ] **C006** - `get hosts` (P3) - [commands/C006-get-hosts.md](velnorctl-migration/commands/C006-get-hosts.md)
- [ ] **C008** - `get slots` (P1, needs 073) - [commands/C008-get-slots.md](velnorctl-migration/commands/C008-get-slots.md)
- [ ] **C011** - `get runs` (P2, needs 074) - [commands/C011-get-runs.md](velnorctl-migration/commands/C011-get-runs.md)
- [ ] **C012** - `get queue` (P2, needs 073 074) - [commands/C012-get-queue.md](velnorctl-migration/commands/C012-get-queue.md)
- [ ] **C013** - `get events` (P2) - [commands/C013-get-events.md](velnorctl-migration/commands/C013-get-events.md)
- [ ] **C014** - `get reservations` (P2) - [commands/C014-get-reservations.md](velnorctl-migration/commands/C014-get-reservations.md)
- [ ] **C015** - `get leases` (P2) - [commands/C015-get-leases.md](velnorctl-migration/commands/C015-get-leases.md)
- [ ] **C016** - `describe` (P1) - [commands/C016-describe.md](velnorctl-migration/commands/C016-describe.md)
- [ ] **C017** - `logs` (P1) - [commands/C017-logs.md](velnorctl-migration/commands/C017-logs.md)
- [ ] **C018** - `events` (P2) - [commands/C018-events.md](velnorctl-migration/commands/C018-events.md)
- [ ] **C019** - `top` (P2) - [commands/C019-top.md](velnorctl-migration/commands/C019-top.md)
- [ ] **C020** - `wait` (P1, needs 073) - [commands/C020-wait.md](velnorctl-migration/commands/C020-wait.md)
- [ ] **C021** - `doctor` (P1) - [commands/C021-doctor.md](velnorctl-migration/commands/C021-doctor.md)
- [ ] **C022** - `preflight` (P1) - [commands/C022-preflight.md](velnorctl-migration/commands/C022-preflight.md)
- [ ] **C023** - `reconcile runners` (P1) - [commands/C023-reconcile-runners.md](velnorctl-migration/commands/C023-reconcile-runners.md)
- [ ] **C024** - `reconcile jobs` (P1) - [commands/C024-reconcile-jobs.md](velnorctl-migration/commands/C024-reconcile-jobs.md)
- [ ] **C025** - `reconcile docker` (P1) - [commands/C025-reconcile-docker.md](velnorctl-migration/commands/C025-reconcile-docker.md)
- [ ] **C026** - `reconcile storage` (P1) - [commands/C026-reconcile-storage.md](velnorctl-migration/commands/C026-reconcile-storage.md)
- [ ] **C043** - `storage status` (P1) - [commands/C043-storage-status.md](velnorctl-migration/commands/C043-storage-status.md)
- [ ] **C045** - `storage du` (P1) - [commands/C045-storage-du.md](velnorctl-migration/commands/C045-storage-du.md)
- [ ] **C046** - `storage gc` (P1) - [commands/C046-storage-gc.md](velnorctl-migration/commands/C046-storage-gc.md)
- [ ] **C047** - `storage history` (P2) - [commands/C047-storage-history.md](velnorctl-migration/commands/C047-storage-history.md)
- [ ] **C048** - `storage reservations` (P1) - [commands/C048-storage-reservations.md](velnorctl-migration/commands/C048-storage-reservations.md)
- [ ] **C049** - `storage leases` (P1) - [commands/C049-storage-leases.md](velnorctl-migration/commands/C049-storage-leases.md)
- [ ] **C050** - `storage explain-pressure` (P1) - [commands/C050-storage-explain-pressure.md](velnorctl-migration/commands/C050-storage-explain-pressure.md)
- [ ] **C051** - `config view` (P1) - [commands/C051-config-view.md](velnorctl-migration/commands/C051-config-view.md)
- [ ] **C052** - `config validate` (P1) - [commands/C052-config-validate.md](velnorctl-migration/commands/C052-config-validate.md)
- [ ] **C053** - `config diff` (P2) - [commands/C053-config-diff.md](velnorctl-migration/commands/C053-config-diff.md)
- [ ] **C058** - `context set` (P1) - [commands/C058-context-set.md](velnorctl-migration/commands/C058-context-set.md)
- [ ] **C061** - `auth check` (P1) - [commands/C061-auth-check.md](velnorctl-migration/commands/C061-auth-check.md)
- [ ] **C063** - `instance install` (P1, needs 076) - [commands/C063-instance-install.md](velnorctl-migration/commands/C063-instance-install.md)
- [ ] **C064** - `instance apply` (P1, needs 072 073) - [commands/C064-instance-apply.md](velnorctl-migration/commands/C064-instance-apply.md)
- [ ] **C065** - `instance delete` (P1, needs 072 073) - [commands/C065-instance-delete.md](velnorctl-migration/commands/C065-instance-delete.md)
- [ ] **C073** - `workflow check` (P2) - [commands/C073-workflow-check.md](velnorctl-migration/commands/C073-workflow-check.md)
- [ ] **C034** - `run list` (P1) - [commands/C034-run-list.md](velnorctl-migration/commands/C034-run-list.md)
- [ ] **C035** - `run view` (P1) - [commands/C035-run-view.md](velnorctl-migration/commands/C035-run-view.md)
- [ ] **C036** - `run watch` (P1) - [commands/C036-run-watch.md](velnorctl-migration/commands/C036-run-watch.md)
- [ ] **C037** - `run cancel` (P1, needs 073) - [commands/C037-run-cancel.md](velnorctl-migration/commands/C037-run-cancel.md)
- [ ] **C038** - `run rerun` (P1) - [commands/C038-run-rerun.md](velnorctl-migration/commands/C038-run-rerun.md)
- [ ] **C039** - `run logs` (P1) - [commands/C039-run-logs.md](velnorctl-migration/commands/C039-run-logs.md)
- [ ] **C040** - `run download` (P1) - [commands/C040-run-download.md](velnorctl-migration/commands/C040-run-download.md)
- [ ] **C041** - `run dispatch` (P1) - [commands/C041-run-dispatch.md](velnorctl-migration/commands/C041-run-dispatch.md)
- [ ] **C042** - `run open` (P2) - [commands/C042-run-open.md](velnorctl-migration/commands/C042-run-open.md)
- [ ] **C027** - `cordon` (P2, needs 073) - [commands/C027-cordon.md](velnorctl-migration/commands/C027-cordon.md)
- [ ] **C028** - `uncordon` (P2, needs 073) - [commands/C028-uncordon.md](velnorctl-migration/commands/C028-uncordon.md)
- [ ] **C029** - `drain` (P1, needs 073) - [commands/C029-drain.md](velnorctl-migration/commands/C029-drain.md)
- [ ] **C030** - `resume` (P1, needs 073) - [commands/C030-resume.md](velnorctl-migration/commands/C030-resume.md)
- [ ] **C031** - `restart` (P1, needs 073) - [commands/C031-restart.md](velnorctl-migration/commands/C031-restart.md)
- [ ] **C032** - `recycle` (P1, needs 073) - [commands/C032-recycle.md](velnorctl-migration/commands/C032-recycle.md)
- [ ] **C033** - `scale` (P2, needs 073) - [commands/C033-scale.md](velnorctl-migration/commands/C033-scale.md)
- [ ] **C075** - `daemon` (P1 XL, needs 073) - [commands/C075-daemon.md](velnorctl-migration/commands/C075-daemon.md)
- [ ] **C074** - `diagnostics bundle` (P2, needs 078) - [commands/C074-diagnostics-bundle.md](velnorctl-migration/commands/C074-diagnostics-bundle.md)

### Closure

- [ ] **079** - Cut over package, remove `velnor-runner` (P1 XL, deps 063-078 + all C001-C075; signed-apt A/B/A proof)
- [ ] **080** - Authenticated remote transport and fleet views (P3, deps 079; explicit operator approval of exact listener/PKI/identity surface)

## Follow-up queue

Immediate next actions, in order (update this section as work lands):

1. Reconcile drift anchors: every command file cites excerpts predating current
   HEAD; refresh per-file before execution (playbook requirement), starting
   with 063.
2. Execute **063** first (direction + fixture control contract); repository
   policy forbids product implementation before direction docs agree.
3. Run **039** in parallel via its own subagent lane; live apply waits for
   reviewed digest + explicit operator authorization.
4. After 063: dispatch 064, then follow the Track B order above.
5. Keep `main` merged into the campaign branch before every leaf start;
   re-run `rtk cargo nextest run --workspace` after every merge.
6. When 079 completes: verify no `velnor-runner` product surface remains; only
   then open the campaign completion PR.

## Done definition (campaign-wide)

Plan 039 + Plans 063-080 + every C001-C075 row DONE; root and category indexes
agree; final acceptance gates green at current HEAD; no unresolved review
finding. Until then the campaign stays incomplete even if every code path works.
