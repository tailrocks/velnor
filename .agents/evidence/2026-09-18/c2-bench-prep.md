# C2/E2 benchmark prep — provisional native N + mixed-engine N (bastion campaign, read-only design, no execution)

Authority: `plans/bastion-three-provider-ci/spec.md` §4.4; work-plan STEP C2 action 5, STEP E2 action 3.
Baseline input: `/tmp/a1-timing.md` (main green run 35159519365, wall 14m24s; critical path Docker/GitHub `Run unit checks` 12m56s cold vs ~50s warm on PR).
Host (re-verify read-only at execution): bastion `root@37.27.110.241`, Debian 13, AMD EPYC 9454P, 48 physical / 96 logical CPUs, ~128 GB RAM.

N ladder: 16 → 24 → 32 → 48 → 64 → higher (80, 96) only while stable useful throughput improves.
48 = physical core count; 64 sits between physical and logical; stop rule in §3 applies to both C2 and E2.

---

## 1. C2 — provisional native-only N

### 1.1 Representative workload choice

- Workload = the full Velnor affected/full Linux unit inventory executed through the native lane only
  (17-unit baseline per work-plan E1 action 1, or its ledger-recorded coverage-equivalent correction).
  Rationale: it is the exact work N will carry in production; no synthetic micro-benchmark substitutes.
- If full-inventory waves prove operationally infeasible at execution, fallback is a fixed declared subset
  that MUST include: `docker` (critical-path unit, 12m56s cold / ~50s warm per A1 baseline),
  `rust-velnor-runner` (next critical path, ~4m), `rust-production-topology`, and at least one
  fast unit (`docs`, `opentofu`) to expose queue/provisioning overhead on short jobs.
  The subset is declared in the ledger before the first wave; changing it mid-ladder invalidates the ladder.
- Each N point = one full workload wave (all units dispatched once) PLUS steady-state behavior observed
  across the wave; a single wave per N per temperature (cold, warm) is the minimum, two waves preferred
  where the second confirms the first within noise.

### 1.2 Comparable-conditions protocol (only N changes)

Fixed across the whole ladder; record each value in the ledger before wave 1:

- Source: one pinned source SHA for all waves (no merges mid-ladder; if source must move, restart the ladder).
- Logical plan: one plan digest — same planner, same unit set, same command/profile/features/fixtures,
  same test expectations. No plan regeneration between N points.
- Image/toolchain identity: same pinned image digests (job/toolchain images), same runner/engine versions.
- Temperature (two sub-series per N, never mixed within a wave):
  - COLD: fresh checkouts, benchmark-namespaced caches evicted (owned inactive resources only —
    no host-wide `docker system prune`, per spec §6); images pre-pulled per spec §4.1
    (pre-pull is the sanctioned warm-inventory substitute; record pre-pull completion before each cold wave).
  - WARM: caches populated by one immediately preceding identical wave at the same N; nothing evicted
    between the primer and the measured wave.
- Host state: no package/config changes mid-ladder; no unrelated load introduced by the operator;
  disk/inode headroom verified before each wave (see §1.4 stop rules).
- ONLY host-level `max_jobs=N` changes between points. Containers remain unbounded at every point
  (C2 quota-free proof, work-plan C2 actions 2–3, is a precondition of wave 1 and is spot-rechecked
  per wave: HostConfig + cgroup ancestry sample on at least one running job container per wave).

### 1.3 Full metric set (every wave)

Record per wave and per N point; per-job rows retained for phase analysis:

- Throughput: completed jobs/minute over the steady window (first start → last terminal, queue drain excluded
  from the rate denominator — report both wall and steady-window rates).
- Time-to-green: wave dispatch → last expected result terminal (end-to-end, includes queue).
- Queue + provisioning delay: per job — `first_seen_at` → permit grant (queue delay);
  grant → runner connected / native worker running (provisioning delay). Report p50/p95/max.
- Phase timings per job: reserve/acquire/provision, checkout, compile, test, cache save/restore,
  cleanup (owned cleanup ≤120s target per spec §8), diagnostic export.
- CPU: host `cpu.max`-free utilization (user/sys/iowait/steal), load average, per-container CPU time sample.
- Memory: available mem low-water mark, PSI memory (some/full) avg/p95, swap activity (must be ~zero),
  OOM-killer events (dmesg/journal; any OOM → diagnostics preserved per spec §4.3, wave flagged).
- Disk/inodes/IO: free bytes + free inodes before/after each wave, IO latency (await/p95 via iostat or
  PSI io), BuildKit/Cargo cache store sizes.
- Contention: Docker daemon API latency/errors, BuildKit concurrent-build queueing, cache-lock wait
  (`ScopeLease` acquire latency), DinD-absence confirmation (native lane: mediated lease only).
- Failures: per-unit outcome, failure class (test failure vs infra failure vs timeout vs OOM),
  retries/redeliveries, cleanup receipts. Any infra-caused failure restarts that N point after root-cause;
  test-content failures freeze the ladder (source must be green — benchmark measures capacity, not debugging).

### 1.4 Unrelated-jobs protection + stop rules

