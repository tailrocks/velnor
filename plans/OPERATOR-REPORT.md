# Estate program operator report

Append-only record of autonomous decisions, deviations, safety skips, and
human-only actions for plans 014–015 and 033–059.

## 2026-07-18 — Context7 unavailable

- Decision: the repository requires Context7 for current third-party docs, but
  no Context7 MCP tools are exposed in this execution environment. Continue
  using primary upstream sources only, as the closest safe fallback, and cite
  the source in implementation/PR evidence.

## 2026-07-18 — Plan 015 history purge deferred

- Current HEAD already contains the safe half: no tracked
  `.velnor-compare/*.html` file remains, `.gitignore` rejects such files, and
  `AGENTS.md` contains the never-commit policy.
- Evidence: `git log --all --diff-filter=A --name-only --
  '.velnor-compare/*.html'` identifies addition commit `55ed22fe3162228876c1c5829d3c4d966b28e0d2`.
- Safety decision: do not rewrite or force-push shared history without the
  explicitly required coordinated operator window.
- Human action during an approved window:
  `git filter-repo --path .velnor-compare/velnor-job.html --invert-paths`,
  force-push every rewritten ref, notify collaborators, and require fresh
  clones. Verify with `git log --all --oneline -- '.velnor-compare/*.html'`.

## 2026-07-18 — Plan 039 organization permissions unavailable

- Evidence: both the local authenticated `gh` session and each existing Sentry
  daemon credential returned HTTP 403 for
  `GET /orgs/{ChainArgos,jackin-project,tailrocks}/actions/runner-groups`.
  Repository-scoped fleet inspection still succeeds; all 22 current slots were
  online and idle at the inventory point.
- Code and the migration runbook can land, but live organization registration
  cannot pass until a credential has organization `Self-hosted runners: read
  and write` (fine-grained/GitHub App) or classic `admin:org`, and the runner
  groups plus repository access lists exist.
- Human action: grant the daemon credential that permission in each
  organization, create the restricted trusted/fork runner groups described in
  `docs/org-fleet-migration.md`, then run the documented tailrocks-first smoke.

## 2026-07-18 — Debian deployment process (operator decision)

All live Sentry Velnor deployments must use the Debian standard process: the
change is committed with DCO signoff and pushed, a `.deb` is built from that
exact commit, and the artifact is installed through `apt`. Direct `dpkg -i`
deployment is prohibited. Same-version host validation uses
`apt-get install --reinstall -y ./velnor-runner_<version>_amd64.deb`.
