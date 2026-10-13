# Velnor Bastion

Current live truth: [docs/CURRENT_STATE.md](docs/CURRENT_STATE.md).

Snapshot captured from `root@37.27.110.241` on 2026-09-23. The host has the Velnor package, Docker, local controller/guardian processes, and a 16-slot local fleet. It is currently **NOT READY / DEGRADED**: no slots are registered with GitHub, routing and runner-group proofs are invalid, and the control API is unavailable.

`CAMPAIGN_LEDGER.md` and the paused handoff preserve historical rollout claims and plans. They are not live health evidence. Do not resume from their “complete” or “in progress” status without rechecking [CURRENT_STATE.md](docs/CURRENT_STATE.md).

The snapshot intentionally excludes secret values. Remote inspection was read-only.
