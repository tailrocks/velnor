# Velnor

Velnor is a Rust self-hosted GitHub Actions runner and node-local control
plane. GitHub remains the scheduler and job source of truth; Velnor validates
jobs before side effects, executes admitted work through an explicitly selected
Docker or Firecracker backend, and keeps bounded operational evidence.

Velnor admits a closed allowlist of actions and rejects every other
JavaScript, Docker, or composite action at planning. Trust scope defaults to
`untrusted` and fails closed: a pool that needs the host Docker socket or user
secrets must set `VELNOR_TRUST_SCOPE=trusted` explicitly.

Docker Rust jobs use the image-pinned Mr Boxington 1.7.0 integration by
default: ordinary `cargo` commands enter Mr Boxington and use a bounded,
host-persistent store partitioned by repository identity and trust class. See
[Job execution](/guides/execution#docker-rust-acceleration) for behavior,
opt-out, and troubleshooting details. This integration does not apply to the
MicroVM backend.

This repository also contains the Velnor documentation site: Fumadocs, MDX,
TanStack Start, strict TypeScript, and Bun.

Read the [documentation](/).

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
