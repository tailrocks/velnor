# Velnor runner — Debian package + apt-native repository

**Status: implemented; publication decoupled for coherence (plan 010).**
`git tag vX.Y.Z && git push origin vX.Y.Z` runs the single
`.github/workflows/release.yml` coordinator. It builds both debs and the
multi-platform GHCR job image once,
then assembles ONE acyclic `release-record.json` binding source SHA -> crate
version -> per-arch binary/deb digests -> OCI image digest -> compiled-manifest
hash -> APT coordinate, and attaches the record + checksum + tar.gz + deb assets
to the GitHub release. Assets are never clobbered (an existing tag succeeds only
on an exact digest match). The source holds **no** APT credential and does **not**
push to or dispatch `tailrocks/velnor-apt` (N6): the apt repo pulls the published
record itself and verifies schema/source/tag/version/all-hashes/OCI before
`reprepro` (see velnor-apt `verify-release.sh`). The packaged daemon ships
never-exit supervision, sd_notify watchdog, `velnor-daemon@<name>` template
instances, `/etc/velnor/secrets.env` (0600, operator-owned), `velnor-doctor`
timers, and — new in plan 010 — a transactional `preinst`/`postinst` that never
builds/never restarts and a `release verify-installed` ExecStartPre coherence
gate on both daemon units. The design below documents the pieces.

Goal: install and upgrade the Velnor runner daemon with native apt:

```bash
sudo apt update
sudo install -d -m 0750 /run/velnor
sudo /usr/bin/flock --exclusive --nonblock --no-fork \
  /run/velnor/package-transaction.lock \
  apt-get install velnor-runner=X.Y.Z
```

The `flock --no-fork` process is replaced by `apt-get`, which remains the
exclusive kernel lock owner through the complete exact-version apt/dpkg
transaction. Maintainer scripts require that `FLOCK WRITE` owner to be an
apt-wrapper ancestor. Direct marker-only, shared-lock, `apt-get install`,
`apt upgrade`, or `dpkg` invocation is refused.
First install uses the same wrapper.

Own repository, hosted on GitHub (GitHub Pages), built + signed in CI on tag.

## Pieces

1. **Build the `.deb`** — `cargo-deb` (Rust-native; reads `[package.metadata.deb]`
   in `Cargo.toml`). The `build` matrix in
   `.github/workflows/release.yml` builds amd64 and arm64 for tagged releases
   and explicit `workflow_dispatch` runs:
   - `jdx/mise-action` (rust + zig + prebuilt cargo-binstall + cargo-zigbuild)
   - sccache + mold
   - Matrix over `x86_64-unknown-linux-gnu` + `aarch64-unknown-linux-gnu`
   - `cargo zigbuild --release --locked --target $TGT` (modern glibc, no .2.17 shims — those are only for portable tarballs)
   - cargo-deb is pinned by mise and installed only as a verified prebuilt
     cargo-binstall/QuickInstall artifact; CI fails instead of compiling it.
   - `cargo deb -p velnor-runner --target $TGT --no-build --deb-version "$VERSION"`
   Produces `velnor-runner_<version>_<arch>.deb` (amd64 + arm64).
   - Package contents: single product binary → `/usr/bin/velnorctl` (the
     velnorctl command center; the velnor-runner crate is a lib-only runtime); systemd units →
     `/usr/lib/systemd/system/velnor-daemon.service` and
     `velnor-daemon@.service`; default config → `/etc/velnor/velnor.env`;
     operator-owned tokens → `/etc/velnor/secrets.env` or
     `/etc/velnor/<instance>.secrets.env` (0600, never shipped).
   - `maintainer-scripts`: `postinst` creates the state/cache/runtime/log dirs,
     migrates any legacy token out of `velnor.env`, reloads systemd, and leaves
     start/enable under operator control. The daemon currently runs as root
     because the packaged default `docker` backend owns host Docker job
     containers and bind mounts. The same package also ships pinned microVM
     identity under `/usr/share/velnor/microvm/` (Firecracker/jailer 1.16.1,
     kernel 6.1.102, `rootfs.ext4`, `velnor-guest-agent`, `pins.json`,
     `manifest.json` sha256). `postinst` verifies those bytes offline. Operator
     selection remains `/etc/velnor/execution.toml` `[execution] backend =
     "docker"` (packaged default) or `"microvm"` with no fallback.
   - systemd unit: `Restart=always`, separate config and secret environment
     files, so the PAT stays off argv/`/proc`; `WantedBy=multi-user.target` for
     boot start. The default and templated units have the same sandboxing and
     storage contract.

