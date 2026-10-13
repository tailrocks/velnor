# Velnor operator guide: current behavior

This is the executable operator surface in the current tree. `velnorctl` is the
human command center. `velnor-runner` remains the interim machine-invoked binary
for daemon, node-role, release, and Debian hooks. The product split and
current/future boundary are defined in [the documentation index](index.md)
and `crates/velnor-runner/src/service.rs:1-12`.

`NOW` below means the command has an implemented, observable result. `RESERVED`
means the parser accepts the command, but current code returns a plan, an
unavailable error, or an unsupported-operation error; it does not perform the
promised host/GitHub action.

> Navigation: [← Runner protocol](runner-protocol-reference.md) · [Index](index.md) · [Next: Observability →](observability.md)

## Production install and start

Use the signed apt repository and an exact package version. The repository/key
bootstrap is an external release-owner procedure; this tree names the approved
repository but does not contain its signing-key URL or publish setup metadata.
Do not invent a key source. Once the approved source is configured, use:

Repository: `tailrocks/velnor-apt`.

```sh
sudo apt-get update
apt-cache policy velnor-runner
sudo install -d -m 0750 /run/velnor
sudo /usr/bin/flock --exclusive --nonblock --no-fork \
  /run/velnor/package-transaction.lock \
  apt-get install velnor-runner=X.Y.Z
```

This is the documented production path; do not install a copied binary, local
`.deb`, `dpkg -i`, or local build on a production host. A fresh package stays
disabled. `postinst` verifies package identity, creates required paths,
reloads systemd, and does not enable, restart, or activate the fleet
(`crates/velnor-runner/debian/postinst:174-205`,
`crates/velnor-runner/debian/postinst:237-247`).

Configure the default scope:

```sh
sudoedit /etc/velnor/velnor.env
sudoedit /etc/velnor/execution.toml
if sudo test ! -e /etc/velnor/secrets.env; then
  sudo install -m 0600 /dev/null /etc/velnor/secrets.env
else
  sudo chmod 0600 /etc/velnor/secrets.env
fi
sudoedit /etc/velnor/secrets.env

sudo systemctl enable --now velnor-guardian.service
sudo systemctl enable --now velnor-daemon.service
sudo systemctl enable --now velnor-doctor.timer
```

`secrets.env` contains the operator-owned token:

```text
GITHUB_TOKEN=...
```

`velnor.env` supplies `VELNOR_URL`, `VELNOR_NAME`, `VELNOR_LABELS`,
`VELNOR_SLOTS`, `VELNOR_WORK_DIR`, and the trust/backend policy. The shipped
defaults and secret split are in `crates/velnor-runner/debian/velnor.env:7-50`.
The default unit reads `/etc/velnor/velnor.env` plus optional
`/etc/velnor/secrets.env` and starts `/usr/bin/velnor-runner daemon`
(`crates/velnor-runner/debian/velnor-daemon.service:20-47`).

The token must authorize runner registration for the selected repository,
organization, or enterprise and any selected runner-group operation. The
repository does not verify GitHub token scopes, and `velnorctl auth check`
reports permissions as unproven; it does not validate the daemon token
(`crates/velnorctl/src/lib.rs:1218-1236`).

For another scope, create matching files and enable the template:

```sh
sudoedit /etc/velnor/NAME.env
sudoedit /etc/velnor/NAME.secrets.env
sudo chmod 0600 /etc/velnor/NAME.secrets.env
sudo systemctl enable --now velnor-daemon@NAME.service
sudo systemctl enable --now velnor-doctor@NAME.timer
```

The template reads those files and stores state below `/var/lib/velnor-NAME`
(`crates/velnor-runner/debian/velnor-daemon@.service:9-14`,
`crates/velnor-runner/debian/velnor-daemon@.service:28-43`).

After editing configuration, restart the exact daemon unit. Registration and
network/token failures in supervised daemon mode retry forever with backoff;
they are visible in `systemctl status`, not treated as a successful fleet
(`crates/velnor-runner/src/runner.rs:2049-2147`).

