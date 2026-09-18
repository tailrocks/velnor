# F2 parity prep — Jackin full-parity capture procedure (design only, no execution)

Status: **read-only design** for work-plan STEP F2 (`plans/bastion-three-provider-ci/work-plan.md` F2).
Authority: `spec.md` §9.1 (Jackin) + §2 (providers/result identity) + §4 (capacity/quotas/N) + §8 (hosted verifier/fault contract) + work-plan §0.8 (gate regression).
Inputs: `/tmp/e1-matrix-prep.md` (Velnor unit×provider capture pattern), `/tmp/f1-swift-prep.md` (routing fix), `/tmp/f1-e2e-probe.md` (E2E surface), `/tmp/f1-release-prep.md` (release reconciliation), `evidence.md` §4 (Jackin inventory boundary).

No command in this procedure mutates bastion, GitHub state, or any repo. All capture steps are reads
(`gh api`, `gh run view`, artifact/JUnit download, generated-tree/manifest inspection, live
container/cgroup inspection). Execution belongs to the F2 author/verifier pair at campaign time.
Gate check: E1+E2 signed off AND F1 signed off. Read-only Jackin research may have run early;
F2 capture starts only after F1.

## 1. Capture shape (114 + 2 + E2E/controls — NEVER 120 Linux)

Expected baseline (evidence.md §4 + spec §9.1): **40 units** = 36 Rust + 1 Bun + 1 Docker + 2 Swift.

| Block | Executions | Rule |
| ----- | ---------- | ---- |
| 38 Linux-oriented units × 3 providers (`github-hosted`, `github-self-hosted`, `velnor`) | **114** | Same source SHA, command/profile/features, fixtures, expectations on all 3 lanes; only provider lane differs (spec §2 identical-inputs rule) |
| 2 Swift units on genuine hosted macOS (Apple Silicon, `macos-26` per Jackin config) | **2** | Hosted lane ONLY — no Velnor lane exists for Swift (`lane_supports_unit_kind`); same-job XCFramework production → `swift build` → `swift test` preserved (`/tmp/f1-swift-prep.md`); never counted as Linux |
| `docker-e2e` with `e2e` enabled (one top-level job; §5) | counted separately | Explicit E2E job, not a 39th unit |
| Control jobs (Policy, Planning, hosted verifier, telemetry writer) | counted separately | Excluded from the workload set (spec §8) |

**116 unit executions + E2E + controls.** "120 Linux executions" is VOID as a count — it can only be
reached by pretending Swift runs on Linux (spec §9.1). Any ledger showing 120 Linux fails F2 on sight.

## 2. Unit inventory procedure (do not invent IDs)

evidence.md §4 records kind counts but NO full per-unit table. F2 enumerates IDs live:

1. Read the generated unit manifest (`.github/ci/project.toml` + generated workflow callers) at the
   qualification source SHA. List all 40 unit IDs with kind + root + watch set.
2. Assert kind counts 36/1/1/2. Any delta vs evidence.md §4 is a **coverage change**: record
   added/dropped/renamed unit, reason, and F1 coverage-ledger justification (work-plan F1 action 4:
   "confirmed 40-unit ledger or justified correction"). An unrecorded delta fails F2.
3. Assert the 2 Swift IDs match the F1 routing fix scope (`swift-package-native` +
   `swift-package-native-design-prototypes-unifiedagentusage`, or their corrected names) and that both
   route to `runs-on: macos-26` in the generated `ci-unit-swift.yml` (single `verify-github` job).
4. Assert NO Swift unit appears in any Linux provider matrix (the 114 cells are Swift-free).
5. Assert `per_mount_isolation_e2e` stays in the `default` profile (hermetic, daemon-free —
   `/tmp/f1-e2e-probe.md` §b); it must NOT be moved to `docker-e2e` nor silently dropped from default.

## 3. Per-cell capture record

One record per (unit × provider) for the 114 Linux cells; one record per Swift unit for the 2 macOS
executions. A cell without ALL fields is incomplete, not green (mirror of E1 §2):

