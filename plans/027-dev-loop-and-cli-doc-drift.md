# Plan 027: Add a one-command dev loop, fix the broken documented CLI flag, and document operator env vars

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. Three small, independent DX fixes.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- mise.toml docs/runner-usage.md crates/velnor-runner/src/cli.rs crates/velnor-runner/debian/velnor.env`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Three onboarding-friction fixes:

1. **No one-command dev loop.** There is no `justfile`/`Makefile` and no
   `[tasks]` in `mise.toml`; a new contributor must reverse-engineer the CI
   gates (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo nextest run`)
   from `ci.yml`, risking local/CI drift.

2. **A documented CLI command fails.** `docs/runner-usage.md` documents
   `configure ... --dry-run-jit-config`, but that flag exists only on the
   `daemon` subcommand; `configure` has `--dry-run`. Copy-pasting the documented
   command errors with clap's "unexpected argument."

3. **Operator env vars are undocumented.** Several vars the code reads
   (`VELNOR_LOG`, `VELNOR_OTLP_ENDPOINT`, `VELNOR_CONFIG_DIR`,
   `VELNOR_IDLE_TIMEOUT_SECONDS`, `VELNOR_REQUIRE_DOCKER_SOCKET`) appear in no
   template or doc, so operators can't discover how to raise log verbosity or
   enable tracing.

## Current state

- No `[tasks]` in `mise.toml` (it has `[tools]` and `[settings]` only). CI
  commands are in `.github/workflows/ci.yml`: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo nextest run --workspace --locked`.
- `docs/runner-usage.md:83-87` documents `configure ... --dry-run-jit-config`.
  `crates/velnor-runner/src/cli.rs` — `ConfigureArgs` defines only `--dry-run`
  (`cli.rs:112-114`); `--dry-run-jit-config` is on `DaemonArgs` (`cli.rs:238-240`,
  field `dry_run_registration`). So the documented `configure` command is
  invalid.
- `crates/velnor-runner/debian/velnor.env` documents ~9 vars (GITHUB_TOKEN,
  VELNOR_URL/NAME/LABELS/SLOTS/TRUST_SCOPE/JOB_CPUS/JOB_MEMORY/WORK_DIR) but not
  the advanced ones the code reads (`VELNOR_LOG` at `telemetry.rs`,
  `VELNOR_OTLP_ENDPOINT`, `VELNOR_CONFIG_DIR` at `config.rs`, plus
  `VELNOR_IDLE_TIMEOUT_SECONDS`, `VELNOR_REQUIRE_DOCKER_SOCKET`). Confirm each by
  grepping `env::var("VELNOR_` and the clap `env =` attributes.

## Commands you will need

| Purpose        | Command                                                          | Expected            |
|----------------|------------------------------------------------------------------|---------------------|
| List mise tasks| `mise tasks`                                                    | shows new tasks     |
| Run a task     | `mise run lint`                                                 | runs clippy         |
| CLI help check | `cargo run -p velnor-runner -- configure --help`               | shows `--dry-run`, not `--dry-run-jit-config` |
| Format         | `cargo fmt --all --check`                                        | exit 0              |

## Scope

**In scope**:
- `mise.toml` — add a `[tasks]` block wrapping the CI commands.
- `docs/runner-usage.md` — fix the `configure --dry-run-jit-config` example.
- `crates/velnor-runner/debian/velnor.env` (and/or `docs/runner-usage.md`) — add
  an "Optional / advanced" env-var section.
- `README.md` / `AGENTS.md` — a one-line pointer to the mise tasks.

**Out of scope**:
- Changing any CLI flag behavior — only docs are wrong, not the code (do not add
  `--dry-run-jit-config` to `configure`; fix the doc).
- Adding new env vars — only document existing ones.

## Git workflow

- Branch: `advisor/027-dev-loop-and-cli-doc-drift`
- Commit(s): `chore(dx): add mise dev-loop tasks`, `docs: fix configure dry-run flag and document advanced env vars`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Add mise dev-loop tasks

Add a `[tasks]` block to `mise.toml` wrapping the exact CI gate commands:

```toml
[tasks.fmt]
run = "cargo fmt --all --check"
[tasks.lint]
run = "cargo clippy --workspace --all-targets --locked -- -D warnings"
[tasks.test]
run = "cargo nextest run --workspace --locked"
[tasks.check]
depends = ["fmt", "lint", "test"]
```

**Verify**: `mise tasks` lists `fmt`, `lint`, `test`, `check`; `mise run fmt`
runs the formatter.

### Step 2: Fix the documented CLI flag

Confirm the correct flag with `cargo run -p velnor-runner -- configure --help`.
In `docs/runner-usage.md:83-87`, change the `configure` example from
`--dry-run-jit-config` to `--dry-run` (the `configure` validate-without-calling-
GitHub flag). Leave `--dry-run-jit-config` in the `daemon` example if it appears
there (that one is correct).

**Verify**: `cargo run -p velnor-runner -- configure --help` shows `--dry-run`
and **not** `--dry-run-jit-config`; the doc now matches.

### Step 3: Document advanced env vars

Confirm the exact set the code reads: `rg 'env::var\("VELNOR_' crates/velnor-runner/src`
and the clap `env =` attributes in `cli.rs`. Add a commented "Optional /
advanced" section to `debian/velnor.env` (and/or a table in
`docs/runner-usage.md`) covering at least: `VELNOR_LOG` (log level),
`VELNOR_OTLP_ENDPOINT` (OTLP tracing, requires the `otel` feature),
`VELNOR_CONFIG_DIR` (config location — note it must not be group-readable, per
plan 025), `VELNOR_IDLE_TIMEOUT_SECONDS`, `VELNOR_REQUIRE_DOCKER_SOCKET`. Only
document vars the code actually reads.

**Verify**: every var you added appears in `rg 'env::var\("VELNOR_'` or a clap
`env =` in the source (no invented vars).

### Step 4: Point to the tasks

Add one line to `README.md` (the "Working on Velnor" section) and/or `AGENTS.md`:
"Run the CI gates locally with `mise run check` (or `fmt`/`lint`/`test`)."

**Verify**: `cargo fmt --all --check` unaffected; the pointer exists.

## Test plan

- No code tests. Verification is `mise tasks` / `mise run`, `configure --help`,
  and the `rg` env-var cross-check.

## Done criteria

- [ ] `mise.toml` has `[tasks]` (`fmt`/`lint`/`test`/`check`) wrapping the CI commands; `mise tasks` lists them
- [ ] `docs/runner-usage.md` no longer documents `configure --dry-run-jit-config`; it uses `--dry-run`
- [ ] Advanced env vars are documented, and every documented var is actually read by the code
- [ ] README/AGENTS point to the mise tasks
- [ ] Only the in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The CLI structs don't match (drift) — re-check `cli.rs` before editing the doc.
- A var you intend to document is not actually read by the code — drop it.

## Maintenance notes

- Keep the mise tasks in sync with `ci.yml` — if CI adds a gate (e.g. plan 017's
  `cargo deny`), add a matching task.
- Reviewer: confirm no documented env var is fictional (the `rg` cross-check).