## Backend and admission rules

`/etc/velnor/execution.toml` selects exactly one backend:

```toml
[execution]
backend = "docker" # or "microvm"
```

There is no backend fallback (`crates/velnor-runner/debian/execution.toml:1-6`).

- `docker`: preflight checks Git, the Docker cgroup boundary, Buildx when
  required, the Docker socket when the selected backend uses it, job-image
  tools, script execution, and bind visibility. A missing required local
  `/var/run/docker.sock` fails closed (`crates/velnor-runner/src/preflight.rs:78-127`,
  `crates/velnor-runner/src/preflight.rs:355-363`).
- `microvm`: preflight checks the packaged Firecracker artifacts, `/dev/kvm`,
  isolation, and a synthetic guest probe. It does not require the host Docker
  socket; failure does not fall back to Docker
  (`crates/velnor-runner/src/preflight.rs:20-76`).

Strict capability admission is mandatory. `strict` is the only accepted value;
the removed `VELNOR_SKIP_CAPABILITY_VALIDATION` and
`VELNOR_DIAGNOSTIC_NODE_SIDECAR` variables make startup fail
(`crates/velnor-runner/src/args.rs:270-305`). Packaged units force the strict
value and lock mise resolution (`crates/velnor-runner/debian/velnor-daemon.service:24-36`).

Run the same preflight explicitly before a manual proof:

```sh
velnorctl preflight \
  --config-dir /etc/velnor \
  --work-dir /var/lib/velnor/work
```

If the Docker daemon sees the work directory at another path, add
`--docker-host-work-dir PATH`. The CLI derives backend and Docker/Buildx
requirements from `execution.toml` (`crates/velnorctl/src/runtime.rs:617-663`).

## Files, state, and sockets

Config directory resolution order is:

1. `--config-dir`
2. `VELNOR_CONFIG_DIR`
3. systemd `STATE_DIRECTORY` plus `runner`
4. `VELNOR_STORAGE_ROOT` plus `lib/velnor/runner`
5. XDG user state plus `velnor/runner`

The resolver never uses `$HOME/.velnor`; interactive state is under XDG state
(`crates/velnor-runner/src/config.rs:91-146`). Runner settings are in
`runner.json`; its directory is `0700` and file is atomically written with
`0600` permissions (`crates/velnor-runner/src/config.rs:171-196`).

When config is resolved implicitly, a named daemon uses this config base:

```text
<resolved-config>/daemons/<sanitized-name>/
```

With more than one slot, each slot is below `slots/slot-N`; with one slot, the
base itself is the slot directory (`crates/velnor-runner/src/runner.rs:3694-3731`,
`crates/velnor-runner/src/runner.rs:4173-4182`).

The durable control database defaults to `/var/lib/velnor/state.db`; pass
`--state-db` or set `VELNOR_STATE_DB` for an explicit path. Its parent must
already exist (`crates/velnor-control/src/store/mod.rs:38-40`,
`crates/velnor-control/src/store/mod.rs:95-110`,
`crates/velnorctl/src/runtime.rs:21-30`).

Each daemon instance exposes:

```text
/run/velnor/<instance>/control.sock  # reads, events, logs, telemetry
/run/velnor/<instance>/admin.sock    # lifecycle POSTs
```

The endpoint URI is `unix:///run/velnor/<instance>`. The client selects the
read socket for queries and the admin socket for mutations
(`crates/velnor-client/src/unix.rs:13-21`,
`crates/velnor-client/src/unix.rs:22-92`). Both sockets are `0660`; the read
group is `velnor`, the admin group is `velnor-admin`. Root, the socket owner,
or the matching group may pass peer authorization
(`crates/velnorctl/src/http.rs:141-150`,
`crates/velnorctl/src/http.rs:489-510`).

Use a named local context when desired:

```sh
velnorctl context set default \
  --endpoint unix:///run/velnor/default
velnorctl context use default
velnorctl -o wide get instances
```

