# Velnor Evidence Record — 2026-09-01

## Purpose and status vocabulary

This is a dated evidence archive. It consolidates historical reports, incidents,
research, ADRs, proposals, runbooks, plan evidence, measured baselines, frozen
references, acceptance results, and known gaps read from this repository on
2026-09-01.

- **Authority snapshot** means the repository's marked contract or a dated
  state capture. It is not proof that runtime behavior currently matches it.
- **Measured** means a reported observation with its original date, run ID,
  version, host, or exact number preserved.
- **Historical** means stale, superseded, or tied to an earlier commit/version.
  It must not be read as current behavior.
- **Acceptance evidence** means evidence recorded by a plan/report. It counts
  only where the owning plan says its current-HEAD and independent gates pass.
- **Open / unproven** means no completion claim is made.

Repository documentation itself says the marked contract is the source of truth
and older conflicting evidence is historical/non-normative
(`docs/README.md:3-18`, `VELNOR_PROJECTS_SETUP.md:3-17`). This record therefore
does not promote an old plan, green local test, release, or live sample into a
current implementation claim.

Source paths below identify the historical Markdown inputs read for this archive.
Those inputs were intentionally removed by the documentation migration; the
claims and measurements needed for ongoing understanding are retained here.

## 1. Authority boundary captured on 2026-09-01

The marked unified-CI contract dated 2026-08-09 states:

- exactly 28 repositories, in four generated classes: 20 code, 5 tap, 2 apt,
  1 fixture;
- byte-identical generated workflows within each class;
- exactly plural `lanes` with values `velnor | github | both`; reusable callers
  derive singular `lane`;
- `jackin-project/*` defaults to GitHub, while `tailrocks/*` and `ChainArgos/*`
  default to Velnor;
- public unmerged contributor code stays GitHub-hosted until lower-trust Velnor
  isolation is live-proven; no silent failover;
- trusted Velnor access uses exact workflow paths and refs; merge gates are
  exactly `ci-required` and `DCO`;
- missing capability is implemented in Velnor, never hidden by a repository
  workaround.

Source: `plans/README.md:3-17`. The canonical 28-repository class map is
preserved at `VELNOR_PROJECTS_SETUP.md:19-52`.

The repository-level readiness snapshot dated 2026-08-31T01:29:35Z recorded:

- branch `velnor1` at `f2f03637df1c173d014344f47dae8fd5fdb4f3b2`, equal to
  `origin/main` `2a6853ae395fd6c2401b48d989b6077c77d534d0`; clean tree and no
  source, fixture, workflow, package, APT, or Sentry mutation;
- unsigned `v0.1.247` at
  `738f18f68472c15e30645d81a7d2d664f29e5cab`; release runs `33344114108` and
  `33344405790` canceled; PR #502 rejected and unmerged;
- root campaign **4/94 done**, Plan 039 in progress, Plan 066 in progress, and
  Plan 079/final signed-release gates incomplete;
- public `velnor-apt` metadata dated `2026-08-29T17:48:19Z` served only
  `0.1.242` and `0.1.244`; no current-main package was published;
- no local GPG secret key and no pinned Sentry host-fingerprint/provenance
  proof; do not tag, publish APT, or touch Sentry from that snapshot.

Source: `plans/production-readiness/README.md:304-337`. These are snapshot
facts, not a claim about the state after that capture.

## 2. Historical incident record

### 2026-06-09 to 2026-06-11: service, release, and fleet failures

The incident table records these structural failures and recorded fixes:

| Incident | Measured symptom | Recorded structural response |
|---|---|---|
| #1 | All three daemons returned 401, 0 runners registered; literal `${VELNOR_GITHUB_TOKEN}` had been written to systemd env; labels were also rewritten. | Secret-file ownership, fail-fast token diagnosis, and no-exit retry loop. |
| #2 | 0 usable slots caused daemon exit/restart loops. | Keep the daemon alive; retry registration with bounded backoff. |
| #3 | Release YAML broke; apt remained at `0.1.0-rc12`. | Workflow linting and repaired release chain. |
| #4 | Fixed-slot churn produced 11–12.7 hour queues. | Dynamic-slot and no-idle-exit work. |
| #5 | Header deletion broke 20 source files and the lockfile; no compile CI existed. | Restore source/lock and add fmt, clippy, tests, actionlint, and aggregate CI. |
| #6 | Published `0.1.2` deb was 3.7 KB and lacked the binary; fleet outage lasted about 13 minutes. | Explicit binary asset plus deb size/content guards. |
| #7 | Fixture local composite action metadata was not found because eager checkout treated `success()` as runtime. | Keep trivial default conditions eager; longer-term ordered checkout. |
| #8 | `jsonwebtoken` provider configuration could panic at runtime. | Pin `rust_crypto` and `use_pem`; clean-room CI gate. |
| #9 | Zombie fleet: daemons active with `NRestarts=0`, but GitHub had no usable runners; `jm 0/10`, `brown 0/2`, `fixture 0/2`, `bcn 1/4`. | Truthful poll classification, 40-minute idle token refresh, 3-minute registry reconciliation, 4-hour max idle age, forensic logs. |
| #10 | Upgrade `0.1.19→0.1.20` killed 7 in-flight jobs. | Graceful drain in `0.1.21`, with `TimeoutStopSec=10800`; busy jobs finish before exit. |
| #11 | Empty shared store left dangling mise shims. | Image seeding before first container for each image identity. |

