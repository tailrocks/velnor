# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

## Completion gate (goal NOT finished until ALL proven)

Operator contract (2026-09-15): finish only after this sequence is independently proven.

| # | Gate | Status |
|---|---|---|
| 1 | macOS-hosted Velnor executes real `tailrocks/velnor` GHA jobs (not GitHub-hosted substitution) | **PASS** — run `34894567066` job `104145226224` on `velnor-macos-recovery-slot-1` @ `363d727b`; Docker precreate + Policy job **Succeeded**. Repeated jobs PASS (slot lifecycle evidence). |
| 2 | Merge **#809, #812, #814, #815** using that macOS Velnor as the Velnor-lane runner; required checks green; no protection/DCO/signing bypass | **PARTIAL** — **#819 MERGED** (`24f05af9`); integration landed on `main`. #809/#812/#814/#815 **CLOSED** (superseded by #819), not individually merged per original gate wording. #816 closed unmerged. Do not merge by bypass. |
| 3 | `main` fully green after the merge stack | **NOT YET** — `main` CI red @ `8c1c8831`; Advisory policy fails (stale `d3e441fb` revision vs new workflows). |
| 4 | New release: Debian apt (`tailrocks/velnor-apt`) **and** Homebrew; artifacts install and operate | **NOT YET** — today Linux debs only; no Homebrew channel |
| 5 | Deploy that verified release to Sentry (not a rescue build) | **NOT YET** — Do not deploy Sentry. JIT wedge unchanged; blocked until gate 4. |
| 6 | Sentry executes real repository jobs; all PRs green, `main` green, release process proven | **NOT YET** |

## Live snapshot (2026-09-15; refresh live)

| Item | Value |
|---|---|
| **main** | `24f05af9` (#819 MERGED). CI red @ `8c1c8831` — Advisory policy fails (stale `d3e441fb` revision vs new workflows). |
| **PR #809** | **CLOSED** — superseded by #819. Last known `466de1a6`. |
| **PR #812** | **CLOSED** — superseded by #819. Last known `4db23c0f`. |
| **PR #814** | **CLOSED** — superseded by #819. Last known `a67e4c28`. |
| **PR #815** | **CLOSED** — superseded by #819. Last known `e5049452`. |
| **PR #816** | CLOSED unmerged. Not the integration PR. |
| **PR #819** | **MERGED** `24f05af9`. Integration landed on `main`. |
| **Focused branch** | `velnor-macos-host` — fix Advisory policy (stale `d3e441fb` revision), PR to `main`. |
| **Host** | slot-6 zombie cleaned; **10/12 ready**. |
| **Lifecycle** | Verified local tests cover exclusive in-flight leases, persisted waiter ownership, teardown-before-release, fail-closed marker scans, orphan cleanup, output spill/reconnect, and repeated slot reuse. |
| **Sentry** | JIT wedge unchanged; tailrocks fleet down. Do not deploy Sentry. |

## Related PR stack

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

## Failure graph

| Surface | Result | Cause |
|---|---|---|
| #809 run `34930642603` attempt 1 | 28 SUCCESS later reused on attempt 2; 3 fails replaced | this-Mac workflow + workflow-contract SUCCESS. 3 container-death fails replaced by attempt 2. |
| #809 run `34930642603` attempt 2 | hung `in_progress` | 7 this-Mac Velnor jobs idle mbx/no rustc ~36min (not compiling). Superseded by rerun. |
| #809 run `34930642603` rerun | in progress | 8/17 Velnor on recovery slots; production-topology Velnor fail ~58s (setup, not compile). Prior attempt force-cancelled after mbx wedge. |
| #809 run `34930642603` prior attempt | force-cancelled (`completed/cancelled`) | 12 Mac containers wedged in mbx ABBA flock deadlock (see wedge row). |
| mbx cross-container flock wedge | ROOT CAUSE PROVEN + FIXED+VERIFIED | All containers shared `/var/cache/mbx`; mbx takes registrar flock(EX)→lease flock(EX), no timeouts; lease names `{pid}-0.lease` collide (every container runs mbx as pid 63). Observed 1 holder + 11 waiters, 0% CPU, 30+ min (runs `34930408749` att.3, `34930642603` att.2). Fix `ec96f089`: per-slot `MBX_CACHE_DIR` (`/var/cache/mbx/slots/slot-N`), verified vs upstream `jdx/mr-boxington` (relocates registrar+leases). Host locks dir rotated; wedge evidence preserved at `.locks.wedged-20260915T0608Z`. |
| Sentry hung test `local_composite_unknown_nested_action_fails_admission_read_only` | TEST-ONLY, FIXED+VERIFIED | `admit_job` errored pre-connect (unset `VELNOR_GITHUB_HTTP_TRANSPORT`); fake server parked in `accept()`, test in `join()` forever (job `104265844196`, 2176/2177 then hang). Fix `bf77c4fd`: env guard + 30s accept deadline + read timeout + test-support gate. Verifier: passes env-unset/set, CI `--all-features` keeps it running. |
| #812 `4db23c0f` / #814 `a67e4c28` / #815 `e5049452` | DCO pass; `ci-required` fail | latest completed runs |
| #816 run `34939230446` @ `e53bc60b` | historical (PR CLOSED unmerged) | Planning **PASS** then; not current integration |
| #816 run `34930408749` | historical (PR CLOSED unmerged) | bootstrap/setup pattern (superseded) |
| #819 MERGED `24f05af9`; leftover run `34960823479` | topology still compiling on this-Mac slot-3 at last watch | integration landed on `main`; do not cancel leftover. |
| Host lifecycle | root cause documented | `child_owns_slot` ignored persisted waiter PIDs after controller restart. OAuth `release_in_flight_after_registration_gone` on branch. Marker release when `runner.json` gone still missing. |
| `velnor-macos-host` | `origin/` remote ref reported gone after merge | local still `velnor-macos-host` with dirty WIP — do not discard. |
| Sentry tailrocks | fleet down | JIT wedge, 0/8 registered after 15:11 restart |

## Decisions

- Recovery hosts stay repository-scoped and refuse org URLs.
- Linux containers on macOS are Linux jobs, not native macOS jobs.
- `velnorctl host start` is the operator entry point on #814.
- Do not force-push unsigned history except the operator-owned DCO rewrite of `991d86bd`. Independent verify: that commit is a legitimate scan-input record, not a hash overwrite. Keep the bytes; sign them.
- Merging via this Mac requires Velnor-lane jobs that a **repository-scoped** runner can claim. `velnor-macos-host` now omits `velnor_runner_group` and emits `runs-on: [self-hosted, velnor-target-mvp]`. #809/#812/#814/#815 still emit `group: velnor-trusted` until they rebase/regen. Recovery hosts must not join that org pool.

## macOS host progress (`velnor-macos-host`)

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

## Next

1. Fix Advisory policy on `velnor-macos-host` (stale `d3e441fb` revision vs new workflows); PR to `main`.
2. Gate 3: `main` green after policy fix.
3. Gates 4–6: release (Debian apt + Homebrew), then Sentry deploy — do not deploy Sentry until gate 4.
