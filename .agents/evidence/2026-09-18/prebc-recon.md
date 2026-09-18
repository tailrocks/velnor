# Pre-B/C Recon — 2026-09-17 (~17:52 UTC bastion time). READ-ONLY, no mutations.

## TRACK 1 — Bastion inventory (root@37.27.110.241)

SSH succeeded on attempt 1 (no retries). Host is a clean, idle, high-spec box.

- `uname -a`: `Linux bastion 6.12.94+deb13-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.12.94-1 (2026-06-20) x86_64 GNU/Linux`
- CPU: AMD EPYC 9454P 48-Core, 96 vCPU (2 threads/core, 1 socket, 1 NUMA node). L1 1.5+1.5 MiB×48, L2 48 MiB, L3 256 MiB. Boost on, scaling 49%.
- `free -m`: 128494 total / 1996 used / 126630 free / 126498 avail (~125 GiB, ~1.5% used). Swap 4095/0.
- Disks: **second NVMe PRESENT and virgin** —
  - `nvme0n1` 3.5T INTEL SSDPF2KX038T1 (SN BTAX340600DS3P8CGN): p1 256M /boot/efi, p2 4G [SWAP], p3 1G /boot, p4 3.5T `/`
  - `nvme1n1` 3.5T INTEL SSDPF2KX038T1 (SN BTAX3415052M3P8CGN): **no partitions, unmounted**
- `df -hT`: `/dev/nvme0n1p4` xfs 3.5T, 27G used, 3.5T avail, 1% on `/`. Rest is tmpfs/efivarfs.
- Docker: **NOT installed** (`docker: command not found`); no containers possible.
- Running services (10, minimal): atd, cron, dbus, getty@tty1, ssh, systemd-journald, systemd-logind, systemd-timesyncd, systemd-udevd, user@0. No docker/velnor/cloud-agent units.
- Velnor residue: **none** — `/run/velnor` absent; `dpkg-query -W velnor-runner` → no match; `dpkg-query -W | grep -i velnor` → empty.
- Uptime 22:38, load 0.00/0.00/0.00. IPs: 37.27.110.241, 2a01:4f9:3070:2142::2.

Track-1 verdict: zero-state box, B/C starts from scratch. Only gap vs plan: docker absent (expected — B/C installs the runner stack).

## TRACK 2 — velnor-apt provenance gap (RESOLVED: in-repo publisher, since deleted)

