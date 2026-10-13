# Estate capability matrix (default-branch workflows)

Generated from the canonical 28-repo fleet map in `VELNOR_PROJECTS_SETUP.md`
and the four byte-identical class templates. Class identity means one code
repo's `ci.yml` is the code class; one tap, one apt, and the fixture are the
other classes. Repository-specific product surfaces are listed as extras.

## Class templates

| Class | Repos | Workflow surface | Docker | Buildx/Bake | Services | Testcontainers | Cache/artifacts | Multi-arch OCI | Attest/publish |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| code (20) | jackin-project/* (6), tailrocks code (11), ChainArgos (3) | generated `ci.yml` + owner reusables; `lanes: velnor \| github \| both` | job container + adapters | yes where the class builds images | possible via reusable jobs | java-monorepo extra | actions/cache + Results Service | Buildx+QEMU user-mode, not native ARM | attest where declared |
| tap (5) | homebrew-* | tap class template | no host devices | no | no | no | no | no | tap publish |
| apt (2) | velnor-apt, holla-apt | apt class template | package publish | no | no | no | no | no | signed apt |
| fixture (1) | velnor-actions-fixture | canonical Actions patterns | host-socket Docker, job+service containers, Buildx, Bake, `docker run` | yes | yes | yes | yes | yes (emulation) | fixture only |

## Explicit special jobs

| Need | Where | microVM implication |
| --- | --- | --- |
| Native ARM execution | Parallax Apple packaging (GitHub-hosted, not a Velnor lane); no amd64 host may advertise native ARM | Firecracker does not emulate a foreign CPU. amd64 may Buildx+QEMU-user-emulate arm64 OCI images only. |
| Nested KVM | none in class templates | reject if a workflow later needs it |
| Host device / FUSE / eBPF / host net | none required by class templates | fail closed if admitted later without a new approval |
| Privileged containers | opt-in `VELNOR_ALLOW_PRIVILEGED_OPTIONS` on trusted docker backend only | microVM guest may be privileged inside the guest; not on the host |
| Direct host paths | docker backend bind-mounts workspace; microVM uses block disks + vsock import | no virtio-fs |

## Backend mapping

- `docker`: current host-Docker semantics (job container, services, per-job network, lease-proxied host socket for trusted scope).
- `microvm`: same GitHub-visible plan inside one Firecracker guest with guest-local Docker. Host socket is never attached.

Fixture parity must keep `lanes: velnor | github | both`. Estate `microvm` pass is not claimed until Sentry signed-apt live proof exists.
