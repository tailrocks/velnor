# Velnor

Velnor is a Rust self-hosted GitHub Actions runner and node-local control
plane. GitHub remains the scheduler and job source of truth; Velnor validates
jobs before side effects, executes admitted work through an explicitly selected
Docker or Firecracker backend, and keeps bounded operational evidence.

Docker Rust jobs use the image-pinned Mr Boxington 1.12.0 integration by
default: ordinary `cargo` commands enter Mr Boxington and use a bounded,
host-persistent store scoped by the daemon pool's trust boundary and, when
available in the acquired job payload, repository identity. See
[Job execution](/guides/execution#docker-rust-acceleration) for behavior,
opt-out, and troubleshooting details. This integration does not apply to the
MicroVM backend.

This repository also contains the Velnor documentation site: Fumadocs, MDX,
TanStack Start, strict TypeScript, and Bun.

Read the [documentation](/).

For a fresh clone or agent checkout, install the pinned tools and local commit
checks:

```bash
mise trust
mise install
mise run bootstrap
```

Normal commits validate the complete staged snapshot with `mise run fmt`, then
`mise run lint`. Unstaged fixes, helper changes, and untracked dependencies cannot
make the staged commit pass. The checks do not format or stage files; use
`mise run fmt-fix`, review the result, and stage it explicitly. Linked worktrees
share the installed launcher but validate their own indexes. Rerun bootstrap
after hook tooling changes. Existing foreign hooks are preserved and reported
for explicit integration rather than overwritten.

See [local validation](plans/ci-performance/local-validation.md) for the tool
decision, isolation boundaries, regression scenarios, and offline preparation.

```bash
bun install
bun run dev
```

Documentation content lives under `content/docs/**/*.mdx`. Add route files
under `src/routes`; TanStack Router updates `src/routeTree.gen.ts` for you.

Build the production app with:

```bash
bun run typecheck
bun run build
bun run start
```

Licensed under the [Apache License 2.0](LICENSE).