| # | Field | Content | Source |
| - | ----- | ------- | ------ |
| 1 | `unit_id` | one of the §2-enumerated IDs | planner output |
| 2 | `provider` | exact canonical ID (`github-hosted`, `github-self-hosted`, `velnor`); Swift cells: `github-hosted` + `platform: macos` | planner output |
| 3 | `engine_identity` | official lane → runner version + Scale Set worker identity; `velnor` → native executor version (NOT DinD, spec §5.4); hosted → runner image/version; Swift → macOS runner image/version + arch proof (Apple Silicon, not Intel) | job logs + runner metadata + provisioning IDs |
| 4 | `image_digests` | validated digests, never `latest`: official runner image, private DinD image, local job/toolchain images; Swift: macOS image identity | provisioning records + `docker inspect` digests |
| 5 | `selected_tests` | exact command/profile/features + fixture digest; Swift: `mise run swift-package-native-ci` chain incl. `desktop-xcframework` step (or corrected equivalent) | planner + job log |
| 6 | `junit_counts` | tests/passed/failed/skipped/errors from JUnit XML, per cell | JUnit artifact preserved OUTSIDE disposable containers (spec §8) |
| 7 | `cache_report` | namespace + hit/miss + integrity-verified reuse; macOS-ARM keys separate from Linux keys via `runner.os`/`runner.arch` | job log + cache records |
| 8 | `timing_report` | queue + provisioning + setup/compile/test/cache/cleanup phases | runner/job metadata + telemetry |
| 9 | `cleanup_receipt` | owned-resources-removed + permit released exactly once | occupancy/capacity ledger |
| 10 | `no_quotas_ref` | pointer to §6 inspection row covering this cell's job ancestry | §6 inspection log |
| 11 | `result` | green/red + full result-identity tuple (§7) | hosted verifier aggregate |

Engine/placement notes (spec §8): hostname printed by a job is NOT placement evidence. Placement =
runner/job metadata × provisioning IDs × engine versions. Apple Silicon proof for Swift = runner
arch metadata + successful `desktop-xcframework` (which bails on non-macOS) — the old exit-1/127
signatures from run `35114867283` must be absent.

## 4. Tool/image re-resolution ledger

Historic declarations (spec §9.1): Rust 1.97.1, Bun 1.3.14, Node 24.18.0, Ubuntu 26.04, macOS 26.
Record per lane: declared → resolved-actual version/digest + compatibility basis. Hard rules:

- A different Linux userspace is never silently forced (any userspace change is explicit + justified).
- Apple-only mise tools are never installed on Linux (assert by tool-install logs per lane).
- Resolved digests are locked in the record; tags are never assumed immutable.

## 5. docker-e2e explicit execution (§9.1 E2E clause)

One top-level job (spec §4.1: the nested 20-capsule test is ONE permit). Requirements:

1. `e2e` feature/profile explicitly enabled (nextest `docker-e2e` profile: `dind_e2e`,
   `session_send_e2e`, `usage_broker_e2e`, `load_options_e2e` included; default excludes them).
2. Capsule Linux ELF built from the TESTED source via `cargo run --bin build-jackin-capsule --
   --export` (zigbuild path); `JACKIN_CAPSULE_BIN` set in job env; ELF magic + exec bit hold
   (`require_capsule_binary_override` passes). NO preview-release substitute.
3. Proven on the lane: Docker, Buildx, Compose where used, `script(1)` PTY support, nested
   privileged DinD, Java Testcontainers, TLS/no-proxy behavior, temporary relay socket/file mounts.
   Each gets a job-log evidence pointer (assert present, not assumed).
4. Serial E2E groups preserved (`docker-e2e` serial group, `max-threads = 1` in
   `.config/nextest.toml`); the 20-capsule fixture fanout exercised
   (`usage_broker_desktop_and_twenty_docker_capsules_make_one_provider_call` with literal 20 —
   `/tmp/f1-e2e-probe.md` §a). NO fixture weakening to make a lane green (counts/thresholds at
   live-main values unless an F1-justified source change moved them).
5. E2E record fields: profile + feature flags, capsule build log + ELF digest, `JACKIN_CAPSULE_BIN`
   value, per-suite JUnit counts, serial-group config hash, fanout count observed (20), cleanup
   receipt (capsule containers force-removed, relay dirs gone), permit released once.

## 6. No-quotas inspection on real Jackin job containers/cgroups (spec §4.3)

Quota-free execution is proven on REAL Jackin qualification jobs — not on fixtures, not on flags
alone. Inspect, per lane (native + official + DinD) and representative nested descendants
(incl. E2E capsule descendants):

