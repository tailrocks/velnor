# ChainArgos → Velnor migration — outstanding checklist

Status snapshot (2026-06-05). ChainArgos/java-monorepo is migrated: production
workflows default to the Velnor self-hosted runner, switchable to GitHub-hosted
or both via the `lanes`/`lane` dispatch input. This file tracks what is DONE and
what is LEFT.

## Done (shipped + verified)

- [x] **Runner selection in real workflows** (PR ChainArgos #1343). `matrix-setup`
      job + `runs-on: ${{ fromJSON(matrix.config.runner) }}`; default velnor,
      schedule = both, dispatch `lanes` = velnor|github|both. Applied to
      `rust.yml`, `ansible.yml`, `rust-docker.yml`. `renovate.yml` +
      `kestra-build-*.yml` use a single runner (default velnor, github via
      `lane`) because they mutate state. Removed `velnor-compat.yml`,
      `velnor-extra.yml`, `velnor-lfs-check.yml`.
- [x] **Behavior parity** — every workflow green on both lanes.
- [x] **Performance** — Velnor 1.6–3.4× faster on Rust, ≈equal Ansible, faster
      Docker (warm buildkit cache).
- [x] **Step list parity** — Set up job / checkout / steps / post / Complete job,
      correct numbers + conclusions + RFC3339 timestamps.
- [x] **Step-record + step-log uploads hardened** — routed through curl + retry
      so nothing drops under heavy concurrent load (was: timestamps/steps missing
      under lanes=all).
- [x] **Per-line timestamp format** — emit GitHub's `.fffffffZ` (7-digit) form so
      the UI strips it (was second-precision → leaked into log content). Guarded
      by `unix_now_iso8601_is_github_strippable` test.
- [x] **`docker logs <job-container>` mirrors the UI** — PID 1 tails a live
      console file velnor appends each masked step's lines to.
- [x] **Checkout step body** — full git trace (`[command]git init/remote/fetch/
      checkout` + output, token masked) instead of a 5-line summary.
- [x] **Post Run actions/checkout body** — shows the credential cleanup
      (`git config --local --unset-all …extraheader`) instead of empty.
- [x] **Daemon `--replace` no longer crashes** on a busy slot (422) — best-effort
      delete + skip un-configurable slots (fails only if 0 usable).
- [x] **Git LFS download** (`lfs: true`) support in checkout.

## To do

### Priority 1 — package the Velnor daemon properly (Debian package)
Proper distribution: a **Debian package (`.deb`)** that installs the runner the
right way, instead of the current ad-hoc transient `systemd-run` + manual
git-bundle deploys. Target this as the real fix for daemon operations.

- [ ] **apt-native repo + `.deb`** — full design in
      [`docs/debian-apt-repo.md`](../docs/debian-apt-repo.md): cargo-deb builds
      the `.deb`, reprepro builds a GPG-signed apt repo, GitHub Pages hosts it,
      GitHub Actions publishes on tag, users `apt install velnor-runner` via a
      `signed-by` keyring and `apt upgrade` to update.
  - [x] **apt repo created + scaffolded**: `donbeave/velnor-apt` (public) — README
        with install steps, `conf/distributions` (reprepro), `publish.yml`
        (download `.deb` → reprepro sign → publish gh-pages).
  - [x] **velnor side: build the `.deb`** — `[package.metadata.deb]` +
        `crates/velnor-runner/debian/` (unit `Restart=always`, `velnor.env`
        conffile, postinst/prerm/postrm) + `.github/workflows/release-deb.yml`
        (cargo deb on `v*` tag → attach to Release → trigger velnor-apt). Layout
        verified locally with cargo-deb 3.7.0. Token via `GITHUB_TOKEN` env (off
        argv). NOTE: runs as `User=root` (manages Docker) — not a `velnor` user;
        harden later if desired.
  - [x] **release-deb.yml validated** — tag `v0.1.0-rc1` built a clean amd64
        `.deb` on Ubuntu CI (run 26983987624) + attached it to the GitHub
        Release. velnor side of the pipeline works end-to-end.
  - [x] **Decision: public binary via Pages** — velnor-apt is public; the
        compiled `.deb` is public, velnor source stays private.
  - [x] **GPG signing key created + wired** — RSA-4096 key
        `B3766CA119CCC5CFEF8F844777786ADA478A7BCD`; `APT_GPG_PRIVATE_KEY` +
        `APT_GPG_PASSPHRASE` (empty: passphraseless) secrets set on velnor-apt;
        `SignWith` set; public `velnor.gpg` published. Private key only lives in
        the velnor-apt secret — rotate via the same steps if needed.
  - [x] **GitHub Pages enabled + LIVE** — served at
        `https://www.zhokhov.com/velnor-apt` (donbeave's custom Pages domain;
        HTTPS works, `donbeave.github.io/velnor-apt` 301-redirects there).
  - [x] **First publish done + VERIFIED** — `publish.yml` signed + published
        `velnor-runner 0.1.0-rc1`; `InRelease` shows **Good signature**, the
        `.deb` is reachable, `apt install velnor-runner` from
        `https://www.zhokhov.com/velnor-apt` works. `publish.yml` reads the `.deb`
        from velnor-apt's OWN release (same-repo token — no private-repo read).
  - [ ] **Only remaining: auto-publish on tag** — add a PAT with write on
        `donbeave/velnor-apt` as a velnor secret `VELNOR_APT_TOKEN`. Then a
        `git tag vX.Y.Z` makes `release-deb.yml` build the `.deb`, upload it to
        velnor-apt's release, and trigger `publish.yml` — fully automatic. Until
        the PAT exists, publish manually (the workflow prints the two commands).
- [ ] **`.deb` package** (e.g. via `cargo-deb` or `nfpm`):
  - ships the `velnor-runner` binary to `/usr/bin` (or `/opt/velnor`)
  - installs a **systemd unit** `velnor-daemon.service` (`Restart=always`,
    `RestartSec`, `WantedBy=multi-user.target` → boot start)
  - creates a dedicated service user + state dirs (`/var/lib/velnor`,
    `/etc/velnor` for the `.env`/PAT), correct perms
  - config file (URL, name, labels, slots, work-dir, token) under `/etc/velnor`
    read by the unit (no secrets on argv — keeps the PAT out of `/proc`)
  - `postinst`/`prerm` enable/disable + start/stop; clean upgrade path
    (`apt install ./velnor-runner.deb` to update the binary + restart)
  - apt repo or release artifact so install/upgrade is `apt`-native
- [ ] **Stop idle-exit churn** — currently `--idle-timeout-seconds 2400` exits
      after 40 min idle; combined with rapid restarts it piles up stale "busy"
      runner registrations until "0 usable slots" exits. For a packaged
      long-running service, drop the idle-timeout, and have startup proactively
      delete *all* of its own prior named runners before registering.
- [ ] **Boot/auto-recovery verified** — daemon comes back after reboot + after a
      crash without manual `gh api DELETE runners` + restart.
- [ ] **Docker network leak** — interrupted jobs / daemon crashes leave
      `velnor-net-*` networks behind; they accumulate until Docker's address pool
      is exhausted (`docker network create … all predefined address pools have
      been fully subnetted`) and then ALL new jobs fail. Hit this after 27 leaked
      networks from this session's restart churn. Fix: remove the job network on
      teardown reliably (even on crash/cancel), and `docker network prune` stale
      `velnor-net-*` on daemon startup. Interim: `docker network rm $(docker
      network ls --filter name=velnor-net -q)`.

> Interim (until the `.deb` exists): a hand-written
> `/etc/systemd/system/velnor-daemon.service` with `Restart=always` + an
> `EnvironmentFile=/etc/velnor/velnor.env` already removes the worst of the
> churn. The `.deb` is the durable answer.

### Priority 2 — live UX
- [ ] **Live-feed WebSocket keepalive** — feed Broken-pipe under load; reconnect +
      resend masks it (final v2 blob unaffected) but the *live* console is choppy.
      Add ping/keepalive so the streaming console doesn't stutter.

### Priority 3 — second repo
- [ ] **Migrate `ChainArgos/blockchain-nodes` to Velnor** — uses `lfs: true`
      (download support already built + proven). Apply the same runner-selection
      pattern; verify green on both lanes.

### Platform gaps (GitHub V2 sends third-party runners a leaner message — likely NOT runner-fixable; decide whether to work around)
- [ ] **Downloadable log archive empty** (`gh run view --log` / "Download log
      archive"). Built from the **v1 timeline log store**, which needs a
      `scopeIdentifier` + the `pipelines…/_apis/distributedtask/…/logs` host —
      both ABSENT from velnor's V2 broker message (`plan.scopeIdentifier=null`,
      only the run-service host given). The v1 timeline-log flow was implemented
      and reverted as dead code. Web UI logs (v2) ARE complete.
      - Optional workaround: upload the full job log as a regular **build
        artifact** (`job-log.txt`) — the artifacts API IS reachable, so that
        download would work (not the native button, but a real archive).
- [ ] **Step display names show `Run <command>`** instead of the YAML `name:`
      (Tests / Rustfmt / Clippy). Proven at the wire: the broker step has
      `name=__run`/`__run_2`, `displayName=None` — GitHub strips the name. Only a
      fragile workflow-YAML parse (glob `.github/workflows`, match job by
      `system.github.job` + `__run_N` index) could recover it, at the risk of
      *wrong* labels. Currently left as GitHub's own unnamed-step format.

### Priority 4 — optional polish
- [ ] **buildx local cache** (`type=gha` → local) for faster repeat Docker builds
      on the Velnor lane (GHA cache backend is unavailable off-GitHub).
- [ ] **Action-adapter text** (setup-mold / sccache / mise) — native adapters
      print different text than the real JS actions. Functionally equivalent;
      exact-text matching is brittle + low value. Deprioritized.
