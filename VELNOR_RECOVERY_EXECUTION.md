# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

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
| #812 GitHub rust-velnor-workflow @ 21822888 | fail | scan input `a2f70c15` → `861def8a`; recorded in 991d86bd |
| #812/#814/#815 Velnor jobs | queued | same org-group dependency |
| #812/#815 DCO | fail | `991d86bd` (and some local socket commits) lack Signed-off-by |
| Sentry tailrocks | fleet down | JIT wedge, 0/8 registered after 15:11 restart |
| Repo-scoped dogfood | cannot claim PR CI | jobs require `velnor-trusted` group |

## Decisions

- Recovery hosts stay repository-scoped and refuse org URLs.
- Linux containers on macOS are Linux jobs, not native macOS jobs.
- `velnorctl host start` is the operator entry point on #814.
- Do not force-push unsigned history; add signed follow-up commits. #812 DCO still needs a signed replacement of `991d86bd` (requires operator force-with-lease).

## macOS host progress (`velnor-macos-host`)

| Step | Status | Evidence |
|---|---|---|
| `velnorctl docker report` | **PASS** | OrbStack linux/arm64, `velnorCompatible=true` |
| Dev socket root | **PASS** | `~/Library/Application Support/velnor/…` |
| `state.db` resolution | **PASS** | uses `config_dir/state.db`, not `/var/lib/velnor` |
| macOS cgroup probe skip | **PASS** | `execution/docker.rs` skips systemd slice on macOS |
| `host start` preflight | **PASS** | writes `execution.toml`, checks job image |
| `host start` daemon | **BLOCKED** | `velnor/job-ubuntu:26.04` missing locally |

Bootstrap job image:
```bash
docker build --file docker/job-ubuntu.Dockerfile --tag velnor/job-ubuntu:26.04 .
export GITHUB_TOKEN=$(gh auth token)
velnorctl host start --repo tailrocks/velnor --work-dir ~/.velnor-recovery/work
```

## Next

1. Build/pull `velnor/job-ubuntu:26.04`; complete `host start` daemon registration.
2. Prove one real Docker-backed repository job from macOS.
3. Sync `velnor-macos-host` → #814; keep #815 as integration branch.
4. Keep #809→#812 merge order once `ci-required` can run.
5. Sentry deploy only from the final merged/released SHA.