Source: `docs/master-plan.md:94-118`. The table is historical incident evidence;
the “fix” column records what the document says shipped at that time, not a
current verification.

The fleet-health investigation identified the enabling architecture: an idle
broker loop was treated as health while GitHub registration could be absent or
offline, so jobs queued forever despite an active process. During capture, the
four pools were `java-monorepo 0/10`, `jackin-agent-brown 0/2`,
`blockchain-nodes 1/4`, and fixture `0/2`. Doctor detected the split-brain but
did not heal it. The required correction was per-slot registry reconciliation
by `agent_id`, with recycle on missing/offline/mislabelled/over-age runners.

Sources: `docs/velnor-fleet-health-investigation-2026-06-11.md:5-17`,
`docs/velnor-fleet-health-investigation-2026-06-11.md:25-50`,
`docs/velnor-fleet-health-investigation-2026-06-11.md:54-61`,
`docs/velnor-fleet-health-investigation-2026-06-11.md:108-147`.

The 2026-06-11 stability sweep recorded 16 scenarios. `0.1.16` was reported to
fix OAuth expiry, broker migration, bounded completion retry (6 attempts,
5–60 seconds), a 2 GiB disk guard, salted backoff, pagination, 120-second OAuth
clock backdating with 300-second lifetime, delete-by-id recycling, and local
failure churn. Docker restart recovery, watchdog ping, pull backoff, and
completion spill-replay remained follow-ups; per-cycle Docker prune and job
CPU/memory caps were later recorded under `0.1.28` / commit `533a5c4`.

Source: `docs/stability-gap-audit-2026-06-11.md:1-32`.

### 2026-06-11: log-format regression

The live WebSocket feed must receive raw lines; uploaded step/job blobs must use
`YYYY-MM-DDTHH:MM:SS.fffffffZ <content>` with exactly seven fractional digits.
On 2026-06-11, run `27319096003` sent blob-formatted lines to the live feed,
causing doubled timestamps in the in-progress UI. Guard tests and a fresh
`lanes=both` visual comparison are required for future changes.

Source: `docs/log-format-contract.md:12-19`,
`docs/log-format-contract.md:22-50`.

### 2026-08-24: organization allowlist drift

The `tailrocks/velnor-trusted` group (id 3) had silently dropped to three
repositories while 21 Velnor-lane repositories existed; eight runners were
online and idle, but unlisted jobs queued indefinitely. The actor was not
recoverable from the operator token. Membership was restored, and the standing
rule became generated policy plus reviewed plan/audit/apply in the same change.

Source: `docs/org-fleet-migration.md:205-220`.

Plan 039's 2026-08-24 read-only evidence found all three groups present with
`visibility=selected`, but each had `restricted_to_workflows=false`,
`selected_workflows=[]`, and read-only workflow restrictions. The recorded
desired-policy digests were:

```text
tailrocks:      sha256:b9f497117c5a4d6bc13b48ac5dbc857de92f9465df06631fcd3d8cb516e8cd57
ChainArgos:     sha256:db3edaa1e0f2e058708fb3310bfc5ca9eca8cbe1c71cdeb76e33fe7ab47f68c0
jackin-project: sha256:97b13ff43e2132fc92fb34cbea4e34bca9c1754457b2899ece08a858ed39571f
```

The code surface was reported complete for generation/audit slices, but live
steps 3–5 remained operator-gated. Additional drift evidence included
unexpected repositories, a `137` versus `157` ledger count mismatch, and a
ref-shape conflict: the ledger admitted only `refs/heads/main` while about 25%
of recent Velnor runs used non-main refs. A fresh 2026-08-27 snapshot still
reported selected repos `21 / 6 / 9` and workflow restrictions disabled.

Sources: `plans/fleet-operations/039-org-jit-multi-repo-fleet.md:34-57`,
`plans/fleet-operations/039-org-jit-multi-repo-fleet.md:260-320`,
`plans/fleet-operations/039-org-jit-multi-repo-fleet.md:322-330`.

### 2026-08-26 to 2026-08-28: Sentry control-plane evidence

The Sentry recovery baseline at 2026-08-26 15:36–15:39 UTC recorded Docker
active and empty at idle, backend `docker`, a completed `ci-required` job, but
degraded GitHub/routing/group health and zero registered/executor-ready slots.
The document explicitly says systemd `active` alone is insufficient.

Sources: `plans/fleet-operations/039-sentry-recovery-2026-08-26.md:3-16`,
`plans/fleet-operations/039-sentry-recovery-2026-08-26.md:31-45`.

Historical observations and corrections:

- On 2026-08-27, read-only Sentry inspection found installed `velnor-runner`
  `0.1.215`, one idle waiter despite an empty Docker list, about 12h42m CPU in
  14h57m, and an approximately 82.7% live CPU sample; no mutation or canary
  claim was made. Source: `plans/issue-408-control-plane.md:435-442`.
