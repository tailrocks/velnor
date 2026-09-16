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

## Trust-scope overlay (D18)

Same-repo **pull_request** jobs on a **trusted** pool (`VELNOR_TRUST_SCOPE=trusted`):

- **Write scope:** `pr` — mbx, targets, executables, and PR-local caches.
- **Read-through:** `trusted` — Cargo registry cache/index/git db overlay from
  trusted stores when overlay mount succeeds; otherwise PR scope only (warned in logs).

Fork and unknown jobs remain on the `untrusted` floor. Trusted events (main
push, schedule, dispatch on default branch) write the `trusted` scope only.

**Partial hooks:** admission derives scope in `trust_class::AdmittedTrust`;
`container::JobContainerSpec::prepare_store_overlays` mounts overlays;
`storage::teardown_store_overlays` unmounts at job cleanup.

**Verify:** consecutive same-repo PR Velnor jobs show mbx hits on unchanged
source; trusted main push saves remain in `trusted/` only (`velnorctl cache du`
lists `pr/` vs `trusted/` separately).
