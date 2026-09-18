# A1 — Preview run 35159519217 guest-payload failure

- Run: `35159519217` @ `33688938` (main), conclusion `failure`
- Failing jobs: `Guest payload x86_64`, `Guest payload aarch64` — step `Upload guest payload`
- Error (both arches, identical):
  `EACCES: permission denied, scandir '.../dist/microvm/work/rootfs-tree/lib/ssl/private'`
- No edits made. Source-fix proposal only.

## Evidence

- `gh run view 35159519217 --log-failed`: both arches fail only in `Upload guest payload`
  (`actions/upload-artifact`, `path: dist/microvm/*`).
- Full log (`/tmp/preview-full-35159519217.log`): seed NOT restored (`restored=false`,
  `Download pinned kernel tarball` + `Build guest vmlinux and rootfs.ext4` ran), build
  printed `built dist/microvm/vmlinux and dist/microvm/rootfs.ext4`, then upload failed.
- Neighboring runs: EACCES also in `35156192575`, `35153334247` (cold seed cache → fresh
  build). Runs failing earlier/differently (`35158059629`, `35157891090`, `35156585412`,
  `35153770838`, `35149906649`) show no EACCES. Failure happens iff a fresh guest build runs.
- Noble container probe (`ubuntu:24.04` + `ca-certificates openssl ssl-cert`):
  - `/usr/lib/ssl/private -> /etc/ssl/private` — **absolute symlink**, shipped by the
    `openssl` package (`dpkg -S` confirms), pulled in via `ca-certificates` in
    `ROOTFS_PACKAGES` (`crates/velnor-runner/src/execution/guest.rs:38`, since `f738a4c1`).
  - `/etc/ssl/private` is `0710 root:ssl-cert` (root-only).
  - `/lib` is a symlink to `usr/lib`, so the failing path traverses
    `rootfs-tree/lib → rootfs-tree/usr/lib`, then escapes the tree via the absolute symlink.

## Root cause

Symlink escape from build scratch into the artifact upload, landing on a root-only host path:

1. `velnor-guest-image build --out dist/microvm` nests scratch **inside** the publish dir:
   `let work = out.join("work")` (`crates/velnor-runner/src/bin/velnor-guest-image.rs:137`),
   unpacking a full Debian tree at `dist/microvm/work/rootfs-tree` plus the `linux-6.1.102`
   kernel source. Nothing ever deletes it.
2. The generator emits a wildcard upload `path: dist/microvm/*`
   (`crates/velnor-workflow/src/primitives/release.rs:725`,
   `render_guest_payload_job`), so upload-artifact recursively walks `work/`.
3. `rootfs-tree/lib/ssl/private` resolves: `lib → usr/lib` (in-tree), then
   `usr/lib/ssl/private → /etc/ssl/private` (**absolute** → escapes to the CI **host**).
4. Host `/etc/ssl/private` is `0710 root:ssl-cert`; the `runner` user is neither → Node
   `scandir` → EACCES.
5. Why only this path: sibling absolute symlinks (`certs`, `cert.pem`, `openssl.cnf`)
   point at world-readable host paths; in-tree `rootfs-tree/etc/ssl/private` was chowned
   to runner and reads fine. Exactly one root-only host target exists in the walk.
6. Why the existing `sudo chown -R uid:gid` (`guest_image.rs:467-480`) can't help: it
   re-owns in-tree inodes only; the failure is through a symlink onto a path outside the tree.

Enabling architecture: (a) build scratch nested under the artifact publish dir with no
cleanup; (b) wildcard artifact glob instead of the explicit consumer file contract;
(c) one shared renderer → **both** `preview.yml` and `release.yml` ship the same broken
`dist/microvm/*` upload (`release.yml:3600`). Bonus damage: cold builds also upload ~GBs
of kernel source + rootfs tree inside `work/`.

## Proposed source fix

Primary — generator, explicit payload contract (`release.rs`, `render_guest_payload_job`
format string at ~724-726). Replace:

```yaml
          path: dist/microvm/*
```

with the exact 5-file consumer contract (use the existing `agent_bin` variable — the
renderer is generic over package, do not hardcode `velnor-guest-agent`):

```yaml
          path: |
            dist/microvm/vmlinux
            dist/microvm/rootfs.ext4
            dist/microvm/rootfs.sha256
            dist/microvm/{agent_bin}
            dist/microvm/guest-agent.sha256
```

Consumers assert exactly these files and nothing else: preview `Stage guest payload`
(`release.rs:768` block) and stable `debian_guest_steps` (`release.rs:926` block).
Then regen `preview.yml` + `release.yml` via the generator (never hand-edit) and extend
the generator test covering the guest-payload upload block to assert the explicit list
and reject `dist/microvm/*`.

Hardening — builder (`velnor-guest-image.rs:137`): stop nesting `work/` under `--out`
(`--work-dir` defaulting to a tempdir or an `<out>-work` sibling; or remove `work/` on
successful build). Defense in depth; also fixes local-run payload-dir pollution.

Explicitly reject: chmod/chown changes in `build_rootfs` (cannot fix a symlink escape
onto a host-owned path); sudo-wrapping upload; `!dist/microvm/work` negative glob
(leaves the scratch-in-publish-dir trap for the next wildcard).

## Adjacent findings (not this failure, flag for follow-up)

- Payload asymmetry: seed-reuse path produces `dist/microvm/vmlinux.sha256`, fresh-build
  path never creates it; no consumer reads it. The 5-file list above matches the consumer
  contract; optionally emit `vmlinux.sha256` in the fresh-build step for parity.
- `rootfs.sha256` format differs by path (bare hash vs two-column `sha256sum` output);
  consumers `awk '{print $1}'` so both parse, but parity would be cleaner.

## Suggested verification for the implementer

- Regen diff on `preview.yml`/`release.yml` shows only the upload-block change.
- Generator unit tests pass, incl. the new upload-block assertion.
- Cold-cache Preview run goes green on both arches.
- Local Linux repro of the mechanism: `find dist/microvm/work -lname '/*'` shows the
  absolute symlinks; non-root `scandir` through `rootfs-tree/lib/ssl/private` reproduces
  EACCES while in-tree `etc/ssl/private` reads fine.