**Answer to the gap**: nothing external publishes the feed. The publisher was the repo's own
`.github/workflows/publish.yml` ("Publish apt repo", workflow id 289495862), which existed at the
deploy commit and was **deleted Sep-16** by `79782849` "chore(ci): clean-room regen — class C apt
workflows omitted (#228)" (author Alexey Zhokhov). Feed has been frozen since Sep-14.

Publisher chain (all timestamps UTC 2026-09-14, fully consistent):
1. Commit `a2c52656` "chore(package): update verified release (#222)" by `tailrocks-package-updater[bot]`
   (committer GitHub), 14:51:11Z — touched ONLY `package-state-preview.json` (6+/6-), a push-trigger path.
2. Run **34858321769** "Publish apt repo", event `push`, head `a2c52656`, created 14:51:16Z,
   **success**. Lane: GitHub `ubuntu-26.04` writer (push path forces GitHub lane; Velnor lane only via dispatch).
   Jobs: Build 14:51:18→14:52:49, Deploy 14:52:54→14:53:24, Publish-required →14:53:30.
3. InRelease `Date:` 14:52:13 → Pages deployment id **6439747926** created 14:52:49
   (deployment object creator: `donbeave`) → CDN `last-modified` 14:53:13 → deploy status `success` 14:53:24
   (log_url → run 34858321769/job/104024170453).

Signing (names/ids only, no key material):
- Secret **name**: `APT_GPG_PRIVATE_KEY`; workflow asserts its fingerprint == committed `velnor.gpg` at runtime.
- Key: 4096-bit RSA, uid `Velnor APT (velnor-apt.tailrocks.com) <apt@tailrocks.com>`,
  primary fpr `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`, signing subkey `CD4693750A4BA4F12BC9ABFD857FCD279679A34B`.
- Local `gpg --verify` against served `velnor.gpg` (throwaway GNUPGHOME): **Good signature**.
  Committed `velnor.gpg` at main has identical fingerprints — served key == pinned key.
- Repo build tool: **apt-ftparchive** via `scripts/verify-release.sh publish` (`reprepro` appears only in
  legacy comments/sentinel name `.reprepro-ok`).

Feed content (`dists/stable`, Origin/Label Velnor, Components main, amd64+arm64):
- `velnor-runner` **0.1.273** + **0.1.274** per arch (current + gpgv-verified rollback pair),
  `pool/main/v/velnor-runner/velnor-runner_0.1.27x_{amd64,arm64}.deb`.
- Declared sources: stable `tailrocks/velnor` tag `v0.1.274` (commit `120f2236…`),
  preview `0.1.274~preview.145+d3e441f` (main `d3e441fb…`). NOTE: velnor-apt's own GitHub Releases
  stopped at `v0.1.121` (Jul-22) — live debs come from **tailrocks/velnor** releases, verified via
  `verify-release.sh` (+ SLSA attestation gate on `ci-release-package-signer.yml` for preview).

Freshness: deploy SHA `a2c52656` is 13 commits behind main `d62820d`, but `package-state.json` and
`package-state-preview.json` are **byte-identical** at both — feed matches current declared state.
The 13 are workflow/regen churn. **However, the republish path is dead**: main has no publish.yml
(only `ci-unit-docs.yml` + dynamic `pages-build-deployment`), so the next package-state bump will
NOT deploy. Pages: `build_type: workflow`, cname verified, HTTPS enforced, cert expires 2026-11-06.

### B4-ready provenance statement
Live feed `https://velnor-apt.tailrocks.com/dists/stable/InRelease` (since 2026-09-14 14:53Z) was
built by in-repo workflow `Publish apt repo` run 34858321769 from commit `a2c52656` (#222, package-updater bot),
assembled with apt-ftparchive, signed with the pinned Velnor APT RSA key (subkey `…79679A34B`, secret
`APT_GPG_PRIVATE_KEY`), and deployed via Pages deployment 6439747926. Source of truth for debs is
`tailrocks/velnor` releases (v0.1.274 + preview), not velnor-apt releases (stale at v0.1.121).
Publisher workflow removed 2026-09-16 (#228) → feed frozen-but-current; B4 must restore or replace
the publish path before the next package-state change, reusing the same pinned key.

## Exact commands run (all read-only)
T1:
1. `ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new root@37.27.110.241 'uname -a'` (probe, 1 attempt)
2. Same SSH with bundled read-only body: `uname -a; lscpu; free -m; lsblk -o NAME,SIZE,TYPE,MOUNTPOINT,MODEL,SERIAL; df -hT; docker info|version; systemctl list-units --type=service --state=running | head -40; ls -la /run/velnor; dpkg-query -W velnor-runner; dpkg-query -W | grep -i velnor; docker ps -a; uptime; hostname -I`
T2 (all `gh api` GET / `gh run list` / `gh release view` / `curl -s` GET; local gpg verify in throwaway GNUPGHOME; scratch /tmp/inrelease.tmp + /tmp/velnor-gpg.tmp removed after):
3. `gh run list --repo tailrocks/velnor-apt --limit 20`; `gh api repos/tailrocks/velnor-apt/actions/workflows`
4. `gh api repos/tailrocks/velnor-apt/pages`; `…/environments`; `…/deployments` (first 8); `gh release list --repo tailrocks/velnor-apt --limit 10`; `…/branches`; `…/tags`
5. `gh api repos/tailrocks/velnor-apt/commits/a2c5265…`; `…/compare/a2c5265…...d62820d`; `…/commits?path=.github/workflows/publish.yml&per_page=10`; `…/git/trees/a2c5265…?recursive=1` (workflows filter)
6. `curl -s -o /tmp/inrelease.tmp …/dists/stable/InRelease`; `gpg --verify`; `curl -s -o /tmp/velnor-gpg.tmp …/velnor.gpg`; `gpg --show-keys --with-colons`; `curl -sI` headers
7. `gh api …/deployments` (id/creator); `…/deployments/6439747926/statuses`; `gh run list --limit 100 --json …` filtered to 2026-09-14
8. InRelease body parse; `curl -s …/dists/stable/main/binary-{amd64,arm64}/Packages`
9. `gh api …/actions/runs/34858321769` (+ `/jobs`); `…/contents/.github/workflows/publish.yml?ref=a2c5265…`
10. Throwaway-GNUPGHOME import + `gpg --verify` (Good); `…/commits/a2c5265…` files; `gh release view v0.1.121 --json assets`
11. `package-state.json` + `package-state-preview.json` at `a2c5265…` vs main; `…/commits?path=package-state{,-preview}.json&per_page=4`
12. publish.yml grep (signing/build steps, 482 lines); committed `velnor.gpg` fingerprints; del-commit `79782849`
13. `scripts/verify-release.sh?ref=a2c5265…` grep (apt-ftparchive vs reprepro); scratch cleanup
