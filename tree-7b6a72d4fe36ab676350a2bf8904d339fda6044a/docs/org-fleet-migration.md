# Organization fleet migration

Velnor uses GitHub's current organization-scoped JIT configuration endpoint so
one same-trust fleet can serve multiple repositories without changing workflow
labels. The reviewed generated fleet policy is the active authority for group,
repository, workflow, and ref changes. Complete the storage capacity and GC
gates before migrating production organizations.

## Target pools

Reconcile restricted runner groups through the reviewed generated fleet policy,
with repository allowlists and the persistent `velnor-target-mvp` label:

| Organization | Initial slots |
|---|---:|
| ChainArgos | 10 general + 4 burst |
| jackin-project | 6–10 |
| tailrocks | 8–12 |

Keep trusted repositories and public-fork workloads in separate groups,
daemons, labels, and `VELNOR_TRUST_SCOPE` values. Never let an untrusted pool
mount trusted stores or the host Docker socket.

## Migration

For each organization, use the reviewed `velnor-tools fleet-policy` boundary
sequentially:

1. Regenerate and validate the deterministic policy from the approved ledger:
   `rtk mise run fleet-generate`. Do not edit repository ids or workflow entries
   by hand.
2. Produce the read-only desired/observed diff and digest:

   ```sh
   rtk cargo run -p velnor-tools --locked -- fleet-policy plan \
     --policy fleet/policies/<org>-desired-policy.json \
     --ledger fleet/release-refs.toml
   ```

3. Save a sanitized pre-change capture, review the exact repository,
   workflow/ref, group, and guard diff, and record the plan digest. A changed
   digest requires a new review. Stop if the ledger is absent, the group is
   missing/default/inherited/read-only, closure is ambiguous, or a removal has
   no reviewed closure evidence.
4. Route new verification to GitHub-hosted, cancel older verification runs,
   drain the Velnor daemon, and prove that no slot is busy. Delete only stale
   or offline registrations owned by validation.
5. After explicit approval of that exact digest, apply only the named
   organization. `apply` writes workflow restrictions first, replaces the
   exact repository set, and requires readback equality:

   ```sh
   rtk cargo run -p velnor-tools --locked -- fleet-policy apply \
     --policy fleet/policies/<org>-desired-policy.json \
     --ledger fleet/release-refs.toml \
     --organization <org> \
     --plan-digest <REVIEWED_PLAN_DIGEST>
   ```

6. Run the read-only audit and require a clean result before resuming the
   organization:

   ```sh
   rtk cargo run -p velnor-tools --locked -- fleet-policy audit \
     --policy fleet/policies/<org>-desired-policy.json \
     --ledger fleet/release-refs.toml \
     --organization <org>
   ```

7. Configure the organization daemon with the stable group name, run
   `velnor-runner doctor`, and dispatch smoke only after the pending
   runner-registration/assignment and full guard-state acceptance evidence is
   captured. Require a non-empty runner and group assignment within two
   minutes, then complete the warm rerun proof before touching the next
   organization.

Do not use direct GitHub group mutations as a parallel path. If any apply,
readback, routing, capacity, storage, or GC gate fails, keep the organization
drained, use the explicit GitHub lane, and preserve the STOP conditions.

## Tailrocks access repair checklist

Historical evidence (2026-07-21; superseded for active decisions): the
`tailrocks` organization had zero registered runners. Its `Default` runner group
had `visibility=all`, so repository access was not the blocker. Five healthy
`velnor-dogfood-slot-*` registrations instead lived under the
`tailrocks/velnor` repository. Do not use this snapshot as current
runner-state evidence.

Plan 039's fresh snapshot is limited to runner-group policy fields. Current
runner registration/assignment state and full guard-state acceptance remain
pending evidence; neither is implied by a group existing or a daemon being
active. The daemon must be drained and migrated from repository scope to
organization scope before estate smoke dispatches.

The authenticated operator token now carries `admin:org`, `repo`, and
`workflow`; no further GitHub scope expansion is required for this migration.

### Historical direct REST examples — non-executable

> Historical/non-executable. These snippets document the superseded manual
> create-group and per-repository `PUT` path. Do not copy or run them. Active
> reconciliation is only through reviewed `fleet-policy plan`, `apply`, and
> `audit`; the generated policy owns the exact repository and workflow/ref
> boundary.