Contexts persist at `$XDG_CONFIG_HOME/velnor/config.toml`, or
`$HOME/.config/velnor/config.toml`. Without a selected context, `--instance`
or `VELNOR_INSTANCE` chooses the instance; otherwise the fallback is
`default` (`crates/velnorctl/src/lib.rs:569-595`,
`crates/velnorctl/src/lib.rs:1167-1177`).

## Daily commands: NOW

Global flags apply before or after subcommands: `--context`, `--output`
(`table`, `wide`, `json`, `yaml`, `jsonl`, `name`), `--instance`, `--repo`,
selectors, `--since`, `--timeout`, `--no-color`, and repeatable `-v`. The
default output is a human table; `--timeout` is a real handler deadline
(`crates/velnorctl/src/lib.rs:84-143`, `crates/velnorctl/src/lib.rs:448-463`).

### Control-plane reads

These require a live, authorized control socket. The daemon performs API/schema
negotiation before each client operation, and read routes are `/v1/info`,
`/v1/<resource>`, `/v1/watch`, `/v1/logs/<subject>`, and `/v1/telemetry`
(`crates/velnorctl/src/http.rs:99-110`,
`crates/velnor-client/src/http.rs:150-240`).

```sh
velnorctl get hosts
velnorctl get instances --selector kind=Instance
velnorctl get runners --limit 100 --output wide
velnorctl describe runner/NAME --output yaml
velnorctl events --limit 100
velnorctl logs RUN_OR_JOB --tail 200
velnorctl telemetry --limit 100
velnorctl top host
velnorctl wait instance/default --for Ready --timeout 60
```

`get` supports `hosts`, `instances`, `slots`, `runners`, `jobs`, `runs`,
`queue`, `events`, `reservations`, and `leases`. Query filters are
`--selector`, `--field-selector`, `--page-token`, `--limit`, and `--since`
(`crates/velnorctl/src/commands.rs:11-80`). `top` maps to hosts, instances,
slots, jobs, or storage reads (`crates/velnorctl/src/commands.rs:122-136`,
`crates/velnorctl/src/lib.rs:762-774`). `wait` re-reads after event cursors and
reboots from a fresh snapshot when a bounded watch cursor expires
(`crates/velnorctl/src/lib.rs:776-831`).

### Workflow-run reads

```sh
velnorctl run list
velnorctl run view RUN_ID
velnorctl run watch RUN_ID
velnorctl run logs RUN_ID
```

These are current read projections. `run view` filters the returned `runs`
resources by numeric name; `run watch` reads the run event stream
(`crates/velnorctl/src/lib.rs:930-964`).

### Local runner operations

```sh
velnorctl status --config-dir /var/lib/velnor/runner/daemons/velnor
velnorctl status --config-dir /var/lib/velnor/runner/daemons/velnor \
  --check-target-mvp
velnorctl status --json --config-dir /etc/velnor --state-dir /run/velnor

velnorctl storage paths
VELNOR_STORAGE_ROOT=/var velnorctl storage status

VELNOR_STORAGE_ROOT=/var velnorctl cache du \
  --work-dir /var/lib/velnor/work
VELNOR_STORAGE_ROOT=/var velnorctl cache gc --dry-run \
  --work-dir /var/lib/velnor/work
```

`status` prints the stored GitHub scope, runner identity, labels, V2 state, and
credential presence. `--check-target-mvp` validates the required x64 target
labels and V2 fields. `--json` is the node health vector, not
`systemctl is-active` (`crates/velnor-runner/src/runner.rs:11804-11863`).

`storage paths` and `storage status` are current. Under `VELNOR_STORAGE_ROOT=/var`,
the canonical roots are `/var/cache/velnor/v1`, `/var/lib/velnor`,
`/run/velnor`, and `/var/log/velnor`; status accounts cache classes
(`crates/velnor-runner/src/storage.rs:10-43`,
`crates/velnor-runner/src/storage.rs:55-99`).