- A supervised local 15-minute test on 2026-08-27 recorded 96/95 reconcile
  cycles, zero overlap/jobs/waiters/JIT mutations, WAL `189,552` bytes, CPU
  `0.0%`, and reconcile p95 ≤`14 ms`. This was local evidence, not production
  Sentry proof. Source: `plans/issue-408-control-plane.md:194-200`.
- Linux idle-scaling evidence measured `6153 / 4500 = 1.367x` controller CPU
  from 1 to 16 slots, under the ≤2x gate. A 2026-08-28 macOS recheck measured
  `1753` to `7428` (`4.24x`); it was explicitly not allowed to replace the
  Linux/Sentry result. Sources: `plans/issue-408-control-plane.md:290-306`.
- After `v0.1.242`, a deterministic synchronized idle CPU burst measured
  75–100% for one 10-second sample while job/churn counters were zero. Root
  cause was five sessions replaying `journal.db` independently; candidate
  `0.1.243` removed per-session replay, but required new signed-APT Sentry
  evidence. Sources: `plans/issue-408-control-plane.md:668-681`.
- On 2026-08-28, installed Sentry `0.1.242` reported seven broker errors
  (`401×5`, `404×2`), 22 registration-loss events, and 30 JIT create attempts;
  no live mutation was made. Source: `plans/issue-408-control-plane.md:725-732`.
- The same record says a real unchanged-fixture Velnor smoke (`33080712906`)
  passed app-a/app-b, provenance, comparison, and `compat-required`; the
  `both` parity run `33081151039` had GitHub success but Velnor parity stayed
  unproven. Replacement run `33083180030` passed result/cache/service/
  provenance checks, but the fixture's case-sensitive comparator skipped its
  own comparison. Sources: `plans/issue-408-control-plane.md:610-620`,
  `plans/issue-408-control-plane.md:635-653`.

## 3. Measured performance and capacity baselines

### 2026-06-11 initial fleet-limited pass

The first pass was not a full-estate benchmark: after cleanup, Velnor had
`java 0/10`, `brown 0/2`, `bcn 1/4`, and fixture `0/2`; only one blockchain slot
could run Velnor work. Recorded GitHub-only examples were:

| Workload | Run(s) | Cold / warm wall |
|---|---|---:|
| `tailrocks/velnor` CI | `27312139408`, `27312241157` | 1m15s / 1m21s |
| `tailrocks/holla` CI | `27312344231`, `27312401649` | 36s / 39s |
| `ChainArgos/jackin-agent-brown` CI | `27312463166`, `27312563136` | 2m11s / 1m58s |
| `ChainArgos/java-monorepo` Ansible | `27312660366`, `27312717034` | 43s / 37s |
| `ChainArgos/blockchain-nodes` both | `27312773945`, `27312875877` | 1m24s / 1m21s |
| fixture compat | `27312972269`, `27313029982` | 40s / 42s |

The requested Velnor full sweep was blocked and the required restoration target
was 2 brown slots, 10 java slots, 4 bcn slots, and 2 fixture slots.

Sources: `docs/ci-performance-report-2026-06-11.md:3-34`,
`docs/ci-performance-report-2026-06-11.md:36-52`,
`docs/ci-performance-report-2026-06-11.md:147-178`.

### 2026-06-11 three-round healthy campaign

The report labels this a healthy-fleet campaign: doctor `18/18`; r1 at
2026-06-10T21:53Z on `0.1.14` per-slot caches; r2 at 02:00Z on `0.1.16`
shared-cache cold fill; r3 at 02:10Z on `0.1.16` shared-cache warm. Rounds 2
and 3 were `22/22` green; r1 had three diagnosed failures.

| Workflow / lane | r1 | r2 | r3 |
|---|---:|---:|---:|
| `ansible.yml` Velnor sum seconds | 81 | 85 | 119 |
| `ansible.yml` GitHub sum seconds | 256 | 35 | 31 |
| `build-publish.yml` Velnor sum seconds | 150 | 179 | 205 |
| `compat.yml` Velnor sum seconds | 65 | 156 | 110 |
| `rust-docker.yml` Velnor sum seconds | 376 | 452 | 62 |
| `rust-docker.yml` GitHub bake sum seconds | 615 | 607 | 519 |
| `rust.yml` Velnor sum / max seconds | 3461 / 477 | 5185 / 643 | 2651 / 382 |
| `rust.yml` GitHub sum / max seconds | 6008 / 721 | 3187 / 528 | 3191 / 564 |

Run wall clocks were `rust.yml 749→664→571 s` (−24%),
`rust-docker.yml 643→635→539 s`, `ansible 263→93→127 s`, and
`docs 215→223→148 s`. The report's headline was Velnor rust-docker warm
`62 s` versus GitHub `519–607 s` (8.5×), and Velnor rust critical path `382 s`
versus GitHub `564 s` (−32%). Queue was 1–3 seconds when capacity was free and
87–100 seconds under saturated 10-slot contention. The reported residuals were
ansible/mise persistence, build-publish rust-script path visibility, and
compatibility MSRV downloads. The report also records zero zombies, zero
queue-forever events, zero restarts, and a 3m35s split-brain self-heal; its
24-hour soak was still described as continuing.