The 2026-08-24 read-only snapshot found all three `velnor-trusted` groups
present, but workflow restrictions were not enabled. The old create-group
procedure below is retained only as historical context; it is not a remedy.

   ```sh
   # HISTORICAL / NON-EXECUTABLE — do not run.
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
   `POST /orgs/{org}/actions/runner-groups` for this historical operation. The
   endpoint reference does not authorize this snippet; do not run it.

The following direct lookup and per-repository `PUT` are historical/non-
executable as well:

   ```sh
   # HISTORICAL / NON-EXECUTABLE — do not run.
   gh api -H 'X-GitHub-Api-Version: 2026-03-10' \
     orgs/tailrocks/actions/runner-groups \
     --jq '.runner_groups[] | [.id, .name, .visibility] | @tsv'
   ```

   ```sh
   # HISTORICAL / NON-EXECUTABLE — do not run.
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
`fleet/release-refs.toml` is the release-ref ledger, `rtk mise run
fleet-generate` regenerates the per-org policy JSONs under `fleet/policies/`,
`rtk mise run fleet-digests` prints their digests, and the audit-ci rule
`fleet-policy-current` fails when committed policy bytes are stale.

### Active Tailrocks procedure

1. Run `fleet-policy plan` with the generated Tailrocks policy and the release-
   ref ledger; save the sanitized diff and digest. The plan is read-only.
2. Review the exact diff. If the desired repository set removes any live
   selection, stop until reviewed closure evidence exists. If the group is
   missing, Default, inherited, or workflow restrictions are read-only, stop
   and escalate; do not create or mutate it from this page.
3. Route verification to GitHub-hosted, drain Velnor, and prove no busy slot.
   With explicit approval of the unchanged digest, run `fleet-policy apply` for
   `tailrocks` only. It applies the complete workflow restriction, replaces the
   exact generated repository set, and performs final readback.
4. Run `fleet-policy audit --policy fleet/policies/tailrocks-desired-policy.json
   --ledger fleet/release-refs.toml --organization tailrocks`; resume only
   after exact semantic equality. Then start the organization daemon with `--pool-name
   velnor-trusted`, run `velnor-runner doctor`, and collect the pending runner-
   state, full guard-state, routing/denial, and warm-run acceptance evidence.
5. Cancel every older active verification run before smoke. Dispatch one
   `lane=both` run, monitor only its returned id, and require a non-empty runner
   and group assignment within two minutes. Never declare migration complete
   from group readback or daemon health alone.

For reference only, the old direct allowlist lookup was:

   ```sh
   # HISTORICAL / NON-EXECUTABLE — do not run.
   gh api --paginate -H 'X-GitHub-Api-Version: 2026-03-10' \
     "orgs/tailrocks/actions/runner-groups/${trusted_group_id}/repositories" \
     --jq '.repositories[].full_name'
   ```

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
onboarded to the Velnor lane must enter the generated policy in the same
change**. Use the reviewed, digest-gated `velnor-tools fleet-policy` plan,
apply, and audit flow for onboarding batches and drift checks. Apply requires
explicit approval of the unchanged plan digest:

```sh
rtk mise run fleet-generate
rtk cargo run -p velnor-tools --locked -- fleet-policy plan \
  --policy fleet/policies/tailrocks-desired-policy.json \
  --ledger fleet/release-refs.toml
rtk cargo run -p velnor-tools --locked -- fleet-policy apply \
  --policy fleet/policies/tailrocks-desired-policy.json \
  --ledger fleet/release-refs.toml \
  --organization tailrocks \
  --plan-digest <REVIEWED_PLAN_DIGEST>
rtk cargo run -p velnor-tools --locked -- fleet-policy audit \
  --policy fleet/policies/tailrocks-desired-policy.json \
  --organization tailrocks
```

The authoritative allowlist is the `selected_repositories` field of
`fleet/policies/tailrocks-desired-policy.json` — generated from
`fleet/release-refs.toml`, digest-reported by `rtk mise run fleet-digests`, and
byte-compared against the ledger by the audit-ci `fleet-policy-current` rule.
Do not mirror it as a hand-maintained table here; obtain live policy state
through `fleet-policy plan`/`fleet-policy audit` with `--ledger
fleet/release-refs.toml` and treat runner-state and full guard-state as pending
acceptance evidence:

```sh
rtk cargo run -p velnor-tools --locked -- fleet-policy audit \
  --policy fleet/policies/tailrocks-desired-policy.json \
  --organization tailrocks
```

## Rollback

If reconciliation fails, keep the organization drained and route new
verification to the explicit GitHub lane. Use the reviewed plan/audit output to
diagnose; never restore broad access. Then remove only registrations owned by
validation and restart the unchanged per-repository units. Because workflow
labels remain constant, no YAML rollback is needed. Do not run both fleet
shapes with the same runner names.
