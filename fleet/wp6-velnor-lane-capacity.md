# WP6 — Velnor-lane capacity and persistent-builder measurement

This document is **measurement-only**. It records what the 5-slot
`velnor-dogfood-slot-{1..5}` pool actually did on 2026-09-13. It does not
change release admission, capability gates, the generator, or generated
workflows.

Release stays GitHub-only until the guest-image user-namespace capability
(`guest-image-build.user-namespace` / unprivileged user namespaces or
`CAP_SYS_ADMIN`) is proven. This report does not bypass that gate.

Captured 2026-09-13T12:13:24Z from GitHub Actions for `tailrocks/velnor`.
Quoted log lines were re-fetched live at 2026-09-13T12:20:50Z and match the
capture. Queue and exec times use `queue_s = started_at − created_at` and
`exec_s = completed_at − started_at`. Nightly timestamps on run 34747340103
are coherent; some later CI/main jobs rewrite `created_at` after `started_at`
and are not used as the primary table.

Fleet version on jobs that uploaded logs:

```
Current runner version: 'Velnor Runner/0.1.274 (protocol: 2.337.0)'
```

Seen on jobs 103697787049, 103697787138 (run 34747340103), 103684693084
(run 34739692833), and 103697887744 (run 34745867993).

No performance speed-up percentages are claimed.

## Queue vs execution (Nightly run 34747340103, SHA fa6d2de)

