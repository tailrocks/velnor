# Velnor fleet host operations (Phase 6)

Committed artifacts and operator steps. No live host access from CI — apply on
each runner host after package install.

## Storage root

Set on every fleet host so the canonical `v1` layout is used:

```bash
# /etc/velnor/velnor.env (merge from config/fleet/velnor-host.env after regen)
VELNOR_STORAGE_ROOT=/var
```

With the packaged default, stores live under:

```text
/var/cache/velnor/v1/<trust-scope>/<class>/…
/var/lib/velnor/work/…
```

Record baseline after first GC:

```bash
velnorctl cache --work-dir /var/lib/velnor/work du
velnor-runner storage status   # when available on the host
```

## Scheduled cache GC

`velnor-runner` ships `velnor-cache-gc.timer` (alongside
`velnor-fleet-policy-audit.timer`) and its `postinst` enables it on install and
on every upgrade; `systemctl mask velnor-cache-gc.timer` is the opt-out the
package honours. An enabled timer never blocks an upgrade: the gc service is a
`Type=oneshot` wrapped in the shared package-transaction lock, and the
maintainer-script drain gates exempt exactly that class.

```bash
systemctl list-timers velnor-cache-gc.timer velnor-fleet-policy-audit.timer
```

The service runs one pass per packaged instance:

```bash
velnorctl cache gc --yes
```

Budgets come from `/etc/velnor/velnor.env` (`VELNOR_BUDGET_*`). Target:
`velnorctl cache du` total ≤ 50 GiB after sustained load; alert when host
available disk < 5 GiB.

## BuildKit GC (trusted docker hosts)

Copy `config/fleet/buildkitd.gc.toml` to the host BuildKit config path, or
reference it from `buildkitd-config-inline` on trusted `velnor-host-docker`
jobs. Keeps builder disk under ~40 GiB used with 10 GiB reserved and 5 GiB
min free.

## Trust-scope `pr` seed (D18)

Same-repo **pull_request** jobs on a **trusted** pool (`VELNOR_TRUST_SCOPE=trusted`):

- **Write scope:** `pr` — mbx, targets, executables, Cargo registry/git db,
  and PR-local caches; persistent, bind-mounted read-write, shared by every
  slot on the host.
- **Seed:** before the job container starts the daemon copies every file of
  the `trusted` Cargo store (`registry/cache`, `registry/index`, `git/db`)
  that `pr` lacks into `pr` — a copy, never a hard link or overlay, so a PR
  rewriting a seeded file in place cannot touch `trusted`. Bounded by the
  compiler store budget; newest entries first, skipped entries logged.

Fork and unknown jobs remain on the `untrusted` floor. Trusted events (main
push, schedule, dispatch on default branch) write the `trusted` scope only.

**Hooks:** admission derives scope and seed source in
`trust_class::AdmittedTrust`; `storage::seed_cargo_store` copies before
`github_job_container_spec` builds the mounts (`runner.rs`,
`execute_script_job_inner`).

**Verify:** the daemon log shows `seeded pr cargo store from trusted: N files,
M bytes, T ms` per PR admission; consecutive same-repo PR Velnor jobs show
mbx hits on unchanged source; trusted main push saves remain in `trusted/`
only (`velnorctl cache du` lists `pr/` vs `trusted/` separately).
