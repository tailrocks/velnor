# Velnor

Velnor is a **drop-in replacement for the GitHub Actions runner**, built in Rust
and specialized for Rust projects.

GitHub still parses your `.github/workflows/*.yml`, expands matrices and reusable
workflows, manages secrets, schedules jobs, and renders the Actions UI. Velnor
replaces only the **runner side**: it registers as a self-hosted runner (V2 JIT),
runs each assigned Linux job in a Docker container, and executes the known action
surface through native Rust adapters. Because it is self-hosted and controls
execution, it aims to be **faster, cheaper, nicer, and more informative** than
GitHub-hosted runners.

Phase 0 scope: run the existing Rust CI/CD workflows of `jackin-project/jackin`
and `ChainArgos/java-monorepo` unchanged, proving correctness against the public
fixture first. No Velnor-native workflow language, no YAML scheduler, no macOS
job execution.

## Documentation (source of truth)

All direction lives in [`docs/`](docs/) — keep it current; everything else
defers to it.

- [Mission](docs/mission.md) — what Velnor optimizes for (fastest, Rust-first,
  nicer output, cheaper).
- [Vision](docs/vision.md) — product goal and Phase 0 scope.
- [Roadmap](docs/roadmap.md) — the plan: what must be implemented, target
  analysis, protocol decisions, verification order.
- [UI comparison](docs/comparison.md) — Velnor vs GitHub-hosted runner output,
  with the GitHub-API extraction method.
- [Runner usage](docs/runner-usage.md) — how to configure, run, and prove the
  runner locally and against the fixture.
- [Docs index](docs/README.md) — everything else (adapter contract, automation
  policy, runbook, runner V2 reference).

## Working on Velnor

Large, scoped work is driven by **goal prompts** in [`prompts/`](prompts/) — run
them with `/goal` (Claude Code or Codex), one per day, in the recommended
sequence. See [`prompts/README.md`](prompts/README.md).

Contributor rules are in [`AGENTS.md`](AGENTS.md).

Run the CI gates locally with `mise run check` (or `mise run fmt`,
`mise run lint`, and `mise run test` for focused loops).

## License

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