`cache du` is read-only. `cache gc --dry-run` only lists candidates. A deleting
GC requires `--yes`, takes exclusive coordination locks, and checks active
cache-scope leases. `--force-no-lease-check` deliberately bypasses that check
and should be treated as an emergency action
(`crates/velnor-runner/src/cache.rs:142-204`).

### Configure, remove, doctor, and canary

`configure` is current for a direct/local JIT setup or proof run:

The command below expects `GITHUB_TOKEN` from a protected environment or secret
manager. Never paste the token into a command line; shell history and process
inspection can expose it.

```sh
velnorctl configure \
  --url https://github.com/OWNER/REPO \
  --name velnor-proof \
  --labels velnor,ubuntu-24.04 \
  --config-dir /tmp/velnor-proof
```

It validates labels, calls GitHub's JIT endpoint, stores `runner.json`, and
persists the returned V2 runner settings and credentials. Organization and
enterprise scopes require `--pool-name` (or `VELNOR_POOL_NAME`); dry runs with
`--pool-name` also require `--pool-id`
(`crates/velnor-runner/src/runner.rs:1094-1149`,
`crates/velnor-runner/src/runner.rs:1229-1315`). The normal packaged daemon
performs preflight, capacity admission, and per-slot JIT configuration itself;
do not preconfigure those slots as a separate step
(`crates/velnor-runner/src/runner.rs:2150-2200`).

The direct doctor/remove commands use the same protected `GITHUB_TOKEN`
environment when remote GitHub access is needed.

```sh
velnorctl doctor \
  --url https://github.com/OWNER/REPO \
  --name velnor --slots 4

velnorctl remove --local-only \
  --config-dir /var/lib/velnor/runner/daemons/velnor --slots 4
velnorctl remove \
  --config-dir /var/lib/velnor/runner/daemons/velnor --slots 4

velnorctl canary --fixture --report /tmp/velnor-canary.json
```

`doctor` lists matching GitHub runners, reports healthy/registered/busy counts,
capacity, leases, cache accounting, timing SLOs, and fails when zero expected
runners are healthy. It warns when only part of the expected fleet is healthy
(`crates/velnor-runner/src/runner.rs:11532-11558`,
`crates/velnor-runner/src/runner.rs:11560-11756`). `remove` deletes the exact
stored remote runner only when a PAT is supplied; otherwise it skips remote
deletion and removes local config (`crates/velnor-runner/src/runner.rs:11282-11330`).
`canary --fixture` records the canary stages locally without calling GitHub;
the CLI also supports `--timeout-seconds` and `--report`
(`crates/velnorctl/src/lib.rs:274-301`).

## Daemon and service operation: NOW

Direct daemon invocation is current for local/proof use:

```sh
velnorctl daemon \
  --url https://github.com/OWNER/REPO \
  --name velnor-proof --slots 2 \
  --work-dir /var/lib/velnor-proof/work
```

The daemon binds both Unix sockets before starting its runner loop, opens the
shared control services, and cleans up socket paths on shutdown. `--name` is
also the API instance identity; absent a name, the API instance is `default`
(`crates/velnorctl/src/runtime.rs:89-164`). In supervised mode the pass order is
durable store → storage selection → backend preflight → capacity permits → slot
supervision; startup JIT registration occurs only after those checks
(`crates/velnor-runner/src/runner.rs:2150-2229`). `--once` is a bounded proof
mode; ordinary daemon mode keeps polling
(`crates/velnor-runner/src/args.rs:211-253`).

Normal packaged operation uses these units:

| Unit | Current role |
| --- | --- |
| `velnor-guardian.service` | Node-local supervision only; no GitHub token, Docker, or job execution. `crates/velnor-runner/debian/velnor-guardian.service:1-18` |
| `velnor-daemon.service` | Default-scope daemon; `Type=notify`, watchdog 180s, automatic restart, graceful stop timeout 10800s. `crates/velnor-runner/debian/velnor-daemon.service:9-19`, `crates/velnor-runner/debian/velnor-daemon.service:48-61` |
| `velnor-daemon@NAME.service` | One daemon per configured scope; same runner lifecycle with `/etc/velnor/NAME.env`. `crates/velnor-runner/debian/velnor-daemon@.service:8-20`, `crates/velnor-runner/debian/velnor-daemon@.service:44-57` |
| `velnor-doctor.timer` / `velnor-doctor@NAME.timer` | Every 10 minutes, persistent timer, randomized delay up to 60s. `crates/velnor-runner/debian/velnor-doctor.timer:1-10`, `crates/velnor-runner/debian/velnor-doctor@.timer:1-10` |
| `velnor-doctor.service` / `velnor-doctor@NAME.service` | One-shot GitHub fleet probe. `loaded inactive dead` after completion is normal. `crates/velnor-runner/debian/velnor-doctor.service:6-18`, `crates/velnor-runner/debian/velnor-doctor@.service:6-24` |

The controller, slot, and job units are executable machine hooks, not the
normal operator start path. The daemon starts the controller/slot lifecycle
itself; `velnor-runner` exposes those node roles for packaged supervision
(`crates/velnor-runner/src/service.rs:35-50`,
`crates/velnor-runner/src/runner.rs:2217-2248`).

## Release and package transaction behavior

Release hooks are current only on the interim service binary:

```sh
sudo velnor-runner release verify-installed
sudo velnor-runner release activate --record RELEASE_RECORD.json
sudo velnor-runner release rollback
sudo velnor-runner release export
```

`release verify-installed` runs automatically before both daemon units and
rejects a missing or mixed binary/package/manifest tuple
(`crates/velnor-runner/debian/velnor-daemon.service:37-47`,
`crates/velnor-runner/debian/velnor-daemon@.service:33-43`). The service parser
and dispatcher expose release hooks (`crates/velnor-runner/src/service.rs:35-68`,
`crates/velnor-runner/src/service.rs:578-600`). `velnorctl release` is **not**
currently in the human CLI command enum (`crates/velnorctl/src/lib.rs:192-278`);
do not document it as an available `velnorctl` command.

For upgrades, drain and stop the exact daemon/timer units first, then hold the
exclusive package lock for the entire apt transaction. Every shipped Velnor
service takes the same shared lock; maintainer scripts reject direct or
unlocked package configuration. The package does
not itself restart or re-enable units.

## Command status: RESERVED / not an actuator

### Lifecycle and reconciliation

The CLI accepts these shapes:

```sh
velnorctl cordon instance/NAME --reason TEXT --idempotency-key KEY
velnorctl uncordon instance/NAME --reason TEXT --idempotency-key KEY
velnorctl drain instance/NAME --reason TEXT --idempotency-key KEY
velnorctl resume instance/NAME --reason TEXT --idempotency-key KEY
velnorctl restart instance/NAME --reason TEXT --idempotency-key KEY
velnorctl recycle instance/NAME --reason TEXT --idempotency-key KEY
velnorctl scale instance/NAME --slots N --reason TEXT --idempotency-key KEY
velnorctl reconcile runners --dry-run
```

These are **RESERVED**. Lifecycle commands first negotiate admin info; because
the server advertises `mutations:false`, the client rejects them before posting
to the admin socket. A direct HTTP POST reaches the adapter’s `501` with
`operation.unsupported`. `reconcile` does not select the admin socket: it reads
control info and returns `reconcile.plan_unavailable` because no reviewed plan
is installed. No host or fleet state changes.
(`crates/velnorctl/src/lib.rs:834-928`,
`crates/velnor-client/src/http.rs:244-291`,
`crates/velnorctl/src/http.rs:868-907`)

### Workflow mutations and diagnostics

These parse but are **RESERVED** and return `run.operation_unavailable`:

```text
velnorctl run cancel RUN_ID
velnorctl run rerun RUN_ID
velnorctl run download RUN_ID
velnorctl run dispatch WORKFLOW --repo OWNER/REPO [--reference REF]
velnorctl run open RUN_ID
```