2. **Build the apt repository** — `reprepro` (standard, simple, signs Release).
   - `conf/distributions`:
     ```
     Origin: Velnor
     Label: Velnor
     Codename: stable
     Architectures: amd64 arm64
     Components: main
     SignWith: <GPG key id>
     ```
   - `reprepro includedeb stable velnor-runner_*.deb` → builds `dists/` +
     `pool/`, generates `Packages`, `Release`, and a **GPG-signed** `InRelease` /
     `Release.gpg`.

3. **Sign** — a dedicated GPG signing key.
   - Private key and passphrase are stored securely by maintainers and manually copied into the GitHub repository secrets `APT_GPG_PRIVATE_KEY` / `APT_GPG_PASSPHRASE` (no loading from external secret managers happens inside GitHub Actions). Imported in CI for reprepro `SignWith`.
   - Public key published at `https://velnor-apt.tailrocks.com/velnor.gpg` (and in the repo) for users to install into `/etc/apt/keyrings`.

4. **Host on GitHub Pages** — the isolated signed tree (`dists/`, `pool/`,
   `velnor.gpg`) is deployed through the official Pages actions. Every index
   contains exactly the candidate and its signed rollback predecessor. The
   publisher verifies prior `InRelease`, package hashes, and publication-record
   signature before carrying that pair forward. No state branch is used. Served
   at `https://velnor-apt.tailrocks.com/`.

### Where it lives (storage decision)

- **Store = GitHub Pages.** apt fetches the signed tree over HTTPS directly.
- **Dedicated repo** `tailrocks/velnor-apt` (not the Velnor source repo) so the
  `.deb` binaries do not enter code git history. GitHub Actions deploys the
  generated signed tree directly as a Pages artifact; no `gh-pages` branch is
  used.
- **GitHub Packages does NOT support apt/deb** (npm/Docker/Maven/NuGet/RubyGems
  only) — can't use it.
- **GitHub Releases** is the alternative blob store: keep `.deb` assets in
  Releases (2 GB/asset, no repo bloat) and host only the small `Packages`/
  `Release` index on Pages pointing at the asset URLs. Use this only if the
  `pool/` ever gets large; for now velnor-runner `.deb` ≈ 12 MB and a few
  versions sit comfortably inside Pages' ~1 GB repo / ~100 GB-month limits.
- **Keep it lean**: the index exposes exactly two versions: current candidate
  and its verified rollback predecessor. Older `.deb` files remain in immutable
  historical Releases but are not indexed.

## CI (GitHub Actions, on tag `v*`)

**Policy:** You should always use GitHub Actions for GitHub Pages deployments (never "Deploy from a branch"). Use `actions/configure-pages`, `actions/upload-pages-artifact`, and `actions/deploy-pages`. The index on Pages is always for currently published versions only.

`.github/workflows/release.yml`:
1. Resolves and validates the immutable tag/commit, then builds and checks the
   amd64 and arm64 binaries, tarballs, guest images, and `.deb` packages.
2. Stages per-arch `.deb` files and SHA-256 sidecars as artifacts, with package
   version, architecture, contents, and size guards.
3. Assembles and independently re-verifies one `release-record.json` binding
   the source commit, package and binary digests, OCI image digests, and APT
   coordinate.
4. Creates or verifies the source GitHub Release and attaches the release
   record, checksums, tarballs, and `.deb` assets. Package subjects then pass
   through the signed package-signer workflows.
5. Performs no APT publication: it holds no APT credential, does not upload to
   `tailrocks/velnor-apt`, and does not dispatch an APT publisher. The signed
   release-record/publisher process in `tailrocks/velnor-apt` independently
   consumes and verifies the record before publishing signed APT metadata.

## User install (modern `signed-by` keyring — not deprecated `apt-key`)

