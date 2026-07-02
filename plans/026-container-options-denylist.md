# Plan 026: Filter privilege-granting `container.options` (and pin the setup-qemu image)

> **Executor instructions**: This plan has an **investigation step** — confirm
> the fixture/estate does not rely on the flags you deny before enforcing. Run
> every verification. STOP ⇒ report. Update `plans/README.md` when done.
> **Security** plan.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/github_adapter.rs crates/velnor-runner/src/container.rs crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

A workflow's `container.options` are passed **verbatim** to `docker run` with no
denylist, so a workflow can set `--privileged`, `-v /:/host`, `--device`,
`--cap-add=ALL`, `--pid=host`, etc., giving a job host-level reach. Separately,
the native `setup-qemu` adapter runs a **workflow-overridable image** with
`--privileged` and mutates host-global `binfmt_misc` shared by all jobs. The host
Docker socket mount is a *documented* tradeoff (settled), but unfiltered option
pass-through and a workflow-controlled privileged image are **incremental**
privilege beyond that decision — and they turn the trust-scoping work
(plans 006/007) into a soft convention rather than an enforced boundary. This
plan adds a default-deny filter for privilege-granting options (operator-tunable
per trust scope) and pins the qemu image. **Guardrails must not break the fixture
or estate** (AGENTS.md: fix Velnor to meet the fixture, never weaken the
fixture), so Step 1 verifies which options the estate legitimately uses first.

## Current state

- `crates/velnor-runner/src/github_adapter.rs` — options passed through verbatim,
  excerpt at `github_adapter.rs:341-346`:
  ```rust
  fn job_container_options(job: &AgentJobRequestMessage) -> Vec<String> {
      job.job_container.as_ref().and_then(container_options).unwrap_or_default()
  }
  ```
  These flow into `JobContainerSpec.options` and are appended to `docker run`
  around `container.rs:131` (read it to see how they're concatenated onto the
  args).
- The native `setup-qemu` adapter runs a workflow-overridable image with
  `--privileged` — grep `native_setup_qemu` / `binfmt` in `executor.rs`
  (around `executor.rs:1789-1802`); the `image` input defaults to
  `tonistiigi/binfmt`.
- `VELNOR_TRUST_SCOPE` (default `"trusted"`) already exists (see plan 006/007) —
  reuse it to allow an operator escape hatch for trusted scopes.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner options --locked`           | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |
| Fixture   | (operator) run the fixture smoke lane                            | passes          |

## Scope

**In scope**:
- `crates/velnor-runner/src/github_adapter.rs` — filter `job_container_options`.
- `crates/velnor-runner/src/executor.rs` — pin/validate the setup-qemu image.
- Tests.

**Out of scope**:
- Removing the Docker socket mount (documented, settled tradeoff) — not changed.
- Full container sandboxing (DinD/rootless) — a larger effort; note as the real
  long-term boundary.

## Git workflow

- Branch: `advisor/026-container-options-denylist`
- Commit: `fix(security): deny privilege-granting container.options; pin setup-qemu image`
- Do NOT push/PR unless instructed.

## Steps

### Step 1 (INVESTIGATE — report before enforcing)

Search the estate/fixture workflows for `container.options` / `options:` usage
and list which flags actually appear (the fixture repo
`tailrocks/velnor-actions-fixture` and the ChainArgos workflows are the ground
truth). Write the list in the PR. The denylist must **not** reject any flag the
fixture/estate legitimately relies on — if it does, that flag needs an allowed
carve-out or an operator override, per AGENTS.md (meet the fixture, don't weaken
it).

**Verify**: no code change; the used-options list is documented.

### Step 2: Add a default-deny filter for privilege-granting flags

In `job_container_options`, filter out privilege-granting flags by default:
`--privileged`, `--cap-add`, `--device`, `--pid=host`/`--pid host`,
`--network=host`/`--network host`, `--security-opt` (relaxations), and host
bind mounts (`-v /:...`, `--mount ...source=/...`). Provide an operator escape
hatch keyed on `VELNOR_TRUST_SCOPE` (or a dedicated
`VELNOR_ALLOW_PRIVILEGED_OPTIONS` env) so a trusted scope can opt back in.
Log (mask-safe) each dropped flag so operators can see what was filtered.

Handle flag spellings robustly (`--flag=value` and `--flag value`, and short
aliases where relevant). If the args are a flat `Vec<String>`, parse pairs
carefully so you don't drop the value of an allowed flag.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 3: Pin the setup-qemu image

Make the `setup-qemu` adapter use a pinned, known image digest by default and
**ignore** a workflow-supplied `image` override that points elsewhere (or
require it to match an allowlist). Keep `--privileged` only as required by
binfmt registration, and document that this adapter mutates host-global
`binfmt_misc`.

**Verify**: `cargo clippy ...` → exit 0.

### Step 4: Tests

Add tests in `github_adapter.rs`/`executor.rs` `#[cfg(test)]`:
- `container_options_drops_privileged`: input options including `--privileged`
  and `-v /:/host` are dropped by default; benign options (e.g. `--hostname x`)
  pass through.
- `container_options_allowed_when_trusted`: with the trusted-scope override set,
  privileged options pass (the escape hatch works).
- `setup_qemu_uses_pinned_image`: a workflow-supplied off-allowlist image does
  not change the pinned image used.

**Verify**: `cargo nextest run -p velnor-runner options --locked` → pass;
`cargo nextest run --workspace --locked` → all pass; then (operator) the fixture
smoke lane passes.

## Test plan

- Denylist drop + benign passthrough; trusted-scope override; qemu image pin.
- Verification: `cargo nextest run --workspace --locked` → all pass; fixture
  smoke green.

## Done criteria

- [ ] PR lists the `container.options` flags the fixture/estate actually use
- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] Privilege-granting options are dropped by default and logged; a trusted-scope override re-enables them
- [ ] The setup-qemu image is pinned and not overridable to an arbitrary image
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass; fixture smoke passes
- [ ] Only the in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The denylist would reject a flag the fixture/estate uses (Step 1) — STOP;
  carve it out or add an override; never weaken the fixture.
- The options args structure is ambiguous to parse safely (dropping a value by
  mistake) — report.
- Pinning the qemu image breaks a fixture lane that needs a specific binfmt
  image — report; allowlist that image instead.

## Maintenance notes

- The real boundary is container isolation (rootless/DinD); this denylist reduces
  the blast radius but does not replace it. Record that as the strategic
  follow-up.
- Ties into plans 006/007 (cache/tool-store scoping) — those become *enforced*
  boundaries only once host-level escape via options is closed.
- Reviewer: confirm the escape hatch is off by default and that dropped flags are
  logged, and insist on the fixture-smoke evidence.
