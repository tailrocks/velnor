# C2 quota-free inspection prep — exact command checklist (bastion campaign, read-only)

Sources: `plans/bastion-three-provider-ci/spec.md` §4.3, `plans/bastion-three-provider-ci/work-plan.md` STEP C2,
`/tmp/c2-prereq-map.md` (Q1–Q31 / N1–N8 / L1–L9).
Scope: design only. No bastion writes: every command runs over SSH with output redirected to
operator-local files. Nothing is created, started, stopped, or reloaded on bastion
(no `tee`/`--bootstrap`/`docker exec` as primary evidence, no `systemctl daemon-reload`).

```sh
BASTION=root@37.27.110.241
OUTDIR=./c2-evidence-$(date -u +%Y%m%dT%H%M%SZ); mkdir -p "$OUTDIR"
```

## P0 — Preconditions (fail-closed)

```sh
date -u | tee "$OUTDIR/00-meta.txt"
ssh "$BASTION" 'uname -a; dpkg-query -W velnor-runner; docker version --format "{{.Server.Version}} {{.Server.CgroupDriver}} {{.Server.CgroupVersion}}"; nproc; free -m | head -2' | tee "$OUTDIR/00-host.txt"
ssh "$BASTION" 'docker ps --format "{{.ID}} {{.Names}} {{.Label \"velnor.job-id\"}}"' | tee "$OUTDIR/00-containers.txt"
```

- **PASS:** ≥1 live `velnor-job-*` container from a real native job (record smoke-job URL + job IDs).
- **FAIL:** zero job containers → checks H/G/E/S are **VOID** (an empty set never passes).

Container universe for all checks below (top-level jobs + nested descendants + BuildKit):

```sh
ssh "$BASTION" '{ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u' | tee "$OUTDIR/00-universe.txt"
```

## H — Docker HostConfig inspection (no NanoCpus/quotas/cpuset/memory ceilings)

```sh
ssh "$BASTION" 'for c in $({ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u); do docker inspect --format "CID={{.Id}} Name={{.Name}} NanoCpus={{.HostConfig.NanoCpus}} CpuQuota={{.HostConfig.CpuQuota}} CpuPeriod={{.HostConfig.CpuPeriod}} CpuShares={{.HostConfig.CpuShares}} CpusetCpus={{.HostConfig.CpusetCpus}} CpusetMems={{.HostConfig.CpusetMems}} Memory={{.HostConfig.Memory}} MemoryReservation={{.HostConfig.MemoryReservation}} MemorySwap={{.HostConfig.MemorySwap}} PidsLimit={{.HostConfig.PidsLimit}} OomKillDisable={{.HostConfig.OomKillDisable}} CgroupParent={{.HostConfig.CgroupParent}}" "$c"; done' | tee "$OUTDIR/10-hostconfig.txt"
ssh "$BASTION" 'for c in $({ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u); do echo "== $c"; docker inspect --format "{{json .HostConfig}}" "$c"; done' | tee "$OUTDIR/11-hostconfig-json.txt"
```

Per-container **PASS** (every container in universe, incl. nested descendants):

| Field | Required |
|---|---|
| `NanoCpus` | `0` |
| `CpuQuota` | `0` |
| `CpusetCpus` / `CpusetMems` | empty |
| `Memory` / `MemoryReservation` | `0` |
| `MemorySwap` | `0` or `-1` (both = unset) |
| `PidsLimit` | `0` (spec: no per-job PID quotas) |
| `OomKillDisable` | `false` (spec: OOM handling never disabled) |
| `CgroupParent` | `velnor-jobs.slice` (identity, not ceiling — record) |

Informational only: `CpuShares`, `CpuPeriod` (shares/period ≠ ceiling while quota is 0).
**FAIL:** any single ceiling value on any container, or any nested descendant missing from the universe.

## G — Effective cgroup ancestry walk (inherited limits, not only emitted flags)

