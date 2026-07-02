# Plan 017: Add a `cargo-deny` advisory gate and de-duplicate the RustCrypto stack

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- Cargo.toml Cargo.lock mise.toml .github/workflows/ci.yml`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: 016 (so the `deny.toml` need not ignore `serde_yaml`)
- **Category**: dx
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Renovate keeps dependency **versions** fresh, but nothing **fails the build**
when the locked tree carries a live security advisory. That is why unmaintained/
advisory-bearing crates can sit in `Cargo.lock` unflagged, and a `.deb` ships
from this tree. A `cargo-deny` advisories check in CI closes that gap and gives a
documented home for consciously-accepted advisories. Secondary hygiene: the
direct `sha2 = "0.11"` pin pulls a **second** copy of the RustCrypto 0.10 stack
(rsa/jsonwebtoken still use 0.10), duplicating sha2/digest/block-buffer in the
build; pinning the direct dep to match removes the dup.

## Current state

- CI jobs are `fmt`, `actionlint`, `clippy`, `test`, `ci-required`
  (`.github/workflows/ci.yml`) — no advisory/deny step
  (`rg "cargo.deny|cargo.audit|rustsec" .github mise.toml renovate.json` returns
  nothing). No `deny.toml` at repo root.
- `mise.toml` pins tools under `[tools]` (actionlint, cargo-nextest,
  cargo-zigbuild, cargo-deb, zig).
- Duplicate stack in `Cargo.lock`: `sha2 0.10.9` **and** `0.11.0`; `digest`
  `0.10.7`+`0.11.3`; `block-buffer` `0.10.4`+`0.12.0`; `crypto-common`
  `0.1.6`+`0.2.2`. Root cause: `Cargo.toml:31` declares `sha2 = "0.11.0"`
  directly, while `rsa 0.9.10` and `jsonwebtoken 10.x` pull `sha2 0.10.9`.
- Known advisories to accept-and-document (not fix now):
  - **RUSTSEC-2023-0071** (`rsa` Marvin timing sidechannel) — usage is
    OAuth-JWT **signing only, local, ephemeral key** (`protocol.rs:479,858`); the
    advisory's decryption-oracle threat is not on this path, and **no fixed `rsa`
    release exists**. Accept with justification.
  - **RUSTSEC-2024-0320** (`serde_yaml` unmaintained) — **removed** by plan 016;
    only ignore it here if 016 has not landed yet.

## Commands you will need

| Purpose        | Command                                                          | Expected               |
|----------------|------------------------------------------------------------------|------------------------|
| Run deny local | `cargo deny check advisories` (after adding the tool)            | exit 0 (with ignores)  |
| Format         | `cargo fmt --all --check`                                        | exit 0                 |
| Lint           | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0                 |
| Dup check      | `grep -n 'name = "sha2"' Cargo.lock`                            | one version after fix  |
| All tests      | `cargo nextest run --workspace --locked`                        | all pass               |

## Scope

**In scope**:
- `deny.toml` (create), `mise.toml` (add `cargo:cargo-deny`),
  `.github/workflows/ci.yml` (add the advisory job, wire into `ci-required`).
- `Cargo.toml` (pin `sha2` to `0.10` to match the RustCrypto 0.10 stack) +
  regenerated `Cargo.lock`.

**Out of scope**:
- License/ban/sources checks in `cargo-deny` — start with `advisories` only to
  avoid noise; a follow-up can add the others.
- The `serde_yaml` migration itself — plan 016.

## Git workflow

- Branch: `advisor/017-cargo-deny-advisory-gate`
- Commit(s): `chore(ci): add cargo-deny advisories gate`, `chore(deps): dedupe sha2 to the 0.10 RustCrypto stack`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: De-duplicate `sha2`

Change `Cargo.toml:31` from `sha2 = "0.11.0"` to `sha2 = "0.10"` (match the
version `rsa`/`jsonwebtoken` already pull). If the single direct `sha2` use-site
uses a 0.11-only API, adjust it to the 0.10 API (0.10 is stable and widely used).
Regenerate the lock with `cargo build --workspace`.

**Verify**: `grep -n 'name = "sha2"' Cargo.lock` shows a single version;
`cargo nextest run --workspace --locked` → all pass. If two `sha2` versions
remain (a transitive dep forces 0.11), that is acceptable — note it and proceed
(the advisory gate is the primary deliverable).

### Step 2: Add `deny.toml`

Create `deny.toml` with an `advisories` section that documents each accepted
advisory with a justifying comment:

```toml
[advisories]
version = 2
ignore = [
    # rsa: Marvin timing sidechannel. Usage is OAuth-JWT signing only, with a
    # locally-generated ephemeral key (protocol.rs); the decryption-oracle
    # threat is not on this path, and no fixed rsa release exists yet.
    "RUSTSEC-2023-0071",
    # serde_yaml unmaintained — REMOVE this entry once plan 016 (migration to
    # serde-yaml-ng) has landed; keep only until then.
    # "RUSTSEC-2024-0320",
]
```

Uncomment the `serde_yaml` line **only** if plan 016 has not yet landed.

**Verify**: `cargo deny check advisories` → exit 0 (install cargo-deny locally
first if needed; do not commit local install state).

### Step 3: Wire it into CI

- Add `"cargo:cargo-deny"` to `mise.toml` `[tools]` (pin a version).
- Add a `deny` job to `.github/workflows/ci.yml` that installs tools via
  `mise-action` and runs `cargo deny check advisories`. Add `deny` to the
  `ci-required` job's `needs` list so it gates merges (match the existing job
  shape — checkout + mise-action + the command).

**Verify**: `actionlint` passes on the edited workflow
(`mise exec -- actionlint` or the repo's actionlint invocation); the new job
mirrors the structure of the existing jobs.

## Test plan

- No Rust unit tests. Verification is `cargo deny check advisories` locally and
  the CI job structure (actionlint clean).

## Done criteria

- [ ] `deny.toml` exists with documented `ignore` entries (rsa; serde_yaml only if 016 not landed)
- [ ] `cargo deny check advisories` exits 0 locally
- [ ] `mise.toml` pins `cargo:cargo-deny`; `ci.yml` has a `deny` job wired into `ci-required`
- [ ] `sha2` direct pin is `0.10` (or the remaining dup is explained)
- [ ] `actionlint` passes; `cargo nextest run --workspace --locked` exits 0
- [ ] Only the in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The CI file structure doesn't match the excerpt (drift).
- `cargo deny check advisories` reports an advisory **not** in the known list
  (rsa, serde_yaml) — STOP and report it; do not blanket-ignore to make CI green.
- Pinning `sha2 = "0.10"` breaks a build due to a 0.11-only API in a dependency
  — report.

## Maintenance notes

- When plan 016 lands, delete the `serde_yaml` ignore from `deny.toml`.
- Revisit the `rsa` ignore when a patched `rsa` release ships (then upgrade and
  drop the ignore).
- A future PR can extend `cargo-deny` to `bans`/`licenses`/`sources`; keep this
  one advisory-only to stay low-noise.
- Reviewer: confirm the `deny` job actually gates (`ci-required` needs it), not
  just runs.