```bash
sudo install -m0755 -d /etc/apt/keyrings
curl -fsSL https://velnor-apt.tailrocks.com/velnor.gpg \
  | sudo tee /etc/apt/keyrings/velnor.gpg > /dev/null
echo "deb [signed-by=/etc/apt/keyrings/velnor.gpg] https://velnor-apt.tailrocks.com stable main" \
  | sudo tee /etc/apt/sources.list.d/velnor.list
sudo apt update
sudo install -d -m 0750 /run/velnor
sudo /usr/bin/flock --exclusive --nonblock --no-fork \
  /run/velnor/package-transaction.lock \
  apt-get install velnor-runner=X.Y.Z
# then set non-secret config and the operator-owned token separately:
sudo nano /etc/velnor/velnor.env      # URL=..., NAME=..., LABELS=..., SLOTS=...
sudo install -m 0600 /dev/null /etc/velnor/secrets.env
sudo nano /etc/velnor/secrets.env     # GITHUB_TOKEN=...
sudo systemctl enable --now velnor-daemon
```

`signed-by=` scopes the key to this repo only (current security best practice;
avoids the deprecated global `apt-key` / `trusted.gpg.d`).

## Notes / decisions

- **cargo-deb vs nfpm**: cargo-deb is the Rust-native fit (config lives in
  `Cargo.toml`); nfpm is fine too if we later want rpm as well.
- **reprepro vs aptly**: reprepro is simpler for a single-arch single-suite repo
  and signs Release out of the box; aptly if we later need snapshots/mirroring.
- **arch**: amd64 + arm64 via matrix (using zigbuild for cross on ubuntu runner).
  The unified `.github/workflows/release.yml` is the sole Velnor release
  coordinator; it builds and attaches `.deb` assets and emits the release
  record, while signed APT publication is handled by the `velnor-apt` publisher.
- **key rotation**: document a key-rotation procedure; expired signing keys break
  `apt update` for everyone (see the `gh` CLI incident).

## Maintainer release and Sentry deployment

Normal Sentry package deployment uses the configured signed `velnor-apt`
repository and the release workflow below. This rule covers first install,
upgrade, downgrade, rollback, and forward recovery. The only exception is the
tightly bounded chicken-egg bootstrap below, allowed only when fresh evidence
proves the Velnor lane failed or is unavailable, that failure blocks building
Velnor itself, an operator explicitly authorizes incident recovery, and exactly
one Debian package is built on this host from the exact pushed campaign SHA.

1. Ensure the release commit is signed off, pushed, and green. Create a signed
   `vX.Y.Z` tag on that exact commit and push only that tag.
2. Monitor Velnor's `Release` workflow. It must build amd64 and arm64, validate
   the packages, and publish the signed release record and immutable release
   assets. The `velnor-apt` publisher independently consumes and verifies that
   record before publishing the signed APT repository; the Velnor source
   release does not dispatch it.
3. Monitor apt publication and Pages deployment. Before touching Sentry, verify
   the `InRelease` signature and signed publication record bind the requested
   tag, source-record digest, exact candidate, and exact rollback predecessor.
   On Sentry, `apt-get update` must complete without a signature warning and
   policy must report the requested exact version from the configured HTTPS
   repository:
   ```bash
   sudo apt-get update
   apt-cache policy velnor-runner
   ```
4. Drain all intended Velnor daemons. Confirm GitHub reports every managed
   runner idle and no Velnor job container remains. Then install the verified
   exact version, never an unpinned candidate:
   ```bash
   sudo install -d -m 0750 /run/velnor
   sudo /usr/bin/flock --exclusive --nonblock --no-fork \
     /run/velnor/package-transaction.lock \
     apt-get install velnor-runner=X.Y.Z
     dpkg-query -W velnor-runner
   ```
   For a one-scope canary on a multi-scope host, preserve unrelated scopes and
   pass their exact drained unit(s) to the same locked apt transaction. The
   package maintainer scripts require those target units to be inactive and
   leave unrelated services, timers, and the guardian running:
   ```bash
   sudo VELNOR_DRAINED_UNITS=velnor-daemon.service \
     /usr/bin/flock --exclusive --nonblock --no-fork \
     /run/velnor/package-transaction.lock \
     apt-get install velnor-runner=X.Y.Z
   ```
   An unscoped transaction still requires a fully drained Velnor host and an
   inactive guardian. The scoped variable accepts only `velnor*.service` and
   `velnor*.timer` unit names; it is not a bypass for the transaction lock or
   the installed-release coherence checks.
