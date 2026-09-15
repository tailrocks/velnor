# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

## Completion gate (goal NOT finished until ALL proven)

Operator contract (2026-09-15): finish only after this sequence is independently proven.

| # | Gate | Status |
|---|---|---|
| 1 | macOS-hosted Velnor executes real `tailrocks/velnor` GHA jobs (not GitHub-hosted substitution) | **PASS — historical evidence only** — run `34894567066`, job `104145226224`, `velnor-macos-recovery-slot-1`, source `363d727b`; Docker-backed Policy job succeeded and repeated-slot evidence exists. Current-source revalidation: **NOT PROVEN**. |
| 2 | Merge **#809, #812, #814, #815** using that macOS Velnor as the Velnor-lane runner; required checks green; no protection/DCO/signing bypass | **NOT SATISFIED** — #809/#812/#814/#815/#816 are closed with `merged_at=null`; #819 (`24f05af9`) was merged but does not satisfy the individual-PR wording. |
| 3 | `main` fully green after the merge stack | **NOT PROVEN** — current `main` is `8708f3aa`; its CI and Preview runs were canceled, and the exact required checks are not green/proven. |
| 4 | New release: Debian apt (`tailrocks/velnor-apt`) **and** Homebrew; artifacts install and operate | **NOT PROVEN / BLOCKED** — public release is still `v0.1.274`; APT exists, but no Velnor Homebrew tap/formula was found. |
| 5 | Deploy that verified release to Sentry (not a rescue build) | **NOT PROVEN** — no final merged/released artifact has been deployed; preserve rollback evidence before any change. |
| 6 | Sentry executes real repository jobs; all PRs green, `main` green, release process proven | **NOT PROVEN** — Sentry is currently degraded and the final release/scenario evidence is absent. |

## Live snapshot (refreshed 2026-09-15T14:17Z)

Sources: `git fetch origin --prune`, GitHub REST/Actions read-only queries, and read-only Sentry SSH checks. Short SHAs below are unambiguous prefixes.

| Item | Current evidence |
|---|---|
| **`origin/main`** | `8708f3aa18edf3e20f9f5b7cba4793ea8b87326a` (`#851`, merged 2026-09-15T14:01:57Z). |
| **`origin/velnor-macos-host`** | **Absent** from `git ls-remote` and GitHub branch API (404). The requested `045931f0` is a reachable historical object, not a live remote ref. |
| **PR #816** | Closed 2026-09-15T10:14Z, head `c6865e86`, `merged_at=null`; GitHub's non-null `merge_commit_sha` field is not merge proof. |
| **Main ruleset** | Active `protect-main`; required contexts are `DCO` and `ci-required`; strict required-status policy is false. Legacy branch-protection endpoint returns 404. |
| **Main-head checks** | Commit `8708f3aa` has zero check-runs and combined status `pending`. Head-specific runs `34978901335` (CI) and `34978900973` (Preview) are `completed/cancelled`; no successful exact-head gate is proven. |
| **Actions backlog** | Six runs were `in_progress` at refresh (`34979936959`, `34979278544`, `34978865825`, `34978595459`, `34977754044`, and stale main run `34976776297`); 29 runs were queued. No run for current main head was active. |
| **macOS capacity** | Historical real-job evidence is retained above. Current runner/slot identity, repeated-job, reconnect, drain, and cleanup evidence at a final source SHA: **NOT PROVEN**. |
| **Sentry** | Debian 13 `x86_64`; `velnor-guardian` and `velnor-daemon@tailrocks` active. Installed `0.1.274~preview.158+7e59e1a`, source `7e59e1ae`, binary SHA `cf23935b…`; health **degraded**, `github_reachable=false`, `routing_valid=false`, `runner_group_valid=false`, 8/8 executor-ready, SQLite registrations `0`. Backend Docker. |
| **Release channels** | Latest public tag `v0.1.274` at `120f2236`; APT repository HEAD `a2c52656`. No Velnor Homebrew tap/formula found; cross-platform release/install proof is **NOT PROVEN**. |

### Requested PR state