- Benchmark demand enters as ordinary eligible demand: FIFO by `first_seen_at`, no priority bypass,
  no reservation carved out of N. Normal main runs are preserved (spec §6); waves are scheduled in
  coordinated windows around them, never by cancelling sibling/branch work.
- Live stop rules (abort current wave, preserve diagnostics, hold the ladder): host OOM storm
  (>1 OOM-kill outside the benchmark's own jobs, or any SSH/control-plane endangerment), disk/inode
  pressure crossing the implemented admission policy, Docker daemon unresponsive >5 min,
  unrelated-job failure attributable to benchmark load. Aborted wave data is kept and labeled, never silently dropped.
- Stability bar for "N qualifies": zero infra-caused failures, zero OOM-kills, queue p95 not exploding
  vs the previous rung (queue growth must be sublinear in offered load), useful throughput (green jobs/min)
  strictly higher than the previous rung. First rung that fails the bar ends the ladder; provisional N =
  highest passing rung. The highest stable useful throughput wins, not the largest integer.

### 1.5 C2 outputs per work-plan

Quota-free inspection bundle + no-raw-socket proof (precondition), health + native smoke job URL,
occupancy/cleanup ledger, and the provisional-N benchmark data table (per-N: all §1.3 metrics, cold and warm).

---

## 2. E2 — mixed-engine N (cold + warm)

Same ladder (16/24/32/48/64+), same stop rules (§1.4), same metric set (§1.3) with these deltas:

### 2.1 Workload and engine split

- Workload = full Velnor unit×provider matrix on the two local engines: every eligible Linux unit fans out
  to `github-self-hosted` (official/Scale Set lane) AND `velnor` (native lane) with identical
  source/command/profile/features/fixtures/expectations (work-plan E1). GitHub-hosted jobs run as usual
  but consume no bastion N and are excluded from all throughput/delay denominators.
- Engine mix is NOT fixed-ratio: both engines draw from the single shared `max_jobs=N` ledger under
  oldest-observed-first admission (spec §4.1–§4.2). The observed per-engine split is a measurement,
  not a control — record offered/acquired/running/completed counts per engine per wave.
- Per-engine metric split (added to every §1.3 metric): jobs/min, time-to-green contribution,
  queue vs provisioning delay (Scale Set provisioning includes DinD-ready + runner-connected legs),
  phase timings (official lane adds JIT/provision/image-pull legs), failures by engine.
- Serial test groups respected: Jackin Docker-E2E / ChainArgos RustFS style serialization is test
  semantics and stays intact; benchmark waves never override intentional serial groups for speed.
- Scale Set-specific contention added to §1.3 contention set: session/acquire latency, JIT config
  latency, DinD daemon startup time, per-worker DinD disk growth, runner-image pull time (cold).

### 2.2 Temperature protocol (cold + warm, explicit)

- COLD wave: both engines cold — owned benchmark caches evicted on native side, fresh DinD data
  directories on the official side (private per worker, no reuse), all images pre-pulled.
- WARM wave: primer wave at same N immediately before; native caches retained, official DinD
  BuildKit cache exported/imported per spec §6 (never a shared DinD data dir).
- Cold and warm are separate reported series per N; the E2 N decision weighs both (cold bounds
  worst-case main-run recovery; warm bounds steady PR throughput).

### 2.3 No fair-compare-vs-hosted rule (spec §4.4, last sentence)

- NEVER present bastion-vs-github-hosted timing parity (or delta) as a performance comparison:
  hosted hardware is different and uncontrolled. All E2 perf claims are same-host before/after only:
  N-to-N on bastion, cold-vs-warm on bastion, C2-native-only vs E2-mixed on bastion at equal N.
- Hosted-lane results in E2 waves serve correctness parity (same tests, same expectations) and the
  strict expected-result set — never throughput rivalry. Any report table with a hosted column labels it
  `correctness reference — not a perf baseline`.

### 2.4 E2 outputs per work-plan

Mixed-engine N evidence table (per-N × cold/warm × per-engine split, full §1.3+§2.1 metrics),
monotonic ≤N ledger (total occupied across both engines never exceeds N), fault-matrix records
(separate procedure), three triple-green run URLs + PR URL, gate report.

---

## 3. Shared run procedure (both ladders)

1. Pre-ladder: record source SHA, plan digest, image/toolchain digests, package version, N ladder,
   workload definition (full inventory or declared subset), temperature order (cold-first per N recommended:
   cold wave doubles as the warm primer). Verify quota-free (C2) / repeated quota-free (E2).
2. Per N: verify disk/inode headroom → pre-pull images → COLD wave → collect §1.3 metrics →
   WARM wave (no eviction between) → collect metrics → spot-check quota-free sample →
   apply stability bar (§1.4) → pass: advance; fail: ladder ends.
3. Post-ladder: provisional/mixed N = highest passing rung; full evidence table to ledger;
   author/verifier pair review per work-plan §0.2 before the N value is consumed by any gate.
4. Requalification trigger: N is remeasured after Jackin/ChainArgos onboarding and after any package,
   image-major, or workload-shape change (spec §4.4: "N is requalified").
