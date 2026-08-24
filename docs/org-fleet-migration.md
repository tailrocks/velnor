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

1. With explicit operator approval, create the currently missing restricted
   group and its complete allowlist in one request. The proposed exact name is
   `velnor-trusted`; all listed repositories are public, so GitHub requires
   `allows_public_repositories=true`. Selection remains repository-scoped and
   fork execution remains governed by the separate untrusted-pool rule above.

   ```sh
   args=()
   for repository in \
     $(jq -r '.selected_repositories[]' fleet/policies/tailrocks-desired-policy.json)
   do
     args+=(-F "selected_repository_ids[]=$(
       gh api -H 'X-GitHub-Api-Version: 2026-03-10' "repos/${repository}" --jq .id)")
   done
   gh api --method POST -H 'X-GitHub-Api-Version: 2026-03-10' \
     orgs/tailrocks/actions/runner-groups \
     -f name='velnor-trusted' \
     -f visibility='selected' \
     -F allows_public_repositories=true \
     "${args[@]}" \
     --jq '[.id, .name, .visibility, .allows_public_repositories] | @tsv'
   ```

   No repository id is inlined here; each id is resolved at run time from its
   full name in the `selected_repositories` field of the generated
   `tailrocks-desired-policy.json`.

   [GitHub documents](https://docs.github.com/en/rest/actions/self-hosted-runner-groups?apiVersion=2026-03-10#create-a-self-hosted-runner-group-for-an-organization)
   `POST /orgs/{org}/actions/runner-groups` for this operation; classic
   OAuth/PAT authentication requires `admin:org`, which the current
   authenticated identity now has. Do not run this command before approval.

2. Find the trusted group id and confirm its visibility is `selected`:

   ```sh
   gh api -H 'X-GitHub-Api-Version: 2026-03-10' \
     orgs/tailrocks/actions/runner-groups \
     --jq '.runner_groups[] | [.id, .name, .visibility] | @tsv'
   ```

3. Set `trusted_group_id` to that numeric id, then add every tailrocks estate
   repository listed in the generated policy. Derive each numeric id at run
   time from its full name in `fleet/policies/tailrocks-desired-policy.json`:

   ```sh
   trusted_group_id=<TRUSTED_GROUP_ID>
   for repository in \
     $(jq -r '.selected_repositories[]' fleet/policies/tailrocks-desired-policy.json)
   do
     repository_id=$(gh api -H 'X-GitHub-Api-Version: 2026-03-10' \
       "repos/${repository}" --jq .id)
     gh api --method PUT -H 'X-GitHub-Api-Version: 2026-03-10' \
       "orgs/tailrocks/actions/runner-groups/${trusted_group_id}/repositories/${repository_id}" \
       --silent
   done
   ```

   The allowlist is owned by the generated policy, never by this page:
   `fleet/release-refs.toml` is the release-ref ledger, `mise run
   fleet-generate` regenerates the per-org policy JSONs under
   `fleet/policies/`, `mise run fleet-digests` prints their digests, and the
   audit-ci rule `fleet-policy-current` fails when committed policy bytes are
   stale. If membership changes, regenerate and commit the policy instead of
   editing repository ids here.

4. Verify the allowlist before dispatching anything:

   ```sh
   gh api --paginate -H 'X-GitHub-Api-Version: 2026-03-10' \
     "orgs/tailrocks/actions/runner-groups/${trusted_group_id}/repositories" \
     --jq '.repositories[].full_name'
   ```

5. Cancel every older active verification run. Dispatch one `lane=both` run
   per repository, monitor only its returned id, and require a non-empty runner
   and group assignment within two minutes. Then run `velnor-runner doctor`
   and the warm rerun proof before declaring migration complete.

## Allowlist drift incident (2026-08-24)

The `velnor-trusted` group (id 3) silently dropped to 3 allowlisted
repositories (`cloudflare-tofu`, `holla`, `velnor`) while the estate had grown
to 21 velnor-lane repositories. All 8 runners stayed online and idle, but every
velnor-lane job in an unlisted repository queued indefinitely (e.g.
tailrocks/velnor-actions run 32678489660, tailrocks/holla-apt run 32678491924).
The org audit log was not readable with the operator token, so the actor behind
the removal is unrecorded.

Membership was restored by re-adding every repository whose default-branch
workflows reference `velnor-target-mvp`. Standing rule: **every repository
onboarded to the Velnor lane must be added to the group allowlist in the same
change**, and `scripts/runner_group_doctor.sh` must be run after onboarding
batches — it fails loudly listing the exact remediation `PUT` per missing
repository:

```sh
scripts/runner_group_doctor.sh            # defaults: --org tailrocks --group velnor-trusted
```

The authoritative allowlist is the `selected_repositories` field of
`fleet/policies/tailrocks-desired-policy.json` — generated from
`fleet/release-refs.toml`, digest-reported by `mise run fleet-digests`, and
byte-compared against the ledger by the audit-ci `fleet-policy-current` rule.
Do not mirror it as a hand-maintained table here; inspect the generated policy
and live state directly:

```sh
jq -r '.selected_repositories[]' fleet/policies/tailrocks-desired-policy.json
scripts/runner_group_doctor.sh            # defaults: --org tailrocks --group velnor-trusted
```

## Rollback

Drain the organization daemon, remove only its registrations, and restart the
unchanged per-repository units. Because workflow labels remain constant, no YAML
rollback is needed. Do not run both fleet shapes with the same runner names.