```sh
ssh "$BASTION" 'set -u; for c in $({ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u); do pid=$(docker inspect --format "{{.State.Pid}}" "$c"); rel=$(sed "s/^0::\\///" /proc/$pid/cgroup); echo "== container=$c pid=$pid cgroup=/$rel"; for f in cpu.max memory.max memory.high memory.swap.max cpuset.cpus cpuset.mems; do echo "-- $f:"; echo "  /: $(cat /sys/fs/cgroup/$f 2>/dev/null || echo MISSING)"; acc=/sys/fs/cgroup; rest=$rel; while [ -n "$rest" ]; do part=${rest%%/*}; case $rest in */*) rest=${rest#*/};; *) rest=;; esac; acc=$acc/$part; echo "  $acc: $(cat $acc/$f 2>/dev/null || echo MISSING)"; done; done; done' | tee "$OUTDIR/20-cgroup-ancestry.txt"
ssh "$BASTION" 'cat /sys/fs/cgroup/cpuset.cpus.effective; nproc; grep -c ^processor /proc/cpuinfo' | tee "$OUTDIR/21-cpu-baseline.txt"
```

Per-level **PASS** (leaf scope up through `velnor-jobs.slice` to `/`):

| File | Required at EVERY ancestry level |
|---|---|
| `cpu.max` | `max 100000` |
| `memory.max` / `memory.high` | `max` |
| `memory.swap.max` | `max`, or `MISSING` only if also `MISSING` at `/` (kernel w/o swap controller — record) |
| `cpuset.cpus` / `cpuset.mems` | empty (unset = unrestricted) |

**FAIL:** any finite quota/ceiling or any non-empty cpuset subset at any level, incl. levels
above the container scope (inherited limits count). `cpuset.cpus.effective` must equal the full host set.

## U — Package units / drop-ins (no CPUQuota/MemoryMax/MemoryHigh, no quota drop-ins)

```sh
ssh "$BASTION" 'systemctl cat velnor-jobs.slice velnor-job@.service velnor-daemon.service velnor-daemon@.service velnor-slot@.service velnor-controller@.service velnor-guardian.service velnor-control.slice' | tee "$OUTDIR/30-systemctl-cat.txt"
ssh "$BASTION" 'systemctl show velnor-jobs.slice velnor-job@.service -p CPUQuotaPerSecUSec,CPUQuotaPeriodUSec,MemoryMax,MemoryHigh,MemorySwapMax,MemoryMin,AllowedCPUs,AllowedMemoryNodes,TasksMax,ControlGroup,DropInPaths,FragmentPath' | tee "$OUTDIR/31-systemctl-show.txt"
ssh "$BASTION" 'ls -la /etc/systemd/system/velnor-jobs.slice.d/ /etc/systemd/system/velnor-job@.service.d/ /run/systemd/system/velnor-jobs.slice.d/ /run/systemd/system/velnor-job@.service.d/ 2>&1; echo "rc=$?"' | tee "$OUTDIR/32-dropin-absent.txt"
ssh "$BASTION" 'grep -rniE "CPUQuota|MemoryMax|MemoryHigh|MemorySwap|AllowedCPUs|AllowedMemoryNodes" /etc/systemd/system/ /run/systemd/system/ 2>/dev/null; echo "grep_rc=$?"' | tee "$OUTDIR/33-quota-grep.txt"
```

- **PASS:** workload ancestry (`velnor-jobs.slice`, `velnor-job@.service`) shows
  `CPUQuotaPerSecUSec=infinity`, `MemoryMax/MemoryHigh/MemorySwapMax=infinity|[not set]`,
  `TasksMax=infinity`, empty `AllowedCPUs`, empty `DropInPaths`; `systemctl cat` shows no
  `# /etc/...` or `# /run/...` drop-in fragments for them; `ls` of the four `.d/` dirs fails
  (absent); repo-wide quota grep finds no match (`grep_rc=1`).
- **Allowed (record, not fail):** `velnor-control.slice` `MemoryMin`/`CPUWeight`/`TasksMax`
  (control-plane protection/shares, prereq map Q19) — must NOT appear on the jobs ancestry.
- **FAIL:** any ceiling/drop-in on the jobs ancestry, or the retired
  `velnor-jobs.slice.d/10-host-cpu.conf` recreated (upgrade-regression check, prereq map Q17).

## E — Effective build env (no injected CARGO_BUILD_JOBS/MBX/BuildKit/Gradle/heap partitions)