Source: `docs/ci-performance-report-2026-06-11.md:187-244`.

### 2026-07-18 storage and host denominator

The historical Sentry host baseline recorded XFS, 919 GiB total, 769 GiB used,
150 GiB available, and 84% use before cleanup. It later recorded 551 GiB free
after cache cleanup, 664,653,627,392 bytes free after stale-workspace cleanup,
and 635,698,315,264 bytes free on campaign recheck. The two invalid target roots
accounted for exactly `432,025,686,016` physical bytes:

- main: `233,025,220,608` bytes;
- Jackin: `199,000,465,408` bytes.

Other recorded values: `/var/lib/velnor` 310.4 GB, `/var/lib/velnor-jackin`
224.9 GB, Docker 260.3 GB, persistent targets about 432 GB, sccache about
21.6 GB, roughly 450,000 target files, 15,333 duplicated artifact keys, and
20–32 repetitions of common artifacts. Unowned SeaweedFS data was about 26 GB.

Source: `docs/host-baseline-2026-07-18.md:1-19`,
`docs/host-baseline-2026-07-18.md:21-56`, and
`docs/storage-and-disk-pressure-2026-07-18.md:26-53`.

The storage root cause was not merely “large caches”: identity/lifetime
ownership was incomplete, persistent target buckets were immortal accumulators,
multiple work roots hid state, and there was no global consumer view. The
recorded failure included `unknown-repository` fallback, depth-4 retention that
could not evict the only candidate, a destructive GC path that returned
“not implemented”, and inspection of the wrong `/root/.velnor/runner/_work`
root. Source: `docs/storage-and-disk-pressure-2026-07-18.md:55-80`.

### 2026-07-19 standardized CI campaign

The report accepted these dated workload results against the 2026-07-18 host
baseline and closing package `v0.1.98`:

- fastest accepted no-change library result: `21 s` (`pg-bigdecimal`);
- Velnor dogfood no-change: `33 s` versus a 90-second Class B budget;
- holla/ruxel: `57 s` / `52 s`;
- tracing: Velnor `23 s` versus GitHub `85 s`;
- accepted results had zero dependency downloads, dependency compilation, and
  tool-install markers;
- five repositories remained explicitly excluded by correctness or
  administrative blockers.

Named blockers were bcn package builds, Jackin Preview's rejected
`actions/attest-build-provenance@v4` surface, parallax's repeated 3,683-pixel
loading-skeleton divergence, termrock rustfmt drift, and tablerock's trunk-only
delivery policy. `audit-ci` reported 168 errors; the report did not misreport
that as zero-error.

Sources: `docs/ci-performance-report-2026-07-19.md:1-18`,
`docs/ci-performance-report-2026-07-19.md:20-57`,
`docs/ci-performance-report-2026-07-19.md:72-81`.

### 2026-08-25 speed research and correction

The research used completed GitHub API job records and bake logs fetched
2026-08-24/25. Historical baselines were:

| Workload | Measured value |
|---|---:|
| java-monorepo Docker build/publish, run `32832916776` | 188 s; queue ≤2 min |
| parallax Velnor dispatch | 5.5 min, including 425 s queue |
| tablerock Velnor vs GitHub | 5.2 vs 3.55 min |
| Jackin six micro-jobs | 0.3–0.5 min each; queue+boot 20–53 s |
| Jackin native-usage-menu-bar | 1592 s of a 1613 s run |

The status correction dated 2026-09-01 says Java PR #1976 is merged and measured
at `188 s` versus preceding no-registry-primary run `32830746424` at `504 s`:
`316 s / 62.7%` reduction. The 188-second result used GitHub endpoint
passthrough with registry-primary cache; it did not use Velnor's native cache
service. Native cache service proof still requires fixture roundtrip, signed
deployment, and controlled A/B evidence.

Sources: `docs/ci-speedup-research-2026-08-25.md:1-21`,
`docs/ci-speedup-research-2026-08-25.md:23-31`,
`docs/ci-speedup-research-2026-08-25.md:71-84`.

The research also measured Docker lifecycle contention: isolated boot was
0.4–3.7 s and teardown 0.6–5.9 s, while an eight-slot burst produced
11.6–70.5 s boot and 12.1–42.1 s teardown tails. Full serialization removed
contention but made the gate itself a queue, with boot reaching 42.7 s; the
bounded gate and phase-scoped permits were the recorded correction.

Source: `docs/ci-speedup-research-2026-08-25.md:238-263`.

### 2026-08-26 journal hot-path microbenchmark

A local Rust benchmark with 5,000 events and 16 reads measured `317.638 ms` for
replay versus `0.612 ms` for materialized reads, about `519×` faster and `99.8%`
lower read time. The record says this was local-only; live Sentry before/after
timing remained required after signed APT publication.

Source: `docs/ci-speedup-research-2026-08-25.md:304-317`.

## 4. Research conclusions and architecture decisions

### Cache and warm-run conclusions

