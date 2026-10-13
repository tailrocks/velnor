# Velnor

Velnor is a Rust self-hosted GitHub Actions runner and node-local control
plane. GitHub remains the scheduler and job source of truth; Velnor validates
jobs before side effects, executes admitted work through an explicitly selected
Docker or Firecracker backend, and keeps bounded operational evidence.

This repository also contains the Velnor documentation site: Fumadocs, MDX,
TanStack Start, strict TypeScript, and Bun.

Read the [documentation](https://velnor.tailrocks.com/docs).

Documentation deploys automatically from `main` to
`https://velnor.tailrocks.com` through GitHub Pages.

```bash
bun install
bun run dev
```

Documentation content lives under `content/docs/**/*.mdx`. Add route files
under `src/routes`; TanStack Router updates `src/routeTree.gen.ts` for you.

The Pages build uses `bun run build:static` to prerender the site and its
Markdown and search endpoints into `.output/public`.

Build the production app with:

```bash
bun run typecheck
bun run build
bun run start
```

Licensed under the [Apache License 2.0](LICENSE).
