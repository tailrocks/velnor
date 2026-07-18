# Velnor runner — Debian package + apt-native repository

**Status: implemented and fully automatic** since v0.1.4 (amd64) / v0.1.6
(arm64): `git tag vX.Y.Z && git push origin vX.Y.Z` runs CI, builds both debs with a
binary-presence + size guard, attaches tar.gz + deb assets to the GitHub
release, uploads to `tailrocks/velnor-apt`, and triggers the signed reprepro
publish to GitHub Pages — zero manual steps. The packaged daemon ships
never-exit supervision, sd_notify watchdog, `velnor-daemon@<name>` template
instances, `/etc/velnor/secrets.env` (0600, operator-owned), and
`velnor-doctor` timers. The design below documents the pieces.

Goal: install and upgrade the Velnor runner daemon with native apt:

```bash
sudo apt update && sudo apt install velnor-runner   # install
sudo apt upgrade                                     # upgrade later
```

Own repository, hosted on GitHub (GitHub Pages), built + signed in CI on tag.

## Pieces

1. **Build the `.deb`** — `cargo-deb` (Rust-native; reads `[package.metadata.deb]`
   in `Cargo.toml`). Both holla and velnor use the exact same approach in their
   `release-deb.yml` (on tag `v*` or workflow_dispatch):
   - `jdx/mise-action` (rust + zig + cargo:cargo-zigbuild)
   - sccache + mold
   - Matrix over `x86_64-unknown-linux-gnu` + `aarch64-unknown-linux-gnu`
   - `cargo zigbuild --release --locked --target $TGT` (modern glibc, no .2.17 shims — those are only for portable tarballs)
   - `cargo deb -p velnor-runner --target $TGT --no-build --deb-version "$VERSION"`
   Produces `velnor-runner_<version>_<arch>.deb` (amd64 + arm64).
   - Package contents: binary → `/usr/bin/velnor-runner`; systemd units →
     `/usr/lib/systemd/system/velnor-daemon.service` and
     `velnor-daemon@.service`; default config → `/etc/velnor/velnor.env`;
     operator-owned tokens → `/etc/velnor/secrets.env` or
     `/etc/velnor/<instance>.secrets.env` (0600, never shipped).
   - `maintainer-scripts`: `postinst` creates the state/cache/runtime/log dirs,
     migrates any legacy token out of `velnor.env`, reloads systemd, and leaves
     start/enable under operator control. The daemon currently runs as root
     because it owns Docker-backed job containers and bind mounts.
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

4. **Host on GitHub Pages** — the reprepro output tree (`dists/`, `pool/`, `velnor.gpg`) is deployed via a GitHub Actions workflow (using the official `actions/deploy-pages`). The index on Pages is generated fresh each time with only current versions (no state branch; old versions forgotten from index per maintainer preference). GitHub Pages is deployed via GitHub Actions (recommended; never "Deploy from a branch"). Served at `https://velnor-apt.tailrocks.com/`.

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
- **Keep it lean**: the index on Pages includes only current versions. Old .debs remain in historical Releases (for manual download if needed) but are not part of the current apt repo.

## CI (GitHub Actions, on tag `v*`)

**Policy:** You should always use GitHub Actions for GitHub Pages deployments (never "Deploy from a branch"). Use `actions/configure-pages`, `actions/upload-pages-artifact`, and `actions/deploy-pages`. The index on Pages is always for currently published versions only.

`.github/workflows/release-deb.yml` (unified with holla's):
1. Uses the exact same pattern as holla: `jdx/mise-action` + sccache + mold + `cargo zigbuild` (latest Debian glibc only, matrix for amd64+arm64) → `cargo deb --target $TGT --no-build --deb-version "$VERSION"`.
2. Stages per-arch debs + shas as artifacts.
3. Attaches the `.deb`(s) to the velnor source GitHub Release.
4. If `GH_VELNOR_APT_TOKEN` is present, cross-uploads the .debs to the `velnor-apt` repository's Releases (same tag) and triggers `publish.yml` in the apt repo via `gh workflow run -f version=$TAG`.
5. The apt-repo's `publish.yml` then downloads the .debs from *its own* Releases (default GITHUB_TOKEN is sufficient), runs reprepro (fresh index with only the current version's debs), and deploys to Pages.

The `.deb` build + attachment to the original release is the responsibility of
the source project. The apt publisher consumes only the apt repository's own
release assets. The Pages index is generated fresh with only the requested
version; historical packages remain in Releases.
6. Also attach the raw `.deb`(s) to the GitHub Release for direct download.

Each new tag → new `.deb` in the pool → regenerated signed `Release` → `apt
upgrade` picks it up. That is the whole upgrade story.

## User install (modern `signed-by` keyring — not deprecated `apt-key`)

```bash
sudo install -m0755 -d /etc/apt/keyrings
curl -fsSL https://velnor-apt.tailrocks.com/velnor.gpg \
  | sudo tee /etc/apt/keyrings/velnor.gpg > /dev/null
echo "deb [signed-by=/etc/apt/keyrings/velnor.gpg] https://velnor-apt.tailrocks.com stable main" \
  | sudo tee /etc/apt/sources.list.d/velnor.list
sudo apt update
sudo apt install velnor-runner
# then set the token + config:
sudo nano /etc/velnor/velnor.env      # GITHUB_TOKEN=..., URL=..., LABELS=...
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
  Both projects (holla + velnor) use the identical release-deb.yml structure.
- **key rotation**: document a key-rotation procedure; expired signing keys break
  `apt update` for everyone (see the `gh` CLI incident).

## Maintainer release and Sentry deployment

1. Ensure the release commit is signed off, pushed, and green. Create the next
   `vX.Y.Z` tag on that exact commit and push only that tag.
2. Monitor Velnor's `Release deb` workflow. It must build amd64 and arm64,
   validate package contents/size, attach assets to both matching Releases, and
   dispatch `tailrocks/velnor-apt`'s `Publish apt repo` workflow.
3. Monitor the apt publish and Pages deployment. Verify the signed index and
   candidate before touching a host:
   ```bash
   curl -fsSL https://velnor-apt.tailrocks.com/dists/stable/InRelease | head
   apt-cache policy velnor-runner
   ```
4. On Sentry, drain the daemons, then install only from the configured signed
   repository:
   ```bash
   sudo apt-get update
   sudo apt-get install velnor-runner
   dpkg-query -W velnor-runner
   sudo systemctl start velnor-daemon velnor-daemon@fixture # plus instances
   ```
5. Run doctor and the fixture smoke. Roll back only to a previously published
   apt version (`apt-get install velnor-runner=<version>`); never sideload a
   release asset.

Verified 2026-07-18: `tailrocks/velnor-apt` publishes amd64+arm64 with a signed
reprepro index through GitHub Actions Pages; Sentry has the scoped keyring and
source configured. Release `v0.1.35` is the current installed baseline; the
program branch publishes `v0.1.36` through the same signed pipeline before
fixture V-A verification. The
source package run is
[`29623974593`](https://github.com/tailrocks/velnor/actions/runs/29623974593)
and the signed repository/Pages run is
[`29624452820`](https://github.com/tailrocks/velnor-apt/actions/runs/29624452820).