The instant-cache investigation measured before-state clippy at `1m58s` with
about 280 fresh crates, Docker bake at `2m44s` including about 40 seconds GHA
cache export, and Kestra at `48s` with tool downloads. Root causes were a false
`HOME=/root` / `CARGO_HOME=/root/.cargo` prelude, unmounted container paths,
per-slot stores, tar/export overhead, cargo-chef mismatch, and an ephemeral tool
image. Source: `docs/perf-instant-cache-plan-2026-06-11.md:1-16`,
`docs/perf-instant-cache-plan-2026-06-11.md:17-39`.

The Velnor-side performance design measured jobs at 2m55s–5m18s for Rust,
6m15s for rust-docker, 36s for Ansible, 23s/job for bcn, 74s for brown, and
32s fixture compat; pickup was about 2s. It ranked host-shared sccache,
persistent repo/target volumes, local buildx, git mirrors, async finalization,
prewarm, path/key correctness, bounded JIT overlap, and log batching. Projected
warm totals were estimates, not acceptance measurements.

Sources: `docs/p3-performance-design-2026-06-11.md:1-22`,
`docs/p3-performance-design-2026-06-11.md:24-62`,
`docs/p3-performance-design-2026-06-11.md:64-77`.

The later cache conclusion is: keep capped sccache as default; evaluate Kache
only after the host controller works and in an isolated trusted canary. Kache's
recorded candidate properties were 50 GiB default limit, trigger above 110%,
evict toward 90%, and reflink/hardlink/copy restoration; its host-container
bind-mount topology was not proven for Velnor. Source:
`docs/storage-and-disk-pressure-2026-07-18.md:296-325`.

The strict cache surface is local-only: exactly one of `sccache | kache | off`,
no remote compiler cache, sccache `v0.16.0` with `SCCACHE_CACHE_SIZE=20G`, and
Kache `v0.14.2` with `github-cache=false`, `max-size=20GiB`.

Sources: `docs/strict-capability-contract.md:114-157`,
`docs/strict-capability-contract.md:159-184`.

### Storage and GC conclusions

The accepted storage direction requires one canonical path/class/trust/owner/
budget/lifetime/delete rule per byte, filesystem-wide capacity control,
physical allocation accounting, active-job leases, a leader lock, generations,
reclaim-before-register, and no broad Docker/filesystem prune. Pressure states
are `normal`, `soft`, `hard`, `emergency`, and `uncertain`; only `normal` admits
new work. Sources: `docs/storage-and-disk-pressure-2026-07-18.md:10-24`,
`docs/storage-and-disk-pressure-2026-07-18.md:140-205`,
`docs/storage-and-disk-pressure-2026-07-18.md:241-293`.

The old `docs/cache-gc-design.md` is explicitly superseded and read-only. It
describes the original `_velnor_*` work-root spike and must not authorize
deletion. Source: `docs/cache-gc-design.md:1-17`,
`docs/cache-gc-design.md:53-74`.

### Control-plane conclusions

Issue 408's structural target is guardian → per-scope controller → stable slot
process → transient job worker, with bounded reconciliation, recovery
coordination, durable handoff, and no idle waiter/job worker processes. Local
tests recorded 1,387/1,387 nextest and the 96/95-cycle no-work soak, but the
same evidence says fixture readiness, Sentry canary/full-fleet proof, and signed
APT evidence were unavailable. Sources: `plans/issue-408-control-plane.md:32-100`,
`plans/issue-408-control-plane.md:179-200`.

### Capability, adapter, and trust conclusions

The strict capability contract requires complete expanded-job validation before
checkout, cache mutation, service startup, or container creation, and rejects
unknown refs, inputs, values, expressions, backends, and combinations with
precise errors. Source: `docs/strict-capability-contract.md:6-18`,
`docs/strict-capability-contract.md:37-45`.

The older Phase 0 native-adapter note says supported adapters ignore marketplace
SHA/tag refs and use internal Rust behavior. That statement is historical and
conflicts with the later strict immutable-ref manifest; it is not a current
execution claim. Sources: `docs/native-action-adapter-contract.md:1-12`,
`docs/native-action-adapter-contract.md:40-68`;
`docs/strict-capability-contract.md:8-13`.

Build L3 remains design-only. The boundary document says tenant work, guest
Docker, BuildKit, writable job objects, and imported cache payloads are outside
the TCB; the controller must verify full-peak blocks/inodes, and uncertain
accounting stops admission. It explicitly says implementation/live proof are
not complete. Sources: `docs/security/build-l3-boundary-v1.md:1-22`,
`docs/security/build-l3-boundary-v1.md:24-45`,
`docs/security/build-l3-boundary-v1.md:80-129`.

The attestation proposal is operator-approved only for Jackin Preview at exact
PR #810 head `a27fcdaff7ac4c9562eb01c468f2d45aac0dac6c`, action ref
`0f67c3f4856b2e3261c31976d6725780e5e4c373` (`v4.1.1`), subject
`dist/*.tar.gz`, and permissions `contents:read`, `id-token:write`,
`attestations:write`. It rejects all other inputs and still requires fixture,
GitHub/Velnor parity, no-credential-leak, and Jackin proof. Sources:
`docs/capability-proposal-attest-build-provenance-v4.md:1-15`,
`docs/capability-proposal-attest-build-provenance-v4.md:17-53`,
`docs/capability-proposal-attest-build-provenance-v4.md:138-160`.