5. Activate the immutable release record, verify the installed package and
   binary plus exact OCI digest and complete labels, inspect
   atomic `active`/`previous` pointers, then start only the intended instance
   units. Run doctor and the fixture smoke before restoring traffic.
6. Rollback uses only the exact signed predecessor already retained in the
   repository: drain, verify its signed identity, `apt-get update`, and
   `install -d -m 0750 /run/velnor` plus the `flock --no-fork` exclusive
   wrapper around `apt-get install velnor-runner=<exact-predecessor>`. Prove rollback, then use
   the same exact signed-APT procedure to move forward. Never republish an old
   release merely to roll back.

### Authorized chicken-egg bootstrap (exception only)

When fresh evidence proves the Velnor lane is failed or unavailable and that
failure prevents building Velnor itself, an explicitly authorized operator may
temporarily build exactly one Debian package on this host from the exact pushed
campaign SHA. This is incident recovery, not a second normal release mechanism:

1. Build from a clean checkout at that exact SHA. Before transfer, publication,
   or installation, create an immutable signed source record binding the exact
   source SHA, package name, package version, architecture, package SHA-256,
   and exact `velnor-apt` repository coordinate (HTTPS origin, suite,
   component, and package path). Record the authorization, provenance, package
   path, and authorized signing-key fingerprint; verify the record signature and
   every binding before proceeding. Sign the repository metadata with the
   authorized release key.
2. Snapshot the current Sentry state and verify the Sentry host fingerprint.
   Verify the signed source record and package SHA-256 before transfer, transfer
   only that Debian package to the authorized staging path through noninteractive
   `ssh sentry`, and verify the same SHA-256 again after transfer. Do not transfer
   source, a checkout, a raw binary, or any other artifact; abort on any identity
   or digest mismatch.
3. Publish that exact package into the signed `velnor-apt` repository through
   the authorized host/Sentry release path at the exact coordinate in the signed
   source record. Verify the record signature and binding, package checksum,
   the exact APT `Release` metadata, its clearsigned `InRelease` signature and
   detached `Release.gpg` signature, the trusted repository signing-key
   fingerprint, and matching metadata/package checksums and bytes. Then prove
   the exact pinned package name/version/architecture is available from the
   configured HTTPS APT source.
4. Drain the affected units and install only the exact published candidate
   through APT under the existing exclusive
   `/run/velnor/package-transaction.lock` `flock` wrapper. Never use `dpkg -i`,
   an APT local path, or an unpinned install.
5. Prove the signed source-record binding and source, package, and binary
   identity; all required signatures and checksums;
   health/readiness; runner registration and lease state; the original
   reproducer; rollback to the retained signed predecessor; forward recovery;
   and fresh `velnor`, `github`, and `both` lane results. Before retrying,
   cancel stale pending/running validation and remove only validation-owned
   stale registrations, prove both are clear, and monitor only the new runs.
6. Record authorization, commands, provenance, signed source-record digest and
   bindings, both transfer hashes, package and signing identities, health, lane,
   predecessor, rollback, and recovery evidence in the incident ledger. Retire
   this path as soon as Velnor self-build is restored.

Raw source, checkouts, and raw binaries are never transferred or deployed; the
exception permits the clean checkout only on this host as the exact build input.
Outside this explicitly authorized exception, never use an unpinned or
unrelated artifact; deploy a local or downloaded `.deb`; use direct `dpkg -i`;
install an APT local path; bypass signature or package verification; or silently
fail over to GitHub. A checksummed immutable release-record download is
activation metadata only; it does not install executable code and cannot replace
any normal signed-APT step.

Verified 2026-07-18 against both repositories and the live host:
`tailrocks/velnor-apt` publishes amd64+arm64 with a signed reprepro index
through GitHub Actions Pages; Sentry has the scoped keyring and source
configured. The v0.1.58 dual-architecture source package run is
[`29638641012`](https://github.com/tailrocks/velnor/actions/runs/29638641012)
and its signed repository/Pages run is
[`29638791029`](https://github.com/tailrocks/velnor-apt/actions/runs/29638791029).
Those run URLs are historical proof of the chain, not a current version pin;
every later deployment must follow this same tag-to-signed-apt chain and verify
the exact version reported by `apt-cache policy` before changing a host.