- Run: [34747340103](https://github.com/tailrocks/velnor/actions/runs/34747340103)
  — Nightly · schedule · main
- `head_sha`: `fa6d2dee92c2fb682929ffc6de77642f1c97f746`
- Created 2026-09-13T08:18:08Z, updated 2026-09-13T08:31:00Z
- Conclusion: failure (`nightly-required`; Velnor `velnor-control` lost
  communication — see [Blocked](#blocked-job-log-protocol-on-v01274))
- Labels: `self-hosted`, `velnor-target-mvp`
- Pool: 5 concurrent slots; runner names increment `-next-<id>-N` per job

All 17 Velnor-lane jobs on that run, ordered by `started_at`:

| Job | Job ID | Runner | `runner_id` | queue_s | exec_s | Conclusion |
| --- | ---: | --- | ---: | ---: | ---: | --- |
| Rust · unit-collector / Velnor | [103697786995](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697786995) | velnor-dogfood-slot-2 | 4635 | 2 | 28 | success |
| Rust · velnor-client / Velnor | [103697787030](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787030) | velnor-dogfood-slot-4-next-254032-8 | 4622 | 2 | 37 | success |
| Rust · velnor-runner / Velnor | [103697787049](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787049) | velnor-dogfood-slot-1-next-180744-12 | 4634 | 2 | 130 | success |
| Docker · Docker / Velnor | [103697787138](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787138) | velnor-dogfood-slot-5-next-89257-15 | 4626 | 2 | 15 | success |
| Rust · velnor-render / Velnor | [103697787079](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787079) | velnor-dogfood-slot-3-next-395836-2 | 4632 | 3 | 36 | success |
| Rust · velnor-workflow / Velnor | [103697787057](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787057) | velnor-dogfood-slot-5-next-89257-16 | 4636 | 29 | 28 | success |
| Rust · velnor-control / Velnor | [103697787039](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787039) | velnor-dogfood-slot-2-next-411555-1 | 4640 | 41 | 601 | failure |
| Documentation · Documentation / Velnor | [103697786991](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697786991) | velnor-dogfood-slot-4-next-254032-9 | 4638 | 43 | 14 | success |
| OpenTofu · OpenTofu / Velnor | [103697786994](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697786994) | velnor-dogfood-slot-3-next-395836-3 | 4639 | 43 | 11 | success |
| Rust · velnor-tools / Velnor | [103697787045](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787045) | velnor-dogfood-slot-3-next-395836-4 | 4642 | 58 | 27 | success |
| Bun · Bun package (velnor) / Velnor | [103697787042](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787042) | velnor-dogfood-slot-4-next-254032-10 | 4643 | 60 | 30 | success |
| Rust · velnor-bench / Velnor | [103697787131](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787131) | velnor-dogfood-slot-5-next-89257-17 | 4641 | 61 | 34 | success |
| Rust · velnor-workflow-contract / Velnor | [103697787103](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787103) | velnor-dogfood-slot-3-next-395836-5 | 4644 | 97 | 20 | success |
| Rust · velnorctl / Velnor | [103697787114](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787114) | velnor-dogfood-slot-5-next-89257-18 | 4646 | 100 | 28 | success |
| Rust · Rust production topology / Velnor | [103697787196](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787196) | velnor-dogfood-slot-3-next-395836-6 | 4647 | 122 | 34 | success |
| Rust · Rust dependency policy / Velnor | [103697787001](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787001) | velnor-dogfood-slot-5-next-89257-19 | 4649 | 136 | 79 | success |
| Rust · velnor-model / Velnor | [103697787086](https://github.com/tailrocks/velnor/actions/runs/34747340103/job/103697787086) | velnor-dogfood-slot-1-next-180744-13 | 4637 | 136 | 75 | success |

Stats, recomputed from the table above (odd-n median = 9th ordered value;
even-n median = mean of the two central values):

- **queue_s (n=17):** min 2s, median 43s, max 136s. Ordered:
  2, 2, 2, 2, 3, 29, 41, 43, **43**, 58, 60, 61, 97, 100, 122, 136, 136.
- **exec_s (n=17, including protocol-fail job 103697787039):** min 11s,
  median 30s, max 601s. Ordered: 11, 14, 15, 20, 27, 28, 28, 28, **30**,
  34, 34, 36, 37, 75, 79, 130, 601.
- **exec_s excluding the 601s job (n=16):** min 11s, median 29s
  ((28+30)/2), max 130s. Ordered: 11, 14, 15, 20, 27, 28, 28, **28**,
  **30**, 34, 34, 36, 37, 75, 79, 130.
- **queue > exec** on 11/17 jobs: velnor-workflow (103697787057, 29s > 28s),
  Documentation (103697786991, 43s > 14s), OpenTofu (103697786994, 43s > 11s),
  velnor-tools (103697787045, 58s > 27s), Bun (103697787042, 60s > 30s),
  velnor-bench (103697787131, 61s > 34s), velnor-workflow-contract
  (103697787103, 97s > 20s), velnorctl (103697787114, 100s > 28s),
  Rust production topology (103697787196, 122s > 34s), Rust dependency
  policy (103697787001, 136s > 79s), velnor-model (103697787086, 136s > 75s).

The 5-slot fleet fills immediately, then queues. First wave (five jobs,
08:20:08Z–08:20:09Z) queued 2–3s. Later jobs wait for a slot (29–136s).

## Persistence across two jobs on the intended pool

Physical slot reuse is observed. Runner identity is **not** reused:
`runner_id` never repeats on the measured jobs, and names increment
`-next-<id>-N` each time a slot takes a new job.

### Slot reuse on Nightly 34747340103

Slot-5, sequential jobs:

| Job ID | Name | Runner | `runner_id` | Window (UTC) | queue_s | exec_s |
| ---: | --- | --- | ---: | --- | ---: | ---: |
| 103697787138 | Docker · Docker / Velnor | velnor-dogfood-slot-5-next-89257-15 | 4626 | 08:20:08Z → 08:20:23Z | 2 | 15 |
| 103697787057 | Rust · velnor-workflow / Velnor | velnor-dogfood-slot-5-next-89257-16 | 4636 | 08:20:35Z → 08:21:03Z | 29 | 28 |
| 103697787131 | Rust · velnor-bench / Velnor | velnor-dogfood-slot-5-next-89257-17 | 4641 | 08:21:07Z → 08:21:41Z | 61 | 34 |
| 103697787114 | Rust · velnorctl / Velnor | velnor-dogfood-slot-5-next-89257-18 | 4646 | 08:21:46Z → 08:22:14Z | 100 | 28 |
| 103697787001 | Rust · Rust dependency policy / Velnor | velnor-dogfood-slot-5-next-89257-19 | 4649 | 08:22:22Z → 08:23:41Z | 136 | 79 |

Slot-4, sequential jobs:

| Job ID | Name | Runner | `runner_id` | Window (UTC) | queue_s | exec_s |
| ---: | --- | --- | ---: | --- | ---: | ---: |
| 103697787030 | Rust · velnor-client / Velnor | velnor-dogfood-slot-4-next-254032-8 | 4622 | 08:20:08Z → 08:20:45Z | 2 | 37 |
| 103697786991 | Documentation · Documentation / Velnor | velnor-dogfood-slot-4-next-254032-9 | 4638 | 08:20:49Z → 08:21:03Z | 43 | 14 |
| 103697787042 | Bun · Bun package (velnor) / Velnor | velnor-dogfood-slot-4-next-254032-10 | 4643 | 08:21:06Z → 08:21:36Z | 60 | 30 |

Same physical slot, new runner process each job.

### Host-persistent cache: yes

Job 103697787049 (run 34747340103, Rust · velnor-runner / Velnor,
`velnor-dogfood-slot-1-next-180744-12`, `runner_id` 4634). Exact log line:

```
Cache paths live on Velnor host-persistent storage (always warm)
```

Same job, git mirror and checkout:

```
Linked 399 object file(s) (14916966 bytes) and 0 ref(s) from the shared mirror; no objects were copied and no network fetch was needed
Pinned 549 file and directory mtimes to the commit timestamp (stable cargo fingerprints across jobs)
Repository path: /var/lib/velnor-dogfood/work/slot-1/48e7aeae-fd5e-58c6-bb54-afd7f5476d06/workspace/.
```

The workspace is a per-job UUID under `/var/lib/velnor-dogfood/work/slot-N/<uuid>/workspace`.
The cache class is host-persistent; the workspace is not a reused runner identity.

### BuildKit / mbx across two Docker jobs on slot-5

Two Docker · Docker / Velnor jobs on slot-5, different runner identities,
about 83 minutes apart.

**Cold-ish** — run [34739692833](https://github.com/tailrocks/velnor/actions/runs/34739692833)
job [103684693084](https://github.com/tailrocks/velnor/actions/runs/34739692833/job/103684693084)
(`velnor-dogfood-slot-5-next-4119141-5`, `runner_id` 4558).
Started 2026-09-13T06:22:31Z, completed 2026-09-13T06:26:04Z, `queue_s=2`,
`exec_s=213`. Exact lines:

```
Current runner version: 'Velnor Runner/0.1.274 (protocol: 2.337.0)'
Finished `release` profile [optimized] target(s) in 1m 59s
mbx[cache]: 0 hits, 1 misses, 1779 not looked up, 243 bypassed; 0 B downloaded, 0 B uploaded, 1.0 GiB stored locally
VELNOR_CI_REPORT {"job":"Docker · Docker / Velnor",...,"queue_seconds":4,"checks_wall_seconds":203,...}
```

**Later, same slot** — run [34745867993](https://github.com/tailrocks/velnor/actions/runs/34745867993)
job [103697887744](https://github.com/tailrocks/velnor/actions/runs/34745867993/job/103697887744)
(`velnor-dogfood-slot-5-next-89257-12`, `runner_id` 4608).
Started 2026-09-13T07:45:49Z, completed 2026-09-13T07:47:26Z, `exec_s=97`.
BuildKit layers `#6` through `#29` are `CACHED`; `#37` is `CACHED`. Exact
lines:

```
Current runner version: 'Velnor Runner/0.1.274 (protocol: 2.337.0)'
#6 CACHED
#29 CACHED
Finished `release` profile [optimized] target(s) in 1m 17s
mbx[cache]: 1651 hits, 66 misses, 88 not looked up, 239 bypassed; 0 B downloaded, 0 B uploaded, 2.0 MiB stored locally
#37 CACHED
VELNOR_CI_REPORT {"job":"Docker · Docker / Velnor",...,"queue_seconds":240,"checks_wall_seconds":84,...}
```

BuildKit layer cache and the mbx store survived across jobs on the same host
slot. Persistent builder **cache**: yes. Persistent builder **daemon
identity**: not observed. Neither Docker job log contains `velnor-builder`,
`persistent builder`, or `buildkitd`. Fleet image is still 0.1.274, so
in-tree persistent-builder work is not deployed.

## Blocked: job-log protocol on v0.1.274

Jobs ran on the pool, then lost communication. GitHub stored no step list and
no log archive. This clears only on fleet redeploy (user-side, not this
change).

### Nightly velnor-control

- Run 34747340103, job 103697787039, Rust · velnor-control / Velnor
- Runner `velnor-dogfood-slot-2-next-411555-1`, `runner_id` 4640
- Started 2026-09-13T08:20:47Z, completed 2026-09-13T08:30:48Z
- `queue_s=41`, `exec_s=601`, `steps=[]`
- `gh run view 34747340103 --job 103697787039 --log` → `log not found: 103697787039`
- Check-run annotation (exact):

```
The self-hosted runner lost communication with the server. Verify the machine is running and has a healthy network connection. Anything in your workflow that terminates the runner process, starves it for CPU/Memory, or blocks its network access can cause this error.
```

### CI/main rust-policy

- Run [34744289684](https://github.com/tailrocks/velnor/actions/runs/34744289684),
  job 103689451226, Rust · Rust dependency policy / Velnor
- Runner `velnor-dogfood-slot-3-next-180765-1`, `runner_id` 4573
- Started 2026-09-13T07:06:54Z, completed 2026-09-13T07:16:54Z
- `queue_s=63`, `exec_s=600`, `steps=[]`
- `gh run view 34744289684 --job 103689451226 --log` → `log not found: 103689451226`
- Same lost-communication annotation as job 103697787039.

Fleet still advertises 0.1.274 on jobs that did upload logs (quoted above).
Redeploy is required to pick up in-tree protocol fixes.

## Capacity stall

The 5-slot pool saturates and then stops assigning.

Run [34748391614](https://github.com/tailrocks/velnor/actions/runs/34748391614)
— CI / main · push · main, `head_sha`
`e5da61458cd261eb37b65e29369d299b4cf0bab5`, created 2026-09-13T08:43:18Z.

At capture 2026-09-13T12:13:24Z the run was still `queued`, `updated_at`
stuck at 2026-09-13T10:42:15Z. Live re-query at 2026-09-13T12:20:50Z: same
status, same `updated_at`, same 11 jobs still queued with no runner.

Completed on the pool (6/17), then assignment stopped:

| Job ID | Name | Runner | queue_s | exec_s |
| ---: | --- | --- | ---: | ---: |
| 103700503388 | Rust · Rust dependency policy / Velnor | velnor-dogfood-slot-4-next-254032-12 | 2 | 15 |
| 103700503396 | Documentation · Documentation / Velnor | velnor-dogfood-slot-2 | 3 | 11 |
| 103700503441 | OpenTofu · OpenTofu / Velnor | velnor-dogfood-slot-1 | 2 | 11 |
| 103700503455 | Rust · velnor-model / Velnor | velnor-dogfood-slot-3-next-395836-7 | 5 | 14 |
| 103700503466 | Rust · velnorctl / Velnor | velnor-dogfood-slot-5 | 2 | 21 |
| 103700503485 | Rust · velnor-client / Velnor | velnor-dogfood-slot-3-next-395836-8 | 23 | 14 |

Still queued at 12:13:24Z (wait ~12500s / 3h28m from `created_at`
08:45:07Z; no runner assigned). Still queued at the 12:20:50Z re-query:

| Job ID | Name | `created_at` |
| ---: | --- | --- |
| 103700503442 | Rust · velnor-runner / Velnor | 2026-09-13T08:45:07Z |
| 103700503451 | Rust · velnor-workflow-contract / Velnor | 2026-09-13T08:45:07Z |
| 103700503458 | Rust · Rust production topology / Velnor | 2026-09-13T08:45:07Z |
| 103700503559 | Bun · Bun package (velnor) / Velnor | 2026-09-13T08:45:07Z |
| 103700503563 | Rust · velnor-workflow / Velnor | 2026-09-13T08:45:07Z |
| 103700503573 | Rust · velnor-control / Velnor | 2026-09-13T08:45:07Z |
| 103700503575 | Docker · Docker / Velnor | 2026-09-13T08:45:07Z |
| 103700503674 | Rust · unit-collector / Velnor | 2026-09-13T08:45:07Z |
| 103700503880 | Rust · velnor-render / Velnor | 2026-09-13T08:45:08Z |
| 103700503916 | Rust · velnor-tools / Velnor | 2026-09-13T08:45:08Z |
| 103700504077 | Rust · velnor-bench / Velnor | 2026-09-13T08:45:08Z |

That is evidence the 5-slot pool saturates: six jobs ran, eleven sat with
empty `runner_name` for hours. This report does not diagnose why assignment
stopped after the first wave.

## Non-goals

- No release capability-gate bypass. Release stays GitHub-only until the
  guest-image user-namespace capability is proven.
- No mbx replacement.
- No raising GC budgets as a persistence substitute.
- No generator or generated-workflow changes.
- No claim that persistent builder *daemon identity* is deployed: fleet is
  still v0.1.274.

## Verdict

| Claim | Evidence |
| --- | --- |
| Queue often exceeds exec on the 5-slot pool | Nightly 34747340103, 17 jobs 103697786991–103697787196: median queue 43s; median exec 30s (n=17) / 29s excluding 601s job 103697787039; queue > exec on 11/17 |
| Physical slot reused, runner identity not | Slot-5 jobs 103697787138 then 103697787057 (`runner_id` 4626 then 4636); slot-4 jobs 103697787030 then 103697786991 (`runner_id` 4622 then 4638) |
| Host cache persistent | Job 103697787049: `Cache paths live on Velnor host-persistent storage (always warm)` |
| Persistent builder *cache* yes | Docker jobs 103684693084 (run 34739692833, mbx 0 hits, exec 213s) → 103697887744 (run 34745867993, layers #6–#29 CACHED, mbx 1651 hits / 66 misses, exec 97s) |
| Persistent builder *daemon identity* not observed | No `velnor-builder` name in those Docker logs; fleet 0.1.274 |
| Job-log protocol blocked on 0.1.274 | Jobs 103697787039 (run 34747340103) and 103689451226 (run 34744289684): `log not found`, `steps=[]`, lost-communication annotation |
| 5-slot pool saturates | Run 34748391614: 6 completed, 11 still queued ~3h28m at 12:13:24Z with no runner |

Redeploy of the fleet is required to clear the job-log protocol rejection.
This measurement does not admit a Velnor release lane.