### Cancellation and log conclusions

The broker-outage analysis checked `actions/runner` on 2026-07-05 and found
renewal sends only `planId` and `jobId`, returns only `LockedUntil`, and does not
carry cancellation state. Velnor must not infer user cancellation from renewal
failure; cancellation remains a `JobCancelMessage` path. Source:
`docs/cancellation-during-broker-outage.md:1-35`.

## 5. Frozen upstream references

### `actions/runner`

The dated upstream refresh checked `actions/runner` `v2.337.0`, tag commit
`397b032cbf865e9c3ddfab89d533ec19325e1273`, against previous `v2.335.1` commit
`7d737449ef346f6524f75688d0c9c95fa10ba10a`. It records broker/session recovery
deltas while saying run-service acquire/renew/complete request shapes and V2
message flow remain unchanged. Source: `docs/reference/latest-runner-v2-refresh-2026-06-01.md:1-18`.

The refresh anchors V2 selection in `Runner.cs:393-403`, broker requirements in
`BrokerMessageListener.cs:85-92`, broker poll fields in
`BrokerMessageListener.cs:293-299`, and run-service parsing in
`Runner.cs:680-703`. Velnor must re-run its reference check before claiming
latest compatibility. Source: `docs/reference/latest-runner-v2-refresh-2026-06-01.md:20-49`.

The attestation proposal's `actions/runner` `v2.336.0`, commit
`98aabcd429c4e8402406c56ce2d26387fed3b9ce`, is an older frozen implementation
reference for that proposal only. It is not the current upstream identity in
this record. Source: `docs/capability-proposal-attest-build-provenance-v4.md:117-136`.

### Firecracker and L3 packaging

ADR 0001 was accepted 2026-08-26. It fixes exactly two selectable backends,
`docker` and `microvm`; microVM is direct Firecracker VMM+jailer on Linux KVM,
one guest/agent/Docker daemon per job, no host-Docker fallback. Firecracker is
pinned to `v1.16.1`; the cited `<125 ms` boot and `<5 MiB` overhead are spec
numbers, not Velnor measurements. Sources:
`docs/adr/0001-firecracker-production-microvm.md:1-39`.

The 2026-08-26 Sentry preflight captured `/dev/kvm`, cgroup v2, XFS
`reflink=1`, nftables, installed `velnor-runner 0.1.214`, apt candidate
`0.1.215`, Firecracker/jailer `1.16.1`, kernel `6.1.102`, and a passed
microVM preflight. `/run` `noexec` blocked jailer there; the proposed isolation
root was `/var/lib/velnor/microvm`. Guest Docker/vsock and an estate microVM
workflow pass remained unproven; crate `0.1.216` was not an apt candidate.

Source: `docs/adr/0001-firecracker-production-microvm.md:49-77`.

### Attestation and action registry

Frozen attestation chain: action `v4.1.1` ref
`0f67c3f4856b2e3261c31976d6725780e5e4c373`, delegate
`a1948c3f048ba23858d222213b7c278aabede763`, and `@actions/attest 3.2.0`.
Source: `docs/capability-proposal-attest-build-provenance-v4.md:117-132`.

The action registry records these major surfaces: checkout v6, cache v5,
upload-artifact v7, download-artifact v8, upload-pages-artifact v5,
configure-pages v6, deploy-pages v5, attest-build-provenance v4.1.1/v4.2.2,
paths-filter v4, mise-action v4, setup-just v4, rust-cache v2, sccache-action
v0, mold v1, runtime v4, Renovate v46, QEMU v4, cosign v4, hadolint v3,
Docker login v4, setup-buildx v4, metadata v6, build-push v7, and bake v7.
Source: `docs/reference/target-action-registry.md:41-82`.

That registry also contains a stale runtime note tracking `actions/runner`
`v2.334.0`; it must not override the dated `v2.337.0` refresh. Source:
`docs/reference/target-action-registry.md:93-104`.

## 6. Acceptance evidence and explicit non-claims

| Area | Recorded evidence | Status preserved |
|---|---|---|
| June fleet health | `0.1.15` code/live split-brain recovery and doctor results; later `0.1.16` hardening. | Historical; 24-hour soak was pending in the source. |
| June performance | Rounds 2/3 `22/22` green; warm cache speedups and residuals recorded. | Historical baseline, not current estate proof. |
| July standardized CI | Accepted no-change samples and zero download/compile/tool markers. | Five repositories excluded; no universal claim. |
| Plan 039 | Deterministic generation/audit slices and exact digests. | Open: apply, exact closure/ref ruling, runner state, routing/denial/warm proof, operator approval. |
| Plan 066 | Historical `0.1.186–0.1.188` hold/cancel run `32787266979`; current-SHA local gates 1,084 runner tests and 1,478 workspace tests. | Open: current-SHA fixture hold/cancel, target evidence, independent verifier/reviewer. |
| Issue 408 | Local control-plane, scaling, isolation, and full-suite tests. | Open: fixture readiness/smoke, active-job faults, signed APT, Sentry/full-fleet soak, promotion. |
| MicroVM | Sentry KVM/checksum/jailer preflight. | No guest-Docker, vsock, same-host VMM comparison, or estate microVM pass claim. |
| Build L3 | Frozen design and threat/control matrix. | Design-only; consumers reject Velnor-signed provenance until live proof. |
| Attestation | Operator-approved exact Jackin surface. | Implementation/fixture/Jackin parity proof incomplete. |
| Native cache service | #378 and #387 landed; v1/v2 support and URL enablement recorded. | Fixture roundtrip, signed deployment, controlled A/B still required. |

