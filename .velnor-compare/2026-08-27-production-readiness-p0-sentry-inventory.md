# Production-readiness P0 Sentry inventory

Captured 2026-08-27 over read-only SSH to `sentry` (`5.9.55.237`) and GitHub
Actions APIs. No runner, job, process, cache, Docker resource, or service was
deleted, cancelled, restarted, drained, or otherwise mutated.

## Host state

`/var/lib/velnor/health.json`:

```json
{"control_live":true,"journal_writable":true,"github_reachable":false,"routing_valid":false,"runner_group_valid":false,"desired_ready_slots":0,"actual_ready_slots":0,"registered_slots":0,"capacity_permits":0,"executor_ready_slots":0,"oldest_queued_job_seconds":0,"oldest_outbox_entry_seconds":0,"external_canary":"unknown","execution_backend":"docker","state":"degraded"}
```

The obsolete `surge_ready_slots` health field was removed by the exact-capacity
migration; the remaining captured fields and values are unchanged.

- Root filesystem: 919G total, 773G used, 147G free, 85% used.
- Backend config: `/etc/velnor/execution.toml` selects exactly `backend = "docker"`.
- Package: `velnor-runner` version `0.1.215`, installed and status `install ok installed`.
- `/usr/bin/velnorctl` and `/usr/bin/velnor-runner` exist.
- `velnor-daemon.service` active; `velnor-guardian.service` active but disabled.
- `velnor-daemon.service` status reports supervising five runner slot processes.
- Active ChainArgos Docker build job is renewing run-service leases; two active
  containers are present (`velnor-job-...` and its BuildKit builder).
- Defunct `velnor-runner` processes observed: PIDs 83252, 380841, 380842,
  380845, 384302–384304, 384306, and 430150.
- Failed Velnor health units: `velnor-doctor@blockchain-nodes`, `fixture`,
  `tailrocks`, `termrock`, `velnor-chainargos`, and `velnor-jackin-project`.
  Twelve failed transient `run-p...` doctor units also remain loaded.

## Registered runners

Repository API observations (`id`, name, status, busy, labels):

- `tailrocks/velnor`: 7 registrations; four offline, three online; names
  include `velnor-dogfood-slot-*` and one `next-*` replacement.
- `ChainArgos/java-monorepo`: 7 registrations; six offline, one online and
  busy (`velnor-sentry-slot-4-next-384305-64`); four `next-*`/replacement
  names exist.
- `jackin-project/jackin`: 6 registrations; five offline, one online idle;
  all names are slot or `next-*` registrations.
- Organization API `orgs/tailrocks/actions/runners` separately returned five
  runners: three online `velnor-fixture-microvm` slots and two offline
  `velnor-tailrocks` slots.

The live host process list contains daemons for fixture, blockchain-nodes,
fixture-microvm, jackin, jackin-agent-brown, jackin-project, dogfood,
ChainArgos/java-monorepo (`velnor-sentry`), and tailrocks. The GitHub registry
and local process inventory therefore disagree on capacity/registration;
this is an unresolved lifecycle/routing issue, not a clean baseline.

## Storage/Docker observations

`velnorctl storage status --output json` reported all listed stores below their
configured per-store pressure budget, but host-wide disk use is already 85%:

| store | logical bytes | physical bytes | budget |
|---|---:|---:|---:|
| Cargo registry | 649,878,894 | 752,373,760 | 20 GiB |
| Cargo git | 808,571 | 1,155,072 | 20 GiB |
| mise installs | 266,805,883 | 283,443,200 | 20 GiB |
| target generations | 62,376,745,346 | 62,532,931,584 | 200 GiB |
| Velnor caches | 28,533,139 | 39,923,712 | 50 GiB |
| sccache | 11,376,475,123 | 11,407,507,456 | uncapped in output |

`velnorctl cache du --output json` showed large target scopes for
`trusted/ChainArgos_java-monorepo` and 16 sccache shard scopes. Host directory
sizes include `/var/cache/velnor` at 148G (including 1.7G quarantine),
`/var/lib/velnor-tailrocks` at 844G, and `/var/lib/velnor-jackin-project` at
72G. Docker reported 56 images (18.3G, 6.27G reclaimable), two active
containers, 937 volumes (201G, 197.4G reclaimable), and 5.47G build cache.

## Commands/evidence limits

Commands used: `systemctl list-units/status/--failed`, `df`, `ps`, `docker ps`,
`docker system df`, `dpkg-query`, `velnorctl status/storage/cache`, GitHub
`repos/<repo>/actions/runners`, and `orgs/tailrocks/actions/runners`.
`velnorctl status --instance velnor-sentry` failed with documented operation
code 8 and remediation `read daemon slot-1 status`; this failure is preserved.
No stale registration was removed because this was inventory, not the
pre-verification cleanup gate.