```sh
ssh "$BASTION" 'for c in $({ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u); do echo "== $c"; docker inspect --format "{{range .Config.Env}}{{println .}}{{end}}" "$c"; done' | tee "$OUTDIR/40-env.txt"
grep -Ei '^(CARGO_BUILD_JOBS|MBX_SCHEDULER_CPUS|MBX_SCHEDULER_MEMORY|MBX_BUILDKIT|BUILDKIT_CPU|BUILDKIT_MEMORY|GRADLE_OPTS|ORG_GRADLE|_JAVA_OPTIONS|JDK_JAVA_OPTIONS|MAVEN_OPTS|MAKEFLAGS)=' "$OUTDIR/40-env.txt"; echo "partition_grep_rc=$? (1 = clean)" | tee "$OUTDIR/41-env-verdict.txt"
printf "CARGO_BUILD_JOBS=4\n" | grep -Ei '^(CARGO_BUILD_JOBS|MBX_SCHEDULER_CPUS|MBX_SCHEDULER_MEMORY|MBX_BUILDKIT|BUILDKIT_CPU|BUILDKIT_MEMORY|GRADLE_OPTS|ORG_GRADLE|_JAVA_OPTIONS|JDK_JAVA_OPTIONS|MAVEN_OPTS|MAKEFLAGS)=' >/dev/null && echo "pattern-control: FIRES ok" | tee -a "$OUTDIR/41-env-verdict.txt"
ssh "$BASTION" 'docker buildx ls; for b in $(docker buildx ls --format "{{.Name}}" 2>/dev/null); do echo "== builder $b"; docker buildx inspect "$b" 2>/dev/null | grep -iE "Driver Opts|CPU|Memory|quota" || true; done' | tee "$OUTDIR/42-buildkit.txt"
```

- **PASS:** partition grep finds nothing (`partition_grep_rc=1`); pattern-control line present
  (proves the grep can fire); `docker buildx inspect` driver opts show no
  `cpu-period`/`cpu-quota`/`memory=` sizing (read-only: never `--bootstrap`).
- **FAIL:** any partition var present. `MAKEFLAGS`/`NODE_OPTIONS`/`MBX_CACHE_DIR|TARGET_ROOT|GC_*`
  note: GC/cache vars are disk hygiene, not partitions (prereq map Q28 — record, not fail);
  a workflow-declared `MAKEFLAGS`/`NODE_OPTIONS` passes only if the verifier traces the exact
  value to the workflow source; otherwise FAIL.
- Optional confirmatory (spawns a process in-container; record-only, not primary evidence):
  `ssh "$BASTION" 'docker exec <cid> env'`.

## S — No-raw-socket-mount proof (mount lists + lease-identity)

```sh
ssh "$BASTION" 'docker context inspect --format "{{.Endpoints.docker.Host}}" 2>/dev/null; echo "DOCKER_HOST=$DOCKER_HOST"; stat -c "host-sock dev=%d ino=%i %n" /var/run/docker.sock /run/docker.sock 2>/dev/null' | tee "$OUTDIR/50-host-socket.txt"
ssh "$BASTION" 'for c in $({ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u); do docker inspect --format "== {{.Name}} ({{.Id}}) mounts: {{json .Mounts}}" "$c"; done' | tee "$OUTDIR/51-mounts.txt"
ssh "$BASTION" 'for c in $({ docker ps -q --filter "label=velnor.job-id"; docker ps -q --filter "name=buildx_buildkit_velnor-builder-"; } | sort -u); do echo "== $c"; docker inspect --format "{{range .Config.Env}}{{println .}}{{end}}" "$c" | grep -E "^(DOCKER_HOST|VELNOR_DOCKER_HOST)" || echo "(no DOCKER_HOST env)"; done' | tee "$OUTDIR/52-dockerhost-env.txt"
```

- **PASS:** no mount with `Source` ∈ {`/var/run/docker.sock`, `/run/docker.sock`, any
  `$DOCKER_HOST`-socket path}; every guest `/var/run/docker.sock` destination is backed by a
  lease socket under `/run/velnor/` whose `stat` dev:ino differs from the host socket
  (distinct daemon identity, not a bind of the management socket); no `DOCKER_HOST=tcp://`
  pointing at a host Docker proxy.
- **FAIL:** any host-socket (or unrestricted host Docker proxy) source in any job/nested
  mount list, or a guest docker socket whose source inode equals the host management socket.

## Bundle manifest

```sh
(cd "$OUTDIR" && sha256sum * > MANIFEST.sha256 && cat MANIFEST.sha256)
```

Verdict rule: C2 quota-free = P0 non-void AND H AND G AND U AND E AND S all PASS on the same
live job set. Re-run whole bundle after every later package upgrade (work-plan C2 action 6 / D3).
