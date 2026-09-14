# Velnor macOS CLI diagnostics

`velnorctl` builds natively on macOS arm64. Its local diagnostics inspect the
Docker/OrbStack VM without claiming that the VM can host the Linux runner.

## Commands

```text
velnorctl host start [--repo tailrocks/velnor] [--slots 1] [--pr 812]
velnorctl host status
velnorctl host drain
velnorctl host stop
velnorctl preflight [--config-dir DIR] [--work-dir DIR]
velnorctl status [--config-dir DIR]
velnorctl docker report [--check-bind-mount] [--image IMAGE]
velnorctl storage paths [--config-dir DIR]
```

`host start` is the on-demand entry point. It registers repository-scoped
runners only and refuses organization URLs so recovery cannot join
`velnor-trusted`. Export `GITHUB_TOKEN`; never pass a token as a flag.
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
