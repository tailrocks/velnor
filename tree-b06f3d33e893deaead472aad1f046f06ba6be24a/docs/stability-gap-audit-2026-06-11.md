# Stability gap audit — 2026-06-11

Proactive failure-mode sweep after incident #9 (zombie fleet). Sixteen
scenarios traced through the code; verdicts below. The five priority gaps are
fixed in 0.1.16 (marked ✅); the remainder are tracked for follow-up.

| # | Scenario | Verdict (pre-0.1.16) | 0.1.16 |
|---|---|---|---|
| 1 | OAuth expires mid-job (cancellation poller 401s ~20min in) | PARTIAL | ✅ poller refreshes credentials on 401/403 and rebuilds the broker client in place |
| 2 | BrokerMigration while busy (poller discarded it; cancellation dead for job tail) | PARTIAL | ✅ poller applies migrations mid-job |
| 3 | run-service 5xx at completion → finished job silently lost | GAP | ✅ `complete_job` bounded retry (6 attempts, 5→60s, 4xx non-retriable except 408/429); lock renewal stays alive until completion accepted |
| 4 | Docker daemon restart mid-job | PARTIAL | follow-up: per-cycle resource prune; infra-category for docker-connect errors exists |
| 5 | Disk full | GAP | ✅ per-cycle free-space guard (2 GiB threshold, slot parks with sd_notify status); trace.jsonl rotation (32 MB, was the only unbounded writer) |
| 6 | Hours-long network loss → lockstep retries (PID-shared jitter) | PARTIAL | ✅ per-slot salted backoff (`slot_retry_delay`) |
| 7 | PAT REST budget | HANDLED (~620/h steady ≈ 12%) | ✅ pagination removed the >30-runner blind spot |
| 8 | Clock skew rejects OAuth exchange | GAP | ✅ assertion validity window backdated 120s (lifetime kept at 300s) |
| 9 | Watchdog starvation under load | PARTIAL | follow-up: dedicated ping thread (low likelihood) |
| 10 | Clean recycle orphans registration (local config removed before delete-by-id) → guaranteed 409 dance; unpaginated `list_runners` blind past 30 runners | GAP | ✅ recycle deletes by id first; `list_runners` paginates 100/page |
| 11 | Image pull fails | PARTIAL | clean Failed completion ✓ (now durable via #3); pull backoff follow-up |
| 12 | Broker slow-loris | HANDLED | reconciler (≤6min) + max idle age backstop |
| 13 | GitHub maintenance (30min 502s) | HANDLED | no exit path confirmed; panicked slot now respawned by index (catch_unwind) ✅ |
| 14 | Host reboot | HANDLED | postinst enable + Requires=docker; template instances need operator `enable` (documented) |
| 15 | **Fast local-failure churn** (docker down/clock skew → delete+re-JIT every 5s × slots → PAT exhausted in ~20min) | GAP (worst) | ✅ `LocalRunnerFailure` classification: local faults keep the registration and back off per-slot 5s→10min |
| 16 | Misc: renewal-failure streak, no container memory caps, poller log spam | GAP-minor | ✅ poller log rate-capped; renewal/mem-caps follow-up |

Follow-ups as of 0.1.16; later status is tracked in
[`docs/master-plan.md`](master-plan.md). Shipped after this audit: per-cycle
Docker prune (`runner.rs::prune_stale_velnor_docker_resources`) and daemon job
container CPU/memory caps (`VELNOR_JOB_CPUS` / `VELNOR_JOB_MEMORY`, 0.1.28
`533a5c4`). Remaining follow-ups: renewal-failure streak cancels local job
(#16), dedicated watchdog ping thread (#9), pull backoff (#11), completion
payload spill-replay across slot recycle (#3 extension).
