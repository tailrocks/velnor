# Velnor Fleet Health Investigation - 2026-06-11

## Summary

The Velnor daemons did not crash and the GitHub token was not the immediate
cause. All production daemon units on Sentry were active with zero restarts:

- `velnor-daemon.service`: active, supervising 10 `java-monorepo` slots.
- `velnor-daemon@jackin-agent-brown.service`: active, supervising 2 slots.
- `velnor-daemon@blockchain-nodes.service`: active, supervising 4 slots.
- `velnor-daemon@fixture.service`: active, supervising 2 slots.

The failure mode is an architecture gap: Velnor treats an idle broker polling
loop as proof that a slot is healthy, but GitHub's runner registration state can
become `offline` or disappear while the process continues polling and logging
`No broker message received`. Once that happens, jobs queue forever because the
scheduler has no usable runner even though the daemon still thinks it is alive.

## Evidence Quality

The current evidence is strong enough to identify the missing Velnor behavior,
but not strong enough to claim the first actor in the failure was GitHub or
Velnor.

Proven by logs and APIs:

- the daemon processes stayed alive (`NRestarts=0`) while runners were
  offline or missing in the GitHub runner API,
- the slots had successfully created JIT runners and broker sessions earlier,
- after the split-brain, the slots continued logging successful empty broker
  polls instead of errors,
- queued jobs did not get assigned to those slots,
- the doctor could detect the degraded GitHub registry state but did not heal
  it.

Not yet proven:

- the exact GitHub-side transition that changed a runner from schedulable to
  offline or absent,
- whether a broker session can keep returning 204 after GitHub has removed the
  corresponding runner registration by design,
- whether GitHub emitted a control-plane message that Velnor ignored or failed
  to log,
- whether the stale state is triggered by idle JIT age, runner deletion, token
  refresh, session lifetime, GitHub maintenance, or a Velnor protocol mismatch.

The product conclusion does not depend on that distinction: a daemon slot must
periodically prove that its local `agent_id` and runner name are still present,
online, correctly labelled, and schedulable in GitHub's runner registry. Broker
poll success alone is insufficient health evidence.

## Confirmed State

GitHub runner API / doctor state during the investigation:

| Repo | Expected | Online | Registered | State |
|---|---:|---:|---:|---|
| `ChainArgos/java-monorepo` | 10 | 0 | 0 | fleet down |
| `ChainArgos/jackin-agent-brown` | 2 | 0 | 0 | fleet down |
| `ChainArgos/blockchain-nodes` | 4 | 1 | 1 | degraded |
| `tailrocks/velnor-actions-fixture` | 2 | 0 | 0 | fleet down |

Before manual stale-runner cleanup, the host doctor had already observed
degradation:

- `java-monorepo`: 6/10 online at 22:50 UTC.
- `blockchain-nodes`: 1/4 online at 22:50 UTC.
- `agent-brown`: 0/2 online at 22:51 UTC, with 2 registered offline.
- fixture: 0/2 online at 22:51 UTC, with 2 registered offline.

After stale registrations were deleted, the affected daemons did not recreate
fresh JIT configs because their slot tasks were still inside successful broker
poll loops.

## Evidence

Daemon startup and registration worked at 21:36 UTC:

- `java-monorepo`: created JIT runner ids `6941` through `6950`, then all ten
  slots created broker sessions.
- `agent-brown`: created ids `642` and `643`, then both slots created broker
  sessions.
- fixture: created ids `4237` and `4238`, then both slots created broker
  sessions.
- `blockchain-nodes`: created ids `2786` through `2789`, then all four slots
  created broker sessions.

Slots that received jobs recycled correctly:

- `blockchain-nodes` slot 4 completed jobs, deleted its broker session,
  discarded local JIT config, created fresh JIT runner ids `2790` through
  `2798`, and remained the only visible online runner.
- `java-monorepo` slots that handled jobs also recycled and created fresh ids
  `6951` through `6961`.

Slots that stayed idle did not recycle. Later, GitHub reported those runners as
offline or absent while the Velnor process kept polling broker sessions.

A live diagnostic run confirmed scheduling was broken:

- Dispatched fixture `compat.yml` with `lanes=velnor-only`.
- The GitHub-hosted setup jobs completed.
- Both Velnor matrix jobs stayed queued for more than two minutes.
- The run was cancelled after confirming no assignment.

## Why This Can Happen

Velnor uses repository-scoped ephemeral JIT runner configs. The current daemon
model is:

1. On daemon startup, `--replace` deletes old local/GitHub registrations and
   creates one JIT runner per slot.
2. Each slot creates one broker session and long-polls for jobs.
3. After a slot handles a job, Velnor deletes the broker session, discards local
   JIT config, creates a fresh JIT runner, and starts a new session.
