# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

## Completion gate (goal NOT finished until ALL proven)

Operator contract (2026-09-15): finish only after this sequence is independently proven.

| # | Gate | Status |
|---|---|---|
| 1 | macOS-hosted Velnor executes real `tailrocks/velnor` GHA jobs (not GitHub-hosted substitution) | **PASS** — run `34894567066` job `104145226224` on `velnor-macos-recovery-slot-1` @ `363d727b`; Docker precreate + Policy job **Succeeded** |
| 2 | Merge **#809, #812, #814, #815** using that macOS Velnor as the Velnor-lane runner; required checks green; no protection/DCO/signing bypass | **NOT YET** — #809 `466de1a6` run `34930642603` rerun: 7 Velnor jobs now on `velnor-macos-recovery-slot-*` (progress). Need `ci-required` green + recovery Velnor-lane proof. Do not merge. |
| 3 | `main` fully green after the merge stack | **NOT YET** |
| 4 | New release: Debian apt (`tailrocks/velnor-apt`) **and** Homebrew; artifacts install and operate | **NOT YET** — today Linux debs only; no Homebrew channel |
| 5 | Deploy that verified release to Sentry (not a rescue build) | **NOT YET** — do not deploy until gate 4 |
| 6 | Sentry executes real repository jobs; all PRs green, `main` green, release process proven | **NOT YET** |

## Live snapshot (2026-09-15 ~05:35 UTC)

| Item | Value |
|---|---|
| **main** | `701fbdd1` (#817). Not `ad5fc59d`. |
| **PR #809** | `466de1a6` — OPEN — run `34930642603` rerun. **28 SUCCESS reused**; 7 Velnor jobs now on `velnor-macos-recovery-slot-*` (gate 2 progress). `ci-required` not posted. Do not merge. |
| **PR #812** | `4db23c0f` — OPEN — DCO pass; `ci-required` fail (latest completed run) |
| **PR #814** | `a67e4c28` — OPEN — DCO pass; `ci-required` fail (latest completed run) |
| **PR #815** | `e5049452` — OPEN — DCO pass; `ci-required` fail (latest completed run) |
| **PR #816** | `94b02ab4` — OPEN — DCO pass; run `34930408749` completed failure (bootstrap/setup pattern) |
| **Focused branch** | `origin/velnor-macos-host` @ `2e5d299b` — `6a88a77d` (workflow CI telemetry/cache/selection), `2e5d299b` (slot acting PID fix) |
| **Host** | relaunched ~05:25 UTC pid `16577`; **12/12 ready**. Running host is old rescue binary. |
| **Lifecycle** | Root cause documented: `child_owns_slot` ignored persisted waiter PIDs after controller restart. OAuth `release_in_flight_after_registration_gone` on branch. Marker release when `runner.json` gone still missing. |
| **Sentry** | `velnor-runner 0.1.274~preview.145+d3e441f` @ `d3e441fb`; tailrocks fleet down |

## Related PR stack

```
main@d3e441fb  (#809/#812 base)
main@ad5fc59d  (#814/#815 base; +#811 +#813)

#809 fb9a31a6  integration
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
- **`velnor-macos-host`:** the branch for running Velnor from macOS. Other agents keep #812/#814/#815.

Merge order: **#809 → #812 → refresh #814/#815**. Do not close any as redundant.

## Failure graph

| Surface | Result | Cause |
|---|---|---|
| #809 run `34930642603` attempt 1 | 28 SUCCESS later reused on attempt 2; 3 fails replaced | this-Mac workflow + workflow-contract SUCCESS. 3 container-death fails replaced by attempt 2. |
| #809 run `34930642603` attempt 2 | hung `in_progress` | 7 this-Mac Velnor jobs idle mbx/no rustc ~36min (not compiling). Superseded by rerun. |
| #809 run `34930642603` rerun | in progress | 7 Velnor jobs now on `velnor-macos-recovery-slot-*` (gate 2 progress). `ci-required` not posted. Do not merge. |
| #812 `4db23c0f` / #814 `a67e4c28` / #815 `e5049452` | DCO pass; `ci-required` fail | latest completed runs |
| #816 `94b02ab4` run `34930408749` | completed failure | bootstrap/setup pattern |
| Host lifecycle | root cause documented | `child_owns_slot` ignored persisted waiter PIDs after controller restart. OAuth `release_in_flight_after_registration_gone` on branch. Marker release when `runner.json` gone still missing. |
| `velnor-macos-host` | `2e5d299b` | `6a88a77d` workflow CI telemetry/cache/selection; `2e5d299b` slot acting PID fix |
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

Proven on this Mac (`velnor-macos-host` @ `c569e6c7`):

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

1. Watch #809 run `34930642603` rerun — 7 Velnor jobs on recovery slots. Preserve the 28 greens.
2. Gate 2 **NOT YET** until `ci-required` green + recovery Velnor-lane proof. Do not merge #809.
3. Land `2e5d299b` slot acting PID fix on recovery host; rebuild/relaunch if needed.
4. Do not deploy Sentry.
