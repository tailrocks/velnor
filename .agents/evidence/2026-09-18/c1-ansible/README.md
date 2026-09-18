[ChainArgos monorepo](../README.md) › Ansible configs

# Ansible configs

Ansible entrypoint for the ChainArgos dedicated servers (all hosted at Hetzner). Read
this to find out which playbook owns which host, how to run one, and where the manual
upgrade runbooks live. Secrets resolve at run time through each playbook's documented
secret provider.

## On this page

- [Install dependencies](#install-dependencies)
- [Host inventory](#host-inventory)
- [Playbooks](#playbooks)
- [Running playbooks](#running-playbooks)
- [Runbooks](#runbooks)
- [Sentry and Velnor](#sentry-and-velnor)

## Install dependencies

```bash
ansible-galaxy collection install -r requirements.yaml
```

Most playbooks additionally require the 1Password CLI (`op`) for secret lookups.
The repository pins `fnox` for local, non-interactive controller authentication. Use
the wrapper below so `OP_SERVICE_ACCOUNT_TOKEN` is injected only into the child
process; do not export it or use `fnox get` in a shell.

### Controller secret bootstrap

The committed [`fnox.toml`](../fnox.toml) contains the project age recipient and
the ChainArgos 1Password provider. The age identity is local-only at
`~/.config/fnox/age.txt`; `fnox.local.toml` is gitignored. On a new controller:

```sh
mise install
mkdir -p ~/.config/fnox
age-keygen -o ~/.config/fnox/age.txt
chmod 600 ~/.config/fnox/age.txt
scripts/fnox-bootstrap.sh
scripts/with-fnox.sh op whoami
```

The bootstrap command prompts for the service-account token without putting it
in shell history or process arguments. Existing Ansible lookups remain the
source of truth for all runtime credentials, including the canonical
`ChainArgos lightdash_dbt@titan PostgreSQL` item and its `password` field.

Run authenticated playbooks as follows:

```sh
scripts/with-fnox.sh rtk ansible-playbook -i hosts.ini --limit delorean <playbook>.yml
```

Do not run `fnox get`, `fnox export`, `export OP_SERVICE_ACCOUNT_TOKEN=...`,
Ansible `--diff`, or pass any secret through `-e`.

### Local Lightdash CSV delivery deployment credentials

The scoped `deploy-lightdash-csv-delivery.yml` playbook uses the encrypted
local bundle at
`~/.config/chainargos/lightdash-csv-delivery/credentials.json.age`.
The age identity is `~/.config/fnox/age.txt`, mode `600`; the encrypted bundle is
never checked into the repository or decrypted to a regular file. From the repository
root, run it through
`scripts/deploy-lightdash-csv-delivery-local.sh`; that script validates the
exact required JSON keys, streams the decrypted bundle through an ephemeral
FIFO, and invokes only this scoped playbook. It does not call `op` or `fnox`.
It rejects extra-var files and credential-bearing extra vars; release controls
must be passed inline without secret values.

The bundle contains only the Titan read-only connection, internal RustFS,
partner S3, delivery API bearer-token, and Docker registry inputs needed by the
delivery service. It has no verifier, Looker API, or second evidence-store
credential set.
Override the bundle or age-identity paths with `LIGHTDASH_CSV_CREDENTIALS_FILE`
or `LIGHTDASH_CSV_AGE_IDENTITY_FILE`, or the script's corresponding options.

### GitHub Actions

GitHub Actions does not use fnox or the local age identity. A trusted job that
needs 1Password access supplies the protected GitHub secret directly to its
process environment:

```yaml
env:
  OP_SERVICE_ACCOUNT_TOKEN: ${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}
```

Do not sync `fnox.local.toml`, install an age key, or invoke fnox in CI.

## Host inventory

Hosts are declared in `hosts.ini` under the `nodes` group (`ansible_ssh_user=root`).

| Host | Role | Setup playbook |
| --- | --- | --- |
| `pegasus` | Blockchain nodes + Kestra backups | `setup-pegasus.yml` |
| `delorean` | Docker apps from `backend/` and `backend-rust/`, backup storage | `setup-delorean.yml` |
| `titan` | PostgreSQL source for CSV delivery | `setup-titan.yml` |
| `postgresql-nova` | PostgreSQL (production blockchain data) | `setup-postgresql-nova.yml` |
| `clickhouse-selene` | ClickHouse analytics (two-tier NVMe) | `setup-clickhouse-selene.yml` |
| `sentry` | Self-hosted Sentry + Velnor CI runners | `setup-sentry.yml` |

Per-host detail — drives, databases, users, what each setup playbook deploys — lived in this
directory's `AGENTS.md`, removed by `2daa0d96` when repository instructions were consolidated
into the root [`AGENTS.md`](../AGENTS.md).

## Playbooks

| Kind | Playbooks | Purpose |
| --- | --- | --- |
| Common | `install-base.yml`, `install-docker.yml`, `update-packages.yml`, `upgrade-debian.yml` | Base packages and toolchains, Docker CE, apt/mise updates, Debian release sources. |
| Drive init | `init-pegasus-drives.yml`, `init-titan-drives.yml`, `init-postgresql-nova-drives.yml`, `init-clickhouse-selene-drives.yml`, `init-delorean-drives.yml` | LVM volume groups and logical volumes; run once at provisioning or when adding a disk. |
| Server setup | `setup-pegasus.yml`, `setup-delorean.yml`, `setup-titan.yml`, `setup-postgresql-nova.yml`, `setup-clickhouse-selene.yml`, `setup-sentry.yml` | Server-specific software, SSH keys, mounts, repos, env files. |
| Scoped deployment | `deploy-lightdash-app.yml`, `deploy-lightdash-csv-delivery.yml`, `deploy-lightdash-dbt.yml`, `deploy-lightdash-nginx.yml`, `deploy-processor-verify.yml` | Pins and deploys the declarative Lightdash app, CSV delivery runtime, nginx proxy, or the processor migration verification stack on delorean. |
| Other | `restart-blockchain-nodes.yml` | Restarts all blockchain nodes on pegasus using `containerctl.main.kts`. |

## Running playbooks

```sh
# Target a specific server
ansible-playbook -i hosts.ini --limit pegasus setup-pegasus.yml

# Target all servers
ansible-playbook -i hosts.ini install-base.yml

# Dry run
ansible-playbook -i hosts.ini --limit <host> <playbook>.yml --check --diff

# Apply only the Ansible-owned PostgreSQL config/reload/report tasks on Titan.
# This path needs no 1Password-resolved account values.
ansible-playbook -i hosts.ini --limit titan setup-titan.yml --tags postgresql_config
```

### Scoped Lightdash CSV delivery deployment

Use the dedicated playbook with an immutable release revision and registry digests. It changes
only the Lightdash CSV checkout pin, environment/profile/CA files, and the scoped
`lightdash-csv-rustfs` and `lightdash-csv-delivery` services. Do not pass `--diff`: credential-bearing templates
suppress diffs, and the play reports only sanitized paths and SHA-256 hashes.

If only the persisted base Compose image pin drifted, converge that one non-secret line without
reading credentials or restarting any service:

```sh
ansible-playbook -i hosts.ini --limit delorean converge-lightdash-csv-image-pin.yml \
  -e lightdash_csv_delivery_image_digest=sha256:<64-hex-digest>
```

```sh
# Local encrypted-credentials dry-run (run from the repository root)
scripts/deploy-lightdash-csv-delivery-local.sh --check \
  -e lightdash_csv_delivery_release_revision=<40-hex-revision> \
  -e lightdash_csv_delivery_image_digest=sha256:<64-hex-digest>

# Local encrypted-credentials apply
scripts/deploy-lightdash-csv-delivery-local.sh \
  -e lightdash_csv_delivery_release_revision=<40-hex-revision> \
  -e lightdash_csv_delivery_image_digest=sha256:<64-hex-digest>
```

The dry-run validates inputs, encrypted-bundle completeness, target safety, and the current
Compose contract; Ansible reports pending file changes, but check mode does not install those
candidate files or prove their resulting runtime behavior. Remove `--check` only after reviewing
that plan. Local mode resolves only this stack's required fields from the encrypted bundle,
passes the registry password through `docker login --password-stdin`, and removes its temporary
Docker authentication directory even on failure.

### Scoped processor migration verification deployment

Use `deploy-processor-verify.yml` only from the checked-out merged revision that
will run. It clones that exact revision into a revision-qualified release
directory, renders the secret environment file, validates the 16-service
Compose surface and digest-pinned image allowlist, and stops/removes only the
16 `chainargos-verify-*` containers, including both monitor identities. It
leaves the staged runtime stopped. It does not reset PostgreSQL, RabbitMQ,
Redis, or compare reports.

Run the controller checks first:

```sh
ansible-playbook ansible-configs/deploy-processor-verify.yml --syntax-check
scripts/with-fnox.sh rtk ansible-playbook -i ansible-configs/hosts.ini \
  --limit delorean ansible-configs/deploy-processor-verify.yml --check \
  -e processor_verify_release_revision=<40-hex-main-revision>
```

Check mode validates that the requested revision is exactly the current
`refs/heads/main` SHA, plus credential presence and the path boundary. It
intentionally skips remote Compose/image/runtime checks. The apply run performs
those checks before removing any fixed allowlist container and leaves no verify
container running.

Apply only after reviewing the sanitized check result:

```sh
scripts/with-fnox.sh rtk ansible-playbook -i ansible-configs/hosts.ini \
  --limit delorean ansible-configs/deploy-processor-verify.yml \
  -e processor_verify_release_revision=<40-hex-main-revision>
```

Do not pass secrets, use `--diff`, or run the broad `setup-delorean.yml` path
for this stack. The release root is
`/projects/java-monorepo-processor-verify-<main-revision>`.

After the staged apply, perform the clean-room sequence in this order:

1. Run `reset.sh --plan`, review its exact eight database targets, then rerun
   it with `--confirm-clean-room`.
2. Purge only the eight `verify-*` RabbitMQ vhosts, flush only
   `chainargos-verify-redis`, and remove only the four compare report paths.
3. Start only RabbitMQ and Redis with `start.sh --mode infra`.
4. Run migrations from the release source checkout.
5. Start the full fixed 16-service surface with `start.sh --mode full`.

The release's rendered env file is the only env input for these helpers; no
processor or compare service may start before step 4 completes.

Run migrations from the exact source checkout, never from the dirty runtime
checkout:

```sh
ssh delorean 'cd /projects/java-monorepo-processor-verify-<main-revision>/source && \
  scripts/processor-verify/migrate.sh \
  --env-file ../env/production/processor-verify.env'
```

### Scoped Lightdash nginx deployment

Use the nginx playbook after replacing Lightdash or when the proxy has retained a retired
container address. It updates only the Lightdash proxy template, recreates only nginx, and
verifies public health plus protected legacy processor identity.

```sh
ansible-playbook -i hosts.ini --limit delorean deploy-lightdash-nginx.yml
```

### Scoped Lightdash dbt deployment

The direct dbt Fusion runtime has its own playbook. It requires an exact release commit and
uses only the canonical `ChainArgos lightdash_dbt@titan PostgreSQL` 1Password item. It installs
the protected environment and systemd units, verifies the immutable Fusion image, and leaves
the timer disabled unless explicitly enabled.

```sh
ansible-playbook -i hosts.ini deploy-lightdash-dbt.yml --check \
  -e lightdash_dbt_release_revision=<40-hex-release-commit>
```

Remove `--check` only after reviewing the plan. This playbook does not run the full all-token
dbt service by default; the service itself performs the Titan free-space gate and sequential
build supervision. To deploy the approved zero-preserving YOLO views and their chart
definitions from the same checked-out release, explicitly enable the bounded content path:

```sh
ansible-playbook -i hosts.ini --limit delorean deploy-lightdash-dbt.yml \
  -e lightdash_dbt_release_revision=<40-hex-release-commit> \
  -e lightdash_dbt_content_sync_enabled=true
```

That path runs `scripts/lightdash-dbt-build.sh build-parity` (views only, no watermark update)
before upserting the approved chart YAML files through the Lightdash content-as-code API. It
resolves the migration PAT from 1Password, uses TLS verification, verifies that the required
target explores were deployed from the same Fusion-compiled release, and is disabled by
default. Set `lightdash_dbt_content_sync_build_parity_enabled=false` only when the guarded
parity relations were already built and verified for the exact release; this reuses those
relations and skips the repeat build before chart upsert. Run `setup-titan.yml` after any SSH-key rotation; it owns the restricted
Delorean-to-Titan `df -Pk /var/lib/postgresql` key required by the non-interactive storage
guard.

## Runbooks

Manual operator procedures live in [`docs/`](docs/README.md):

| Runbook | What it covers |
| --- | --- |
| [docs/upgrade-debian.md](docs/upgrade-debian.md) | Debian release upgrade for one host. |
| [docs/upgrade-postgresql.md](docs/upgrade-postgresql.md) | In-place PostgreSQL major upgrade (17 -> 18). |

## Sentry and Velnor

Velnor on sentry is installed only through Debian apt (`velnor-runner` from
`https://velnor-apt.tailrocks.com`). Do not cargo-install binaries and do not
`docker commit` over the job image. The packaged `/usr/bin/velnor-workflow` is
the workflow CLI; jobs bind-mount it into the container.

Full setup or update:

```bash
ANSIBLE_LOCAL_TEMP=/private/tmp/ansible-local \
  ansible-playbook -i hosts.ini --limit sentry setup-sentry.yml
```

Sentry-only maintenance:

```bash
ANSIBLE_LOCAL_TEMP=/private/tmp/ansible-local \
  ansible-playbook -i hosts.ini --limit sentry --tags sentry setup-sentry.yml
```

Velnor-only maintenance:

```bash
ANSIBLE_LOCAL_TEMP=/private/tmp/ansible-local \
  ansible-playbook -i hosts.ini --limit sentry --tags velnor setup-sentry.yml
```

Remove any leftover GARM artifacts:

```bash
ANSIBLE_LOCAL_TEMP=/private/tmp/ansible-local \
  ansible-playbook -i hosts.ini --limit sentry --tags garm-remove setup-sentry.yml
```

What the playbook manages:

- Sentry self-hosted config and compose lifecycle.
- Three Velnor self-hosted GitHub Actions runner daemons (all label `velnor-target-mvp`; the ChainArgos daemon also receives the pinned external-workflow label `velnor-trusted`):
  - `velnor-daemon` — 4 slots, `ChainArgos/java-monorepo` (each job may use 4 CPUs, so this reserves host capacity for Docker/Testcontainers and the other scopes)
  - `velnor-daemon-fixture` — 2 slots, `donbeave/velnor-actions-fixture`
  - `velnor-daemon-blockchain-nodes` — 4 slots, `ChainArgos/blockchain-nodes`
  - Velnor PAT files are injected into the RAM-backed `/run/chainargos-secrets` tmpfs; rerun the playbook after reboot or credential rotation.
- GARM removal tasks (idempotent; safe to run on a clean server).

Repository-local workflow jobs should target:

```yaml
runs-on: ["self-hosted", "velnor-target-mvp"]
```

The pinned ChainArgos reusable workflow uses `velnor-trusted`; Ansible manages
that additional label on the ChainArgos daemon so both workflow contracts select
the same Sentry fleet.

### Required 1Password items

Vault: `ChainArgos`

Item: `ChainArgos Velnor`

Required fields:

- `GitHub.PAT` — token with `repo` + `workflow` scope on `ChainArgos/java-monorepo`
- `GitHub.Fixture PAT` — token with `repo` scope on `donbeave/velnor-actions-fixture`

### Manual Sentry bootstrap

The Sentry installer still has one manual confirmation path when a new Sentry version has not been bootstrapped yet. If the playbook pauses, follow the prompt on the `sentry` server and continue the playbook after the Sentry installer reports completion.

The marker file `/root/self-hosted/.ansible-bootstrap-complete` records the bootstrapped Sentry version so later runs skip that prompt for the same version.
`deploy-lightdash-csv-delivery.yml` deploys the Lightdash CSV
delivery service from an exact release revision and immutable delivery image
digest. It starts only the internal RustFS and delivery services; partner S3
fan-out uses the configured destination credentials.

The playbook always installs exactly two readable shadow destinations: internal
RustFS and the temporary partner S3 location. A missing partner read-capable
grant is a failed acceptance precondition, not a one-destination deployment
mode.
