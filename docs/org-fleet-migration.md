# Organization fleet migration

Velnor uses GitHub's current organization-scoped JIT configuration endpoint so
one same-trust fleet can serve multiple repositories without changing workflow
labels. Complete the storage capacity and GC gates before migrating production
organizations.

## Target pools

Create restricted runner groups with repository allowlists and the persistent
`velnor-target-mvp` label:

| Organization | Initial slots |
|---|---:|
| ChainArgos | 10 general + 4 burst |
| jackin-project | 6–10 |
| tailrocks | 8–12 |

Keep trusted repositories and public-fork workloads in separate groups,
daemons, labels, and `VELNOR_TRUST_SCOPE` values. Never let an untrusted pool
mount trusted stores or the host Docker socket.

## Migration

1. Confirm every repository is granted access to its group and record the group
   name. Keep the existing `velnor-target-mvp` label throughout the migration.
2. Cancel queued/in-progress verification attempts, then send SIGTERM to each
   per-repository daemon. Wait for graceful drain and confirm no busy slots.
3. Delete only the stopped fleet's stale/offline registrations. Configure the
   replacement daemon with `--url https://github.com/<org> --pool-name <group>`
   and the same labels. Velnor resolves the name to GitHub's numeric group id.
4. Start the organization daemon and run `velnor-runner doctor` against the
   organization URL. Dispatch the fixture or repository smoke only after every
   expected slot is online.
5. Repeat for the next organization after the first fleet has remained healthy
   and a second run confirms warm-store reuse.

## Tailrocks access repair checklist

Current evidence (2026-07-21): the `tailrocks` organization has zero registered
runners. Its `Default` runner group has `visibility=all`, so repository access is
not the blocker. Five healthy `velnor-dogfood-slot-*` registrations instead live
under the `tailrocks/velnor` repository. The daemon must be drained and migrated
from repository scope to organization scope before estate smoke dispatches.

The authenticated operator token now carries `admin:org`, `repo`, and
`workflow`; no further GitHub scope expansion is required for this migration.

1. Find the trusted group id and confirm its visibility is `selected`:

   ```sh
   gh api -H 'X-GitHub-Api-Version: 2026-03-10' \
     orgs/tailrocks/actions/runner-groups \
     --jq '.runner_groups[] | [.id, .name, .visibility] | @tsv'
   ```

2. Set `trusted_group_id` to that numeric id, then add every tailrocks estate
   repository. These repository ids are stable GitHub ids:

   ```sh
   trusted_group_id=<TRUSTED_GROUP_ID>
   for repository_id in \
     1255367013 1235761953 1277301638 1301508644 1262209244 \
     1265722009 1302045151 1168023899 1247026498 1247026496 \
     1256201624
   do
     gh api --method PUT -H 'X-GitHub-Api-Version: 2026-03-10' \
       "orgs/tailrocks/actions/runner-groups/${trusted_group_id}/repositories/${repository_id}" \
       --silent
   done
   ```

   The ids map respectively to `velnor`, `parallax`,
   `parallax-telemetry-playground`, `tablerock`, `holla`, `ruxel`, `termrock`,
   `schemalane`, `pg-bigdecimal`, `tracing-request-level`, and
   `velnor-actions-fixture`.

3. Verify the allowlist before dispatching anything:

   ```sh
   gh api --paginate -H 'X-GitHub-Api-Version: 2026-03-10' \
     "orgs/tailrocks/actions/runner-groups/${trusted_group_id}/repositories" \
     --jq '.repositories[].full_name'
   ```

4. Cancel every older active verification run. Dispatch one `lanes=both` run
   per repository, monitor only its returned id, and require a non-empty runner
   and group assignment within two minutes. Then run `velnor-runner doctor`
   and the warm rerun proof before declaring migration complete.

## Rollback

Drain the organization daemon, remove only its registrations, and restart the
unchanged per-repository units. Because workflow labels remain constant, no YAML
rollback is needed. Do not run both fleet shapes with the same runner names.
