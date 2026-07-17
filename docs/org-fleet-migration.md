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

## Rollback

Drain the organization daemon, remove only its registrations, and restart the
unchanged per-repository units. Because workflow labels remain constant, no YAML
rollback is needed. Do not run both fleet shapes with the same runner names.