| PR | State | Head | Base | Merge SHA |
|---|---|---|---|---|
| #809 | closed, unmerged | `466de1a6` | `ad5fc59d` | — |
| #812 | closed, unmerged | `4db23c0f` | `ad5fc59d` | — |
| #814 | closed, unmerged | `a67e4c28` | `ad5fc59d` | — |
| #815 | closed, unmerged | `e5049452` | `ad5fc59d` | — |
| #816 | closed, unmerged | `c6865e86` | `701fbdd1` | — |
| #837 | merged | `938d7623` | `88d8fb46` | `8fd242cc` |
| #841 | merged | `c3122efc` | `ab6e718c` | `46ad5ad4` |
| #842 | merged | `370f09bd` | `8fd242cc` | `92f933fd` |
| #844 | merged | `2f2907be` | `362506c9` | `fdc7bce3` |
| #845 | merged | `40dea12e` | `0ef08df2` | `57c40fd9` |
| #848 | merged | `b46b2d9d` | `0ef08df2` | `28865199` |
| #850 | merged | `514b3934` | `92f933fd` | `362506c9` |
| #851 | merged | `cbd48ccb` | `f4069d68` | `8708f3aa` |
| #854 | merged | `d8b30e04` | `fdc7bce3` | `3d5f734e` |
| #855 | merged | `cbc6cf35` | `3d5f734e` | `0ef08df2` |
| #856 | merged | `cf2f934b` | `16e788a5` | `f4069d68` |
| #857 | merged | `895f4853` | `57c40fd9` | `16e788a5` |

GitHub reports a `merge_commit_sha` field for closed #816, but `merged_at` is null; it is therefore recorded as unmerged. Historical #819 remains merged at `24f05af9`, but does not satisfy the individual-PR gate for #809/#812/#814/#815.

## Current blockers and next actions

1. **Recovery branch continuity.** `045931f0` contains the latest recorded branch work (guest-payload routing test `47b43bfd`, release pins `5047ef2c`, action allowlist `e4b742da`, man-page mode `c4d49af9`, and formatting `0ba8ef34`), but its remote ref and PR #816 are gone/closed. Restore the implementation stream through a normal, signed branch/PR operation; do not treat the closed PR or historical object as merged.
2. **#844 lifecycle risk.** #844 is merged as `fdc7bce3`, but independent audit found a structural ownership-key mismatch: creation labels containers with `velnor.job-id=<container name>` (`velnor-job-<job-id>`), while recovery passes the raw job ID, making exact-label teardown a no-op. Marker-backed stale jobs can bypass teardown; missing/malformed PID markers are admitted as dead; and completion acknowledgement can precede durable cleanup. Port a canonical ownership ID, fail-closed `Live/Dead/Ambiguous` admission, durable cleanup obligation, and production-shaped regression tests before accepting this gate. Verifier: Faraday, isolated read-only lifecycle audit; Mencius verification pending.
3. **Tests and generator.** Historical `045931f0` verification reported 376 `velnor-workflow` tests and repeated generator checks passing. A local integration test after adding `main@92f933fd` reported 374 passed plus two failures (release identity pin and stale Preview routing assertion); #856/#857 later landed, but current `main@8708f3aa` has no successful exact-head CI/generator evidence. Re-run the full configured test, format/lint, generator stability, actionlint, and Docker gates on the restored branch and current main; record the exact run/job IDs.
4. **Homebrew.** Release configuration currently targets Linux triples and the consumer path is `tailrocks/velnor-apt`; candidate Velnor taps returned 404 and no formula exists. Add/land the real supported macOS formula/tap and publish/install/operate proof, or leave Gate 4 blocked. Do not document a nonexistent channel. Verifier: Pascal, isolated release audit.
5. **Sentry and final scenarios.** Do not deploy the current preview or a dirty/source-bootstrap build. Record active/previous release identities, prepare rollback, deploy only the verified merged release, then prove Sentry jobs, simultaneous Mac capacity, clean drain, Sentry-only continuation, and reconnect. All post-release fields remain **[PENDING EVIDENCE]**.

### Current evidence placeholders

| Required proof | State |
|---|---|
| Final `velnor-macos-host` branch SHA and pushed signed history | **[PENDING — remote ref currently absent]** |
| Exact current-source multi-job/reconnect/drain/cleanup run/job/runner IDs | **[PENDING]** |
| Green `DCO` + `ci-required` at merged revisions and on `main` | **[PENDING]** |
| New APT/Homebrew artifact versions, checksums, installs, and runtime proof | **[PENDING]** |
| Sentry final deployment version, rollback execution, and post-deploy jobs | **[PENDING]** |

