# Velnor macOS CLI diagnostics

`velnorctl` builds natively on macOS arm64. Its local diagnostics inspect the
Docker/OrbStack VM without claiming that the VM can host the Linux runner.

## Commands

```text
velnorctl host bootstrap-image
velnorctl host start [--repo tailrocks/velnor] [--slots 1] [--pr 812]
velnorctl host status
velnorctl host drain
velnorctl host stop
velnorctl preflight [--config-dir DIR] [--work-dir DIR]
velnorctl status [--config-dir DIR]
velnorctl docker report [--check-bind-mount] [--image IMAGE]
velnorctl storage paths [--config-dir DIR]
```

Focused development branch: `velnor-macos-host` (from PR #814).

`host bootstrap-image` compiles linux `velnor-workflow` in `rust:1.98.1-bookworm`
and builds `docker/job-ubuntu.Dockerfile` on the current daemon. It does not
pull GHCR or copy Sentry caches. Export `GITHUB_TOKEN` for the Dockerfile
`github_token` secret; never pass the token as a flag.

`host start` is the on-demand entry point. It registers repository-scoped
runners only and refuses organization URLs so recovery cannot join
`velnor-trusted`. Export `GITHUB_TOKEN`; never pass a token as a flag.
When `VELNOR_STORAGE_ROOT` is unset, `host start` uses a user-owned prefix
instead of `/var`: `~/.velnor-store` on macOS (a short dot-directory, because
`~/Library/Application Support/velnor/run/velnor/<name>/control.sock` overflows
the 104-byte Unix socket path limit) and `~/.local/state/velnor` on Linux.
See `content/docs/guides/macos-host.mdx` for the full host guide.
Slot and job children exec `velnor-runner` beside `velnorctl` (or hidden
`velnorctl slot` / `velnorctl job` if that binary is missing).
Linux container jobs on macOS stay Linux jobs. `--pr` prints scheduling
semantics; GitHub remains the scheduler and already-queued
`group: velnor-trusted` jobs cannot be claimed by this host.

`velnorctl docker` and `velnorctl docker status` are equivalent to `docker
report`. Add `--output json` for a stable diagnostic object. A failed
diagnostic still writes its report and exits non-zero.

The report includes the selected Docker context and endpoint source, Unix
socket resolution, API/server architecture, Buildx, cgroup mode, Docker
resources, and separate `velnorCompatible`/`runnerReady` results.
`--check-bind-mount` runs a bounded disposable container probe against the
selected image. It never pulls an image, so pre-pull the image before
diagnosing an offline daemon.

Docker target precedence is `VELNOR_DOCKER_HOST`, then
`VELNOR_DOCKER_CONTEXT`, then Docker's `DOCKER_CONTEXT`, then `DOCKER_HOST`,
then the Docker CLI's current context/default socket. The chosen source is
printed in the report; every Docker probe uses that same target.

## OrbStack and Docker limits

OrbStack can prove the local CLI endpoint and often the host-to-VM bind mount.
When its server reports `cgroupfs-v2`, the report marks the Linux
`systemdCgroupV2` invariant as `unsupported`, while keeping endpoint and
resource compatibility separate. `velnorCompatible` can therefore be true
for Docker diagnostics while `runnerReady` is false. `preflight` and
`status` retain the non-zero runner-readiness result and print the Linux
remediation; no cgroup failure is hidden or relabeled as a macOS hard failure.

For a bind mount where the daemon sees a different path, pass both paths:

```text
velnorctl docker report --check-bind-mount \
  --work-dir /Users/me/velnor-work \
  --docker-host-work-dir /host/velnor-work \
  --image alpine:3.20
```

`velnorctl storage paths` prints cache, library, run, log, config, work,
runner-log, and daemon-shared artifact roots. A slot work directory is lifted
to its daemon-shared parent before resolving `_velnor_artifacts`, matching the
runner's artifact ownership path.

## Packaged daemon instances (Linux `.deb` hosts)

On a host installed from the `.deb`, every daemon is a
`velnor-daemon@<instance>.service` unit whose configuration is the unit's
environment: `/etc/velnor/<instance>.env` (`VELNOR_NAME`, `VELNOR_URL`,
`VELNOR_SLOTS`, `VELNOR_WORK_DIR`, `VELNOR_TRUST_SCOPE`, ...) layered under
`velnor-daemon@.service` and any `velnor-daemon@<instance>.service.d/*.conf`
drop-in. Storage root, work dir, trust scope, daemon directory, and the
control socket all derive from that environment inside `velnor-runner`; there
is no second place that decides them.

`velnorctl get`, `status`, `host status`, `host drain`, `host stop`, and
`cache` resolve the daemon they address through the same code
(`velnor_runner::daemon_instance`), so they never need `VELNOR_STORAGE_ROOT`
or `VELNOR_TRUST_SCOPE` exported by hand:

```text
velnorctl --instance dogfood get jobs --since 1h   # systemd instance name…
velnorctl --instance velnor-dogfood status        # …or its VELNOR_NAME
velnorctl cache du                                 # every packaged instance
velnorctl --instance dogfood cache gc --yes        # one instance's stores
```

Selection rules:

* Packaged instances exist and `--instance` (or `VELNOR_INSTANCE`) names one:
  that instance. A name that matches no instance is a usage error listing the
  known instances; it is never treated as a development daemon, because on a
  packaged host that would address a socket no daemon listens on.
* Packaged instances exist and nothing is requested: the only instance when
  there is exactly one; otherwise a usage error listing the choices.
  `cache du` and `cache gc` are the exception and run one pass per instance,
  each with that instance's storage root and trust scope.
* No packaged instances: the development daemon under this process's own
  socket root (`velnorctl daemon --name`, or `default`).

The control socket of a packaged instance is
`<VELNOR_STORAGE_ROOT>/run/velnor/<VELNOR_NAME>/control.sock` —
`/run/velnor/<name>/control.sock` for the unit's default `/var` storage root —
and its journal and health document live in the unit's `StateDirectory`,
`/var/lib/velnor-<instance>/runner/daemons/<VELNOR_NAME>/`. `--config-dir` and
`--state-dir` on `status` still override the resolved daemon directory.

`velnor-cache-gc.timer` runs `velnorctl cache gc --yes` without `--instance`
and therefore reclaims every packaged instance's stores in one pass.
