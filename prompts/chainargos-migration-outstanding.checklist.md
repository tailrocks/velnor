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

### Priority 1 — make the Velnor daemon production-grade (operational)
- [ ] **Real systemd service** (not transient `systemd-run`): unit file with
      `Restart=always`, `RestartSec`, `WantedBy=multi-user.target` (boot start).
- [ ] **Stop idle-exit churn** — currently `--idle-timeout-seconds 2400` exits
      after 40 min idle; combined with rapid restarts it piles up stale "busy"
      runner registrations until "0 usable slots" exits. Either drop idle-timeout
      for a long-running service, or have startup proactively delete *all* of its
      own prior named runners before registering.
- [ ] **Boot/auto-recovery verified** — daemon comes back after reboot + after a
      crash without manual `gh api DELETE runners` + restart.

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