## Historical snapshot (superseded; pre-2026-09-15T14:17Z refresh)

| Item | Value |
|---|---|
| **main** | `24f05af9` (#819 MERGED). CI red @ `8c1c8831` — Advisory policy fails (stale `d3e441fb` revision vs new workflows). |
| **PR #809** | **CLOSED** — superseded by #819. Last known `466de1a6`. |
| **PR #812** | **CLOSED** — superseded by #819. Last known `4db23c0f`. |
| **PR #814** | **CLOSED** — superseded by #819. Last known `a67e4c28`. |
| **PR #815** | **CLOSED** — superseded by #819. Last known `e5049452`. |
| **PR #816** | CLOSED unmerged. Not the integration PR. Branch DCO rewrite closed it; superseded by #819. |
| **PR #819** | **MERGED** `24f05af9`. Integration landed on `main`. |
| **Focused branch** | `velnor-macos-host` — fix Advisory policy (stale `d3e441fb` revision), PR to `main`. |
| **Host** | Relaunched 07:35Z PID 89135 on fixed binary (per-slot mbx), 12/12 accountable, healthy execution; latest: slot-6 zombie cleaned, **10/12 ready**. |
| **Lifecycle** | Verified local tests cover exclusive in-flight leases, persisted waiter ownership, teardown-before-release, fail-closed marker scans, orphan cleanup, output spill/reconnect, and repeated slot reuse. Plus per-slot `MBX_CACHE_DIR` isolation, admission-test env guard, per-PR concurrency (see Verified fixes). |
| **Sentry** | JIT wedge unchanged; tailrocks fleet down. Do not deploy Sentry. |

## Historical PR stack (superseded; pre-2026-09-15T14:17Z refresh)

```
main@d3e441fb  (#809/#812 base)
main@ad5fc59d  (#814/#815 base; +#811 +#813)

#809 466de1a6  last known; --cpus 2
 ├─ #812 21822888 portable setup-runtime
 │    └─ 991d86bd scan-input record (DCO unsigned)
 │         └─ #815 cdbf0399 macOS docker diagnostics
 │              └─ 781f0e32 recovery docs
 └─ #814 ce2e783e macOS docker diagnostics (sibling of #812, not stacked)
      └─ local: socket-root + host start
```

Unique work:
- **#809:** shared integration (ci-required, runner, generator rev 32)
- **#812:** setup-runtime portability + generator scan hash
- **#814:** native macOS velnorctl Docker diagnostics + on-demand host (source of `velnor-macos-host`)
- **#815:** #812 unique + #814 diagnostics + recovery docs
- **`velnor-macos-host`:** `origin/` remote ref reported gone after #819 merge. Local still `velnor-macos-host` with dirty WIP — do not discard. Other agents keep #812/#814/#815.

Merge order: **#809 → #812 → refresh #814/#815**. Do not close any as redundant.

## Historical failure graph (retained evidence; pre-2026-09-15T14:17Z refresh)

| Surface | Result | Cause |
|---|---|---|
| #809 run `34930642603` attempt 1 | 28 SUCCESS later reused on attempt 2; 3 fails replaced | this-Mac workflow + workflow-contract SUCCESS. 3 container-death fails replaced by attempt 2. |
| #809 run `34930642603` attempt 2 | hung `in_progress` | 7 this-Mac Velnor jobs idle mbx/no rustc ~36min (not compiling). Superseded by rerun. |
| #809 run `34930642603` rerun | in progress | 8/17 Velnor on recovery slots; production-topology Velnor fail ~58s (setup, not compile). Prior attempt force-cancelled after mbx wedge. |
| #809 run `34930642603` prior attempt | force-cancelled (`completed/cancelled`) | 12 Mac containers wedged in mbx ABBA flock deadlock (see wedge row). |
| mbx cross-container flock wedge | ROOT CAUSE PROVEN + FIXED+VERIFIED | All containers shared `/var/cache/mbx`; mbx takes registrar flock(EX)→lease flock(EX), no timeouts; lease names `{pid}-0.lease` collide (every container runs mbx as pid 63). Observed 1 holder + 11 waiters, 0% CPU, 30+ min (runs `34930408749` att.3, `34930642603` att.2). Fix `ec96f089`: per-slot `MBX_CACHE_DIR` (`/var/cache/mbx/slots/slot-N`), verified vs upstream `jdx/mr-boxington` (relocates registrar+leases). Host locks dir rotated; wedge evidence preserved at `.locks.wedged-20260915T0608Z`. Post-rewrite SHA `14dda640` (same fix). Force-cancelled wedged attempts: run `34930408749` att.3 and run `34930642603` att.2/prior (completed/cancelled). |
| Sentry hung test `local_composite_unknown_nested_action_fails_admission_read_only` | TEST-ONLY, FIXED+VERIFIED | `admit_job` errored pre-connect (unset `VELNOR_GITHUB_HTTP_TRANSPORT`); fake server parked in `accept()`, test in `join()` forever (job `104265844196`, 2176/2177 then hang). Fix `bf77c4fd`: env guard + 30s accept deadline + read timeout + test-support gate. Verifier: passes env-unset/set, CI `--all-features` keeps it running. Post-rewrite SHA `2d6bc863` (same fix). |
| #812 `4db23c0f` / #814 `a67e4c28` / #815 `e5049452` | DCO pass; `ci-required` fail | latest completed runs |
| #816 run `34939230446` @ `e53bc60b` | historical (PR CLOSED unmerged) | Planning **PASS** then; not current integration |
| #816 run `34930408749` | historical (PR CLOSED unmerged) | bootstrap/setup pattern (superseded) |
| #819 MERGED `24f05af9`; leftover run `34960823479` | topology still compiling on this-Mac slot-3 at last watch | integration landed on `main`; do not cancel leftover. |
| Host lifecycle | root cause documented | `child_owns_slot` ignored persisted waiter PIDs after controller restart. OAuth `release_in_flight_after_registration_gone` on branch. Marker release when `runner.json` gone still missing. Orphan-leak fix pointer: `8ea96b4c` (recover dead persisted jobs on reconnect, local head). |
| `velnor-macos-host` | `origin/` remote ref reported gone after merge | local still `velnor-macos-host` with dirty WIP — do not discard. |
| Sentry tailrocks | fleet down | JIT wedge, 0/8 registered after 15:11 restart |
| cpus-budget test `builds_start_container_args_with_mounts` | FIXED+VERIFIED | Expected `--cpus` hardcoded; fix `3ec86c23` (post-rewrite `756c3576`): derive expected value from slot budget. |
| `velnor-workflow` fmt check | FIXED+VERIFIED | rustfmt drift; fix `30c63ea1` (post-rewrite `ed52f625`): apply rustfmt. |
| repo-wide Velnor concurrency group | ROOT CAUSE PROVEN + FIXED | Single group cancelled overlapping runs with 0 jobs started, making green structurally unobtainable. Fix `a5b2ff5a`: scope concurrency group per PR. |
| branch DCO rewrite | DONE | Unsigned `991d86bd` forced operator-owned rewrite (pre/post SHA pairs in Verified fixes); #816 closed unmerged, superseded by #819 (merged `24f05af9`). |
| host relaunch 07:35Z | HEALTHY | PID 89135 on fixed binary (per-slot mbx); 12/12 slots accountable, healthy execution. |

## Historical verified fixes (each independently verified; retained post-rewrite evidence)

| Fix | Pre-rewrite | Post-rewrite | Verifier |
|---|---|---|---|
| admission-test hang (env-unset transport + accept/join deadlock) | `bf77c4fd` | `2d6bc863` | passes env-unset/set; CI `--all-features` keeps it running |
| per-slot mbx (`MBX_CACHE_DIR` → `/var/cache/mbx/slots/slot-N`) | `ec96f089` | `14dda640` | matches upstream `jdx/mr-boxington` relocation; host healthy post-relaunch |
| cpus-budget test expectation | `3ec86c23` | `756c3576` | test derives `--cpus` from slot budget |
| `velnor-workflow` fmt | `30c63ea1` | `ed52f625` | rustfmt clean |
| per-PR concurrency groups | — (post-rewrite only) | `a5b2ff5a` | no more 0-job cross-PR cancels |

## Historical recon results (2026-09-15; superseded by live snapshot above)

- **#809 gate:** only blocker is cpus (budget/expectation mismatch); all other gates passable.
- **Stack red heads:** #812/#814/#815 latest completed runs DCO pass but `ci-required` fail (see Failure graph); #816 closed unmerged.
- **Release map + Homebrew gap:** today Linux debs only; no Homebrew channel (gate 4).
- **Sentry:** apt-only recipe; `.145` live; rest of fleet down (JIT wedge). Do not deploy until gate 4.
- **Orphan-leak:** `child_owns_slot` ignored persisted waiter PIDs after controller restart → fix `8ea96b4c`.
- **Lone-cancel:** isolated cancels; OAuth `release_in_flight_after_registration_gone` on branch; marker release when `runner.json` gone still missing.

## Decisions

- Recovery hosts stay repository-scoped and refuse org URLs.
- Linux containers on macOS are Linux jobs, not native macOS jobs.
- `velnorctl host start` is the operator entry point on #814.
- Do not force-push unsigned history except the operator-owned DCO rewrite of `991d86bd`. Independent verify: that commit is a legitimate scan-input record, not a hash overwrite. Keep the bytes; sign them.
- Merging via this Mac requires Velnor-lane jobs that a **repository-scoped** runner can claim. `velnor-macos-host` now omits `velnor_runner_group` and emits `runs-on: [self-hosted, velnor-target-mvp]`. #809/#812/#814/#815 still emit `group: velnor-trusted` until they rebase/regen. Recovery hosts must not join that org pool.
- Velnor concurrency groups are scoped per PR (`a5b2ff5a`). A repo-wide group cancelled overlapping runs before any job started (0-job cancels), making green structurally unobtainable.

## Historical macOS host progress (`velnor-macos-host`)

| Step | Status | Evidence |
| --- | --- | --- |
| `velnorctl docker report` | **PASS** | OrbStack linux/arm64, `velnorCompatible=true` |
| Dev socket root | **PASS** | `~/Library/Application Support/velnor/…` |
| `state.db` resolution | **PASS** | uses `config_dir/state.db`, not `/var/lib/velnor` |
| macOS cgroup probe skip | **PASS** | `execution/docker.rs` skips systemd slice on macOS |
| `host start` preflight | **PASS** | writes `execution.toml`, checks job image |
| `host bootstrap-image` | **PASS** | `velnor/job-ubuntu:26.04` built locally (~95s) |
| macOS socket bind fix | **PASS** | Darwin `lchown`/`chmod` on path; `SocketIdentity::from_path` |
| `host start` daemon | **PASS** | `test-mac2` control/admin sockets live |
| Real GitHub job executed | **PASS** | run `34894567066` Policy on `velnor-macos-recovery-slot-1`; Docker job completed Succeeded after `363d727b` apt-workflow skip |

Bootstrap job image:

```bash
export GITHUB_TOKEN  # never pass as a flag
velnorctl host bootstrap-image
velnorctl host start --repo tailrocks/velnor --work-dir ~/.velnor-recovery/work
```

Linux workflow binary (source-bootstrap, not GHCR):

```text
release-binaries/arm64/velnor-workflow
ELF aarch64, dynamically linked, 4.4M
```

Job image is local: `velnor/job-ubuntu:26.04` linux/arm64. GHCR pull is
unavailable (`read:packages` 403; `:latest` not found).

Proven on this Mac (`velnor-macos-host`; refresh live, do not freeze SHA):

```text
velnorctl host bootstrap-image   # job image present
velnorctl host start --repo tailrocks/velnor
  Docker preflight passed
  slot … published first heartbeat
  health: github_reachable + executor_ready; registered_slots=0
```

Host processes were SIGTERM'd after ~3 minutes in this session before JIT
registration appeared on GitHub. Next start must stay up and show a
repo-scoped runner that is not in `velnor-trusted`.

## Historical next actions (superseded; pre-2026-09-15T14:17Z refresh)

1. Fix Advisory policy on `velnor-macos-host` (stale `d3e441fb` revision vs new workflows); PR to `main`.
2. Gate 3: `main` green after policy fix.
3. Gates 4–6: release (Debian apt + Homebrew), then Sentry deploy — do not deploy Sentry until gate 4.
4. #819 green proof (post-merge verification of the integration).
5. #809 operator rebase, then stack merges (#812/#814/#815).