- Docker HostConfig: no `NanoCpus`/CPU quota/cpuset, no memory/reservation/swap ceiling.
- Effective cgroup ancestry: `cpu.max`, `memory.max`, `memory.high`, swap/cpuset = `max`/unset.
  Inherited limits inspected, not only emitted flags.
- Package units/drop-ins: no quota drop-ins recreated by upgrade; preflight assumes none.
- Effective build env: no slot-divided `CARGO_BUILD_JOBS`, MBX/BuildKit entitlement, Gradle worker
  count, or artificial heap ceiling injected as disguised partition.
- Correctness-required serialization kept: Jackin Docker E2E group stays serial (test semantics,
  not a CPU partition — spec §4.3).

One inspection row per inspected container/cgroup, linked from §3 field 10. Any ceiling found =
F2 fails + quota source named (config, package hook, BuildKit/service handling, or test).

## 7. Correlation method (run/attempt/job IDs → cells)

Result identity per spec §2 (fixed BEFORE execution):

```text
repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest
```

(`platform` distinguishes `linux` cells from the 2 `macos` executions.)

1. Pre-execution: enumerate the full expected result set from trusted planner output — 114 Linux
   tuples + 2 macOS tuples + E2E tuple(s) + control-job tuples. Frozen before first job start.
2. Per observed job, join: GitHub side (`run_id` + `run_attempt` + `job_id`, ALL API pages,
   attempts distinguished) × display-name provider (`… / <provider> / …`) × management side
   (provisioning IDs × engine versions × image digests) × outcome side (JUnit × cache/timing ×
   cleanup, §3 fields 5–10).
3. Mismatch classes that FAIL the gate: missing, skipped, cancelled, timed-out, failed,
   duplicate-conflicting, identity-mismatched, stale-attempt, wrong-provider report
   (spec §2 + §8: required aggregate fails even with remaining providers green).
4. Fail-fast MUST be disabled on all qualification matrices (literal `false`; expansion test +
   run-timeline behavior proof, per E1 §5). A run where one red cell cancelled siblings is VOID.
5. Per-provider independence (E1 §6): every cell owns its engine identity, JUnit, timing, cleanup.
   Compiler-cache reuse with namespace isolation is legitimate; copying any report/verdict across
   providers voids both cells. Unit aggregate green ONLY if ALL non-excepted provider cells green
   on own evidence.
6. Reruns revalidate exact identity + outcome provenance; a later reporter/cancellation NEVER
   overwrites an already-failed required result with success (spec §8).
7. Record per cell: `run_url`, `run_id`, `run_attempt`, `job_id`, `job_url`, expected tuple,
   observed tuple, match verdict.

## 8. Representative PR + main procedure (work-plan F2 action 2)

1. **Representative PR run**: open (or designate) a PR against `jackin-project/jackin` whose diff
   exercises the generated CI surface representatively (touches ≥1 Rust unit path + the planner
   input so all lanes expand; F2 author justifies "representative" in the gate report — which
   units affected, why the selection covers all three providers + Swift + E2E).
2. **Main-run evidence**: a full qualification run on `main` at the recorded source SHA (post-F1
   tree). PR and main runs BOTH complete the full §1 capture shape; neither is a reduced subset
   (`coverage: reduced` can never count — E1 §4 rule carries over).
3. Record for EACH run: run URL, source SHA, `plan_digest`, planner identity, attempt number(s),
   per-phase timings, all job IDs/URLs mapped per §7.
4. Pre-declared exceptions (if any) use the E1 §4 format, committed BEFORE run creation, citing a
   typed capability (platform), controller rule (trust), or planner affected-analysis. Anything
   unlisted that fails to go green FAILS F2.
5. Waves persist per spec §8 if hosted lifetimes/matrix limits force them; units are never omitted
   to fit one run.

## 9. N-remeasure trigger (work-plan F2 action 3, spec §4.4)

**Trigger: IFF the Jackin workload mix changes N materially.** Decision procedure:

1. Baseline: the mixed-engine N requalified at E2 (cite value + evidence pointer).
2. Compare Jackin qualification telemetry against the E2 mix: jobs/minute, end-to-end
   time-to-green, queue/provisioning delay, phase timings, CPU, memory pressure/OOM, disk/IO,
   Docker/BuildKit/cache-lock contention, failures.
3. Material change = any of: sustained throughput regression at same N, new OOM/pressure regime,
   new contention class (e.g. nested-DinD/E2E footprint shifts the stable point), or stability
   loss at the E2 N. F2 author states the comparison numbers; verifier judges materiality.