Sources: `plans/velnorctl-migration/066-operational-history-and-events.md:18-34`,
`plans/velnorctl-migration/066-operational-history-and-events.md:36-58`;
`plans/issue-408-control-plane.md:282-289`; `docs/adr/0001-firecracker-production-microvm.md:63-77`;
`docs/security/build-l3-boundary-v1.md:3-9`; `docs/ci-speedup-research-2026-08-25.md:10-21`.

The production-readiness plan defines a stricter rule: `TODO`, `RUNNING`,
`BLOCKED`, `WAITING`, `UNKNOWN`, `UNPROVEN`, and historical evidence never
count as complete; local green tests or partial workflow success cannot infer
completion. Source: `plans/production-readiness/README.md:1-11`.

## 7. Migration and plan ledger

The durable goal inventory audited at Velnor commit `77b2b66` on 2026-08-24 is:
Plan 039 + Plans 063–080 (18 shared items) + C001–C075 (75 command items) =
94 executable items. Source: `plans/goal-execution/README.md:8-24`.

Plan state recorded by the migration index:

- 063: DONE, historical direction/fixture contract;
- 064: DONE, scaffold/workspace/CLI seams;
- 065: DONE, resource/rendering/CLI conventions;
- 066: IN PROGRESS, durable history/events;
- 067–078: TODO shared API, config, query, logs, observation, health,
  lifecycle, GitHub, storage, Debian, capability, and diagnostics services;
- 079: TODO final package/binary cutover and removal of `velnor-runner`;
- 080: TODO authenticated remote transport and fleet views.

Source: `plans/velnorctl-migration/README.md:76-100`.

Research coverage preserved the exact arithmetic: 79 supplied command leaves,
minus five removed release commands, plus one `velnorctl daemon` task = 75
command tasks. Debian `apt`/`dpkg` are the native installed-version authority;
there is no Velnor release resource or compatibility alias. Sources:
`plans/velnorctl-migration/RESEARCH_COVERAGE.md:1-20`,
`plans/velnorctl-migration/RESEARCH_COVERAGE.md:57-114`.

Command status is one completed row only: C005 `velnorctl man` is DONE; C001–C004
and C006–C075 are TODO in the command index. Every row requires typed Clap,
versioned output, stable errors, nextest/check, exact fixture proof, sanitized
evidence, and no backward-compatible alias. Sources:
`plans/velnorctl-migration/commands/README.md:9-36`,
`plans/velnorctl-migration/commands/README.md:40-116`,
`plans/velnorctl-migration/commands/README.md:121-130`.

The final migration is intentionally breaking: only `velnorctl` and its
service-only `daemon` remain; Plan 079 deletes the old crate, binary, Debian
package, systemd invocation, release surface, and aliases after all command
rows are green. Source: `plans/velnorctl-migration/README.md:6-22`,
`plans/velnorctl-migration/README.md:129-179`.

The old plan families 001–038 and 040–062 are explicitly historical and
non-executable; Git history preserves their delivery evidence. Source:
`plans/README.md:50-52`.

## 8. Known gaps and reconciliation obligations

These remain open or require fresh evidence; this record does not close them:

1. Campaign completion is still 4/94; Plan 039, Plan 066, Plan 079, and final
   signed-release gates remain unresolved at the dated readiness snapshot.
2. Plan 039 lacks operator-approved exact policy application, exact workflow/ref
   closure, runner registration/assignment proof, denial/routing/warm evidence,
   and full guard-state acceptance. Its 2026-08-27 groups still had workflow
   restrictions disabled.
3. Fixture readiness and current-HEAD fixture hold/cancel/smoke/parity evidence
   are not uniformly green. External fixture defects, unavailable trusted
   runners, missing private job images, and a comparator path-case defect were
   recorded; Velnor-side workarounds are forbidden.
4. Sentry has historical registration/session churn, 401/404 errors, stale
   waiter/zombie observations, and an invalidated idle-soak history. A clean
   idle window, active-job/isolation fault proof, promotion, and full-fleet soak
   remain required.
5. Signed APT release provenance, current-main package publication, final
   installation, rollback, forward recovery, and all-lane evidence remain open;
   a local unsigned tag or package is not an installation authority.
6. MicroVM has preflight only. Guest Docker, vsock delivery, same-host
   Firecracker comparison, signed deployment, and estate workflow evidence are
   absent. Build L3 remains unproven.
7. Native cache service has landed but its fixture roundtrip, signed deployment,
   and controlled A/B measurement are absent. Kache's Velnor mount topology is
   also unproven.