The implemented run leaves are only list/view/watch/logs
(`crates/velnorctl/src/lib.rs:930-974`). `velnorctl diagnostics bundle
--archive PATH` is also **RESERVED** and returns
`diagnostics.bundle_unavailable` (`crates/velnorctl/src/lib.rs:1005-1020`).

### Storage commands not live through `velnorctl`

`velnorctl storage paths` and `velnorctl storage status` are NOW. These parsed
commands are **RESERVED/UNAVAILABLE** through the current control CLI:

```text
velnorctl storage du
velnorctl storage gc ...
velnorctl storage history
velnorctl storage reservations
velnorctl storage leases
velnorctl storage explain-pressure
```

The top-level dispatcher deliberately returns
`storage requires a reachable v1 control API endpoint` for those leaves instead
of invoking the runtime implementation (`crates/velnorctl/src/lib.rs:525-538`).

### Planning/config/auth placeholders

- `config view` and `config validate` return a built-in-only resolver result;
  `config diff` returns `[]`; `config sources` returns a fixed source list.
  This is **NOW as output**, not a live host configuration authority
  (`crates/velnorctl/src/lib.rs:1133-1165`).
- `auth status` returns GitHub permissions as `Unproven`; `auth check` returns
  the same report and exits with a degraded condition. It does not validate the
  daemon token (`crates/velnorctl/src/lib.rs:1218-1236`).
- `instance init|install|apply|delete` emits an operation with
  `phase: "planned"`; it does not create, install, apply, or delete a host
  instance (`crates/velnorctl/src/lib.rs:1239-1266`).
- `workflow check --repo ... --reference ... --workflow ...` performs only
  basic field validation plus API reachability, then reports `valid: true`; it
  is not a full workflow compatibility proof
  (`crates/velnorctl/src/lib.rs:977-1003`).

## Capability and adapter inspection: NOW

These commands inspect the compiled local manifest without changing the host:

```sh
velnorctl capability list
velnorctl capability explain actions/checkout
velnorctl capability check --job-dump /path/to/sanitized-job.json
velnorctl capability export

velnorctl adapter list
velnorctl adapter describe actions/checkout@v4
velnorctl adapter check actions/checkout@v4

velnorctl capabilities export
velnorctl capabilities check --job-dump /path/to/sanitized-job.json
```

`capability` and `adapter` are local manifest lookups. `capabilities` is the
runtime compatibility surface with only `check` and `export`
(`crates/velnorctl/src/lib.rs:1022-1131`,
`crates/velnorctl/src/runtime.rs:445-470`). Unknown entries fail as unavailable;
they do not trigger a network lookup.

## Failure triage

1. Check the exact unit and its pre-start release proof:

   ```sh
   systemctl status velnor-daemon.service
   journalctl -u velnor-daemon.service -b --no-pager
   systemctl list-timers 'velnor-doctor*'
   ```

2. If the daemon reports token trouble, fix the matching `*.secrets.env`; the
   supervised loop will keep retrying. The daemon explicitly diagnoses absent
   or literal `${...}` token placeholders (`crates/velnor-runner/src/runner.rs:2055-2069`).

3. If `velnorctl` reports control API unavailable or authorization denied,
   verify `/run/velnor/<instance>/{control,admin}.sock`, the selected context,
   and membership in `velnor`/`velnor-admin`. The client enforces a ten-second
   per-request transport deadline and only accepts the canonical `v1` Unix API
   (`crates/velnor-client/src/http.rs:127-155`,
   `crates/velnor-client/src/http.rs:306-335`).

4. If startup fails before GitHub registration, run `velnorctl preflight` with
   the same config/work paths. Backend selection errors, missing Docker/Buildx,
   missing `/dev/kvm`, and failed bind visibility are intended fail-closed
   results (`crates/velnor-runner/src/preflight.rs:19-35`,
   `crates/velnor-runner/src/preflight.rs:36-127`).

5. Do not interpret a completed doctor oneshot as a dead daemon. Inspect its
   timer and failed-unit state; the one-shot service is expected to become
   inactive after its probe (`crates/velnor-runner/debian/velnor-doctor.service:6-18`).