4. If triggered: remeasure mixed-engine N per spec §4.4 — representative full+mixed workload,
   values such as 16/24/32/48/64 and higher only while stable useful throughput improves;
   comparable source/plan/image/cold-warm conditions; same-host before/after only (never
   cross-hardware comparison); highest stable useful throughput wins; only host-level N changes,
   containers stay unbounded. Record full benchmark table + chosen N + basis.
5. If NOT triggered: record the no-remeasure justification (comparison numbers + "E2 N stands")
   — one paragraph, signed by verifier. Skipping silently is not allowed.

## 10. Velnor non-regression proof for generic F1 changes (work-plan F2 action 4, §0.8)

For EVERY generic change made during F1 (generator fix, e.g. Swift routing at `ir.rs:3936`;
Velnor package fix; any shared primitive):

1. Name the change + owning workstream: it returns to its AFFECTED author/verifier pair
   (work-plan §0.8), not the F2 pair.
2. Ordering: tooling published before consumers repin; host code packaged before upgrade;
   Velnor requalified for that change BEFORE Jackin advances on it.
3. Proof: the affected pair re-runs the Velnor proofs covering the change (generator: regen +
   ownership + expansion + affected unit tests incl. new regression tests; package: upgrade path +
   affected qualification evidence). Cite prior proof IDs + rerun outputs; no unverified
   mixed-version state.
4. F2 gate report lists each generic change with its §0.8 proof pointer (§11 row 8). A generic
   change without a requalification pointer blocks F2 sign-off. No full campaign restart for
   unrelated changes.

## 11. Gate report skeleton

```text
F2 GATE REPORT — Jackin full parity
====================================
1. Identity: repo, source SHA(s) (PR + main), plan_digest(s), planner identity,
   generator pin, velnor-runner version, run URLs + attempts.
2. Inventory: 40-unit ID table (kind/root), kind counts 36/1/1/2, coverage
   ledger vs evidence.md §4 (deltas justified or "none").
3. Linux matrix: 114/114 cells green, each with §3 11-field record pointer;
   fail-fast-disabled proof; per-provider independence attestation.
4. Swift: 2/2 genuine macOS executions (§3 records), Apple Silicon proof,
   same-job XCFramework→build→test chain intact, old exit-1/127 signatures absent.
5. E2E: docker-e2e record (§5): capsule-from-source digest, JACKIN_CAPSULE_BIN,
   per-suite counts, serial groups, 20-capsule fanout observed, no weakening.
6. No-quotas: §6 inspection rows linked; ceilings found: none (or named + F2 failed).
7. Tools: §4 re-resolution ledger (declared → actual + basis).
8. Generic changes: each F1 generic fix + §0.8 requalification proof pointer (§10).
9. N: E2 baseline N, comparison numbers, remeasure table (if triggered) or
   signed no-remeasure justification (§9).
10. PR + main: URLs, identities, attempts, timings (§8); exception list (§8.4).
11. Verdict: PASS/FAIL + independent Jackin parity verifier signature
    (verifier ≠ F1/F2 author).
```

## 12. F2 evidence bundle checklist

- [ ] §2 unit inventory (40 IDs + kinds) + coverage ledger vs evidence.md §4.
- [ ] 114 Linux per-cell records (§3) + pre-execution expected-tuple list + planner digest.
- [ ] 2 Swift per-execution records (§3) + Apple Silicon proof.
- [ ] Per-cell correlation rows: run/attempt/job IDs → expected tuple (§7).
- [ ] Pre-declared exception list (possibly empty), timestamped before run creation.
- [ ] Fail-fast-disabled proof (YAML inspection + expansion test + timeline behavior).
- [ ] Per-provider independence attestation (distinct provisioning/JUnit/cleanup per cell).
- [ ] docker-e2e record (§5) + no-fixture-weakening attestation.
- [ ] No-quotas inspection log (§6) linked from every cell.
- [ ] Tool/image re-resolution ledger (§4) with locked digests.
- [ ] PR + main runs: URLs, identities, attempts, timings (§8).
- [ ] N decision: remeasure table or signed no-remeasure justification (§9).
- [ ] Velnor non-regression pointers for all generic F1 changes (§10).
- [ ] Signed gate report (§11) by the independent Jackin parity verifier.