8. Storage has a strong design and historical cleanup numbers, but acceptance
   still requires bounded steady-state use, no ENOSPC, no active-cache deletion,
   deterministic admission, deletion forensics, trust isolation, and warm
   no-download/no-recompile reruns. Source:
   `docs/storage-and-disk-pressure-2026-07-18.md:498-526`.
9. Documentation has known historical contradictions requiring reconciliation:
   the action-adapter Phase 0 note says pinned marketplace refs are ignored,
   while strict capability requires immutable manifest refs; the migration
   research names `/var/lib/velnor/state.db`, while the storage contract names
   `/var/lib/velnor/storage.db`; the action registry's runtime note says
   `v2.334.0` while the latest refresh says `v2.337.0`. Sources:
   `docs/native-action-adapter-contract.md:40-68`,
   `docs/strict-capability-contract.md:6-18`,
   `plans/velnorctl-migration/RESEARCH_COVERAGE.md:70-89`,
   `docs/storage-and-disk-pressure-2026-07-18.md:140-145`,
   `docs/reference/target-action-registry.md:93-104`.
10. The historical production-readiness checkpoint `RESUME.md` is explicitly
    non-executable: its branch/HEAD `0dd27f5ac8a0a51a32d3ac9d29817f7fb61e98ef`,
    PR, CI, live validation, merge, deploy, and PRD completion were all not
    done. Source: `plans/production-readiness/RESUME.md:1-10`,
    `plans/production-readiness/RESUME.md:29-78`.

## 9. Source coverage

The read pass covered every Markdown source currently present in the repository
that is a report, incident record, ADR, proposal, reference, runbook, plan,
plan index, or command task. The plan inventory itself requires reading every
executable plan and every command file before action
(`plans/goal-execution/README.md:47-56`). Coverage groups:

- Historical reports and investigations: `docs/ci-performance-report-2026-06-11.md`,
  `docs/ci-performance-report-2026-07-19.md`, `docs/ci-speedup-research-2026-08-25.md`,
  `docs/host-baseline-2026-07-18.md`, `docs/p3-performance-design-2026-06-11.md`,
  `docs/perf-instant-cache-plan-2026-06-11.md`,
  `docs/velnor-fleet-health-investigation-2026-06-11.md`,
  `docs/stability-gap-audit-2026-06-11.md`, and `docs/master-plan.md`.
- ADR, proposal, security, and contracts: `docs/adr/0001-firecracker-production-microvm.md`,
  `docs/capability-proposal-attest-build-provenance-v4.md`,
  `docs/security/build-l3-boundary-v1.md`, `docs/strict-capability-contract.md`,
  `docs/storage-and-disk-pressure-2026-07-18.md`, `docs/cache-gc-design.md`,
  `docs/action-foundations.md`, `docs/log-format-contract.md`, and
  `docs/cancellation-during-broker-outage.md`.
- Runbooks and operational references: `docs/target-live-runbook.md`,
  `docs/debian-apt-repo.md`, `docs/runner-usage.md`, `docs/org-fleet-migration.md`,
  `docs/required-check-handoff.md`, `docs/lane-compare-watch-design.md`,
  `docs/comparison.md`, `docs/reference/velnorctl-commands.md`,
  `docs/reference/latest-runner-v2-refresh-2026-06-01.md`, and
  `docs/reference/target-action-registry.md`.
- Supporting design/current references: `docs/rust-build-cache-hygiene-velnor.md`,
  `docs/estate-capability-matrix.md`, `docs/rust-automation-policy.md`,
  `tools/unit-collector/README.md`, `SECURITY.md`, `README.md`,
  `docs/mission.md`, `docs/vision.md`, `docs/roadmap.md`, `docs/prompt.md`,
  `docs/README.md`, and `VELNOR_PROJECTS_SETUP.md`. The cache-hygiene note
  preserves the bounded-growth conclusion and the requirement for a real
  Parallax attribution report (`docs/rust-build-cache-hygiene-velnor.md:1-7`,
  `docs/rust-build-cache-hygiene-velnor.md:46-75`,
  `tools/unit-collector/README.md:48-50`). The estate matrix preserves
  the class/backend capability boundary and explicitly does not claim an estate
  microVM pass (`docs/estate-capability-matrix.md:1-35`). Release trust and
  signed-APT ownership remain documented in `SECURITY.md:7-26`; Rust automation
  and committed evidence rules remain in `docs/rust-automation-policy.md:1-44`.
- Plan records: `plans/fleet-operations/039-org-jit-multi-repo-fleet.md`,
  `plans/fleet-operations/039-sentry-recovery-2026-08-26.md`,
  `plans/issue-408-control-plane.md`, `plans/production-readiness/README.md`,
  `plans/production-readiness/RESUME.md`, all Plans 063–080, and all C001–C075
  command files. Their authoritative indexes preserve the 94-item inventory,
  plan statuses, and 75-command status map at
  `plans/goal-execution/README.md:8-24`,
  `plans/velnorctl-migration/README.md:76-120`, and
  `plans/velnorctl-migration/commands/README.md:40-116`.

No item in this source list is a current runtime certification unless the
current authority and acceptance gates separately prove it.
