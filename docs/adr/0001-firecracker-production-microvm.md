# ADR 0001: Firecracker is the production microVM

Status: accepted (2026-08-26)

## Decision

Velnor has exactly two operator-selectable execution backends:

```toml
[execution]
backend = "docker" # or "microvm"
```

The `microvm` backend uses the official Firecracker VMM and matching jailer
directly on Linux KVM (HTTP API over the jailer’s Unix socket). One jailed
Firecracker process, one guest agent, and one guest-local Docker daemon own
each GitHub job. There is no fallback onto host Docker.

## Why Firecracker (primary sources, not popularity)

Official spec: <https://firecracker-microvm.github.io/> and
<https://github.com/firecracker-microvm/firecracker> (v1.16.1 pin).

| Axis | Firecracker | Cloud Hypervisor | QEMU/KVM | crosvm | Kata/Dragonball | StratoVirt |
| --- | --- | --- | --- | --- | --- | --- |
| Device model | 5 devices, no virtio-fs | Broader (hotplug, PCI, virtio-fs) | Legacy + everything | ChromeOS-oriented | Orchestration stack | Huawei stack |
| Control | HTTP Unix socket + jailer | HTTP Unix socket | QMP | custom | CRI/Kata | custom |
| Snapshot | Create/load, version-bound | Yes, not cross-version | Yes | limited | via VMM | yes |
| Isolation | jailer cgroup/netns/seccomp/drop | seccomp | wide TCB | minijail | Kata + VMM | custom |
| Suitability for one VM per GitHub job | Yes: ephemeral, jailed, block+vsock | Fallback only | Too large a TCB | Not product-owned | Not product orchestration | Not estate-owned |

Unused features (PCI, virtio-fs, GPU, live migration) are not capability
advantages for this job. Firecracker is rejected only if a named estate
workflow produces a reproducible capability failure and Cloud Hypervisor is
proven to provide that missing capability under the same suite. No such
blocker has been reproduced.

Spec boot/overhead numbers (<125 ms, <5 MiB) are cited, not measured here.
Same-host VMM benches require Sentry KVM plus pinned artifacts.

## Isolation

- Storage: virtio-block (immutable kernel/rootfs, job-local CoW writable disk).
  Not virtio-fs, bind mounts, NFS, or 9p.
- Net: virtio-net on a unique TAP + netns.
- Control: virtio-vsock, never SSH or a management TCP port.
- Host `/var/run/docker.sock` is not in the guest.

## Packaging

`firecracker` 1.16.1, matching `jailer`, guest kernel, rootfs, guest agent, and
optional snapshot are checksummed in `microvm/manifest.json` and bound to the
Debian/apt release identity. Mixed generations fail closed before advertising.

## Live measurement

This agent host is macOS (no `/dev/kvm`). Sentry previously had `/dev/kvm` and
no `firecracker`/`jailer` binaries. Live boot and Firecracker-vs-Cloud
Hypervisor benches wait on signed-apt artifact publication.
