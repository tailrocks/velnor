# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

## Completion gate (goal NOT finished until ALL proven)

Operator contract (2026-09-15): finish only after this sequence is independently proven.

| # | Gate | Status |
|---|---|---|
| 1 | macOS-hosted Velnor executes real `tailrocks/velnor` GHA jobs (not GitHub-hosted substitution) | **PASS** — run `34894567066` job `104145226224` on `velnor-macos-recovery-slot-1` @ `363d727b`; Docker precreate + Policy job **Succeeded** |
| 2 | Merge **#809, #812, #814, #815** using that macOS Velnor as the Velnor-lane runner; required checks green; no protection/DCO/signing bypass | **IN PROGRESS** — #816; this Mac ran rust-velnor-control + rust-production-topology on `velnor-macos-recovery-slot-*` (run `34901667088`). Both slots then fenced (`SlotStale` immediately after `RemoteAcked`) while waiters kept the broker session, so successor acquires were rejected (`slot must still be Ready`). Controller fix: do not fence an acting slot; terminate waiters on fence so generation can recover. |
| 3 | `main` fully green after the merge stack | **NOT YET** |
| 4 | New release: Debian apt (`tailrocks/velnor-apt`) **and** Homebrew; artifacts install and operate | **NOT YET** — today Linux debs only; no Homebrew channel |
| 5 | Deploy that verified release to Sentry (not a rescue build) | **NOT YET** — do not deploy until gate 4 |
| 6 | Sentry executes real repository jobs; all PRs green, `main` green, release process proven | **NOT YET** |

## Live snapshot (2026-09-15)

| Item | Value |
|---|---|
| **main** | `ad5fc59d` |
| **PR #809** | `fb9a31a6` `codex/merge-all-velnor-20260914` — OPEN, BLOCKED |
| **PR #812** | `991d86bd` `codex/velnor-macos-action-portability-20260915` — OPEN, BLOCKED, DCO fail |
| **PR #814** | macOS `velnorctl` + host start — `codex/velnorctl-macos-support-20260915` — OPEN, BLOCKED |
| **PR #815** | #812 + #814 combo `recovery/pr812` — OPEN, BLOCKED, DCO fail |
| **Focused branch** | **`velnor-macos-host`** — continue macOS Docker-backed Velnor here |
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
| #809 Velnor jobs | cancelled/queued | `group: velnor-trusted` empty (all org runners offline) |
| #812 GitHub rust-velnor-workflow @ 21822888 | fail | scan input `a2f70c15` → `861def8a` because `21822888` added `scripts/test-setup-velnor-workflow-action.sh` without recording it |
| #812 `991d86bd` generator | **verified** | Independent clean-tree `--check` and `--force` at `991d86bd`: byte-stable, scan `861def8a` is the real `RepositoryShape` digest. Not a hash overwrite. |
| #812/#814/#815 Velnor jobs | queued | same org-group dependency |
| #812/#815 DCO | fail | `991d86bd` still lacks `Signed-off-by` (operator force-with-lease to replace) |
| Sentry tailrocks | fleet down | JIT wedge, 0/8 registered after 15:11 restart |
| Repo-scoped dogfood | this branch claimable | `velnor-macos-host` emits label-only `runs-on`; #809/#812/#814/#815 still require `velnor-trusted` |

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

1. Restart `velnor-macos-recovery` via launchd onto the acting-slot fence fix. Prove a **second** acquire on `velnor-macos-recovery-slot-*` after a completed job.
2. Prove Docker / Velnor on this Mac (not dogfood). Host already runs `VELNOR_TRUST_SCOPE=trusted`.
3. Rebase/regen #809/#812/#814/#815 onto labels-only. Operator DCO-sign `991d86bd`.
4. Sentry deploy only from the final merged/released SHA.