4. If a slot is idle, it can poll forever. There is no independent check that
   the corresponding runner id is still registered and online in GitHub's runner
   list.

That means an idle slot can become a zombie:

- process alive,
- broker HTTP polling returns empty/204,
- systemd watchdog stays green,
- daemon status says "supervising N runner slot(s)",
- but GitHub's scheduler has no online runner for the job labels.

This is why the fleet can "disappear" from GitHub while Velnor still looks
healthy locally.

## What Was Forgotten

The missing piece is a control-plane reconciliation loop for idle slots.
Post-job recycling is not enough, because idle slots may never enter the
recycle path.

Velnor needs to periodically verify each slot's `agent_id` against GitHub's
runner list and force a slot recycle when:

- the runner id is missing,
- the runner is `offline`,
- the runner is registered but labels do not match expected labels,
- the runner has been idle beyond a configured max JIT age,
- the broker polling loop has only returned empty responses for too long.

The doctor timer detects this state, but it only alerts. It does not heal the
daemon.

## Immediate Recovery

A daemon restart should restore the missing runners because the packaged units
start with `--replace`, which forces all slots to create fresh JIT configs:

```sh
sudo systemctl restart velnor-daemon.service
sudo systemctl restart velnor-daemon@jackin-agent-brown.service
sudo systemctl restart velnor-daemon@blockchain-nodes.service
sudo systemctl restart velnor-daemon@fixture.service
```

Then verify:

```sh
sudo systemctl start velnor-doctor.service
sudo systemctl start velnor-doctor@jackin-agent-brown.service
sudo systemctl start velnor-doctor@blockchain-nodes.service
sudo systemctl start velnor-doctor@fixture.service
```

## Product Fix

Implement slot self-healing in the daemon:

1. Add a per-daemon runner-state reconciler using the same list-runners API as
   `doctor`.
2. Map expected slot name -> stored `agent_id`.
3. If a slot is missing/offline/mislabelled, signal that slot task to stop its
   broker loop, delete any stale local config, create a fresh JIT config, and
   start a new broker session.
4. Treat `online < slots` as unhealthy in `sd_notify STATUS`, not just in doctor
   logs.
5. Add a bounded max idle JIT age so every idle slot periodically refreshes even
   if GitHub still lists it as online.
6. Add an integration test or smoke command that deletes a registered idle
   runner and verifies the daemon recreates it without restarting.

This should become a P1 reliability item because queue-free performance
measurements depend on an actually online fleet.

## Fix Shipped (0.1.15, 2026-06-11)

Master-plan P1.9 implements the product fix; incident #9 in the review table.
Root cause found while implementing: `BrokerClient::get_runner_message`
treated **any** response with an empty body as "no message" — including 401
(expired OAuth token, ~1 h lifetime), 403/404 (deleted session), and curl
transport failures (status 0). An idle slot's token expired, every poll
became an empty-body 401, and the slot logged `No broker message received.`
forever. That matches the observed timeline: slots that handled jobs recycled
(fresh tokens) and survived; pure-idle slots went offline ~1 h after their
last registration.

Shipped changes:

1. `classify_broker_poll`: only 204 / 2xx-empty are "no message"; non-2xx and
   status-0 are errors → existing supervised error path recycles the slot
   with a fresh JIT config (self-healing via machinery that already existed).
2. Proactive idle credential refresh every 40 minutes (broker + run-service
   clients rebuilt in place, same as the ForceTokenRefresh control path).
3. Idle registry reconciler every 3 minutes: the slot looks up its own
   `agent_id` (curl transport, TLS-throttle-safe): 404 → recycle immediately;
   not-online twice in a row → recycle; lookup errors are logged but never
   recycle a healthy session. Recycle reasons surface in sd_notify STATUS.
4. Bounded max idle slot age (default 4 h, `--max-idle-slot-age-seconds`,
   0 disables): idle slots get periodically fresh registrations even when
   GitHub still lists them online.
5. Forensic logs (per-slot folders): `<slot-config>/logs/broker.log` (every
   poll outcome with HTTP status + consecutive counts), `registry.log` (every
   reconcile verdict), `lifecycle.log` (sessions, control messages, refreshes,
   recycle reasons), plus `<config-base>/logs/daemon.log` (supervisor events).
   All lines are timestamped and identity-prefixed (runner/agent_id/session),
   32 MB rotation. `tracing` JSON spans land in `logs/trace.jsonl`.

Remaining gates (P1.9): live split-brain repro on the fixture daemon (delete
an idle slot's registration via the API → daemon recycles it within ~3 min
without a restart), then a 24 h zero-zombie soak during the benchmark
campaign.
