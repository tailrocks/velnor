# Plan 025: Write the runner credential file with 0600 permissions and a 0700 directory

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. **Security** plan.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/config.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`config::save` writes the runner settings file with `fs::write` (no mode) into a
directory created with `fs::create_dir_all` (default 0755). That file holds
`StoredRunnerConfig` — the decoded JIT `.credentials`, i.e. the runner-session
OAuth signing key / access token. With a typical umask the file lands 0644
(world-readable). Under the shipped systemd unit it sits in root's HOME
(`/root`, 0700) so ambient protection covers it — but if an operator points
`VELNOR_CONFIG_DIR` at the group-readable state dir (`/var/lib/velnor`, 0750),
the credential becomes readable by the `velnor` group. This is inconsistent with
the 0600 hygiene the curl transport already uses for transient token files. Fix:
create the dir 0700 and write the file 0600.

## Current state

- `crates/velnor-runner/src/config.rs` — `save`, excerpt at `config.rs:67-73`:
  ```rust
  pub fn save(dir: &Path, config: &StoredRunnerConfig) -> Result<()> {
      fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;   // default 0755
      let path = dir.join(SETTINGS_FILE);
      let bytes = serde_json::to_vec_pretty(config)?;
      fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;   // default umask (~0644)
      Ok(())
  }
  ```
- `StoredRunnerConfig` carries the session credentials (grep its definition; it
  is populated from the JIT `.credentials`).
- The 0600 pattern to mirror is in `protocol.rs` (the curl `--config` temp file
  is created with `std::os::unix::fs::OpenOptionsExt::mode(0o600)`).

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner config --locked`           | new test passes |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/config.rs` — `save` (dir mode + file mode); a test.

**Out of scope**:
- Relocating the default config dir — not changed here.
- Non-Unix platforms — the mode calls are Unix; gate with `#[cfg(unix)]` if the
  crate builds on non-Unix (it targets Linux; a `cfg` guard keeps it portable).

## Git workflow

- Branch: `advisor/025-credential-file-permissions`
- Commit: `fix(security): write runner credentials 0600 in a 0700 directory`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Create the directory 0700 and write the file 0600

Rewrite `save` to set restrictive permissions on Unix:

```rust
pub fn save(dir: &Path, config: &StoredRunnerConfig) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ok();
    }
    let path = dir.join(SETTINGS_FILE);
    let bytes = serde_json::to_vec_pretty(config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true)
            .mode(0o600).open(&path)
            .with_context(|| format!("write {}", path.display()))?;
        f.write_all(&bytes).with_context(|| format!("write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
```

Note: `OpenOptions::mode` only applies on **create**; if the file already exists
with looser perms, also `set_permissions(&path, 0o600)` after opening to
normalize an existing file.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy --workspace
--all-targets --locked -- -D warnings` → exit 0.

### Step 2: Test the file mode (Unix)

Add `saved_config_is_0600` in `config.rs` `#[cfg(test)]` (gate `#[cfg(unix)]`):
save to a temp dir, then assert the file's mode `& 0o777 == 0o600` and the dir's
mode `& 0o777 == 0o700`. Model temp-dir setup on any existing test in the crate
that uses a temp path.

**Verify**: `cargo nextest run -p velnor-runner config --locked` → passes;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- `saved_config_is_0600` (Unix): file 0600, dir 0700.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] `config::save` writes the credential file 0600 and its dir 0700 on Unix
- [ ] An already-existing file is normalized to 0600 (not left loose)
- [ ] `cargo nextest run --workspace --locked` exits 0; the Unix mode test passes
- [ ] Only `config.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The `save` excerpt doesn't match (drift).
- The crate must build on a non-Unix target and the `cfg` split doesn't compile
  there — report.

## Maintenance notes

- If the credential file may already have been written group-readable on a
  production host with a shared `VELNOR_CONFIG_DIR`, recommend rotating the
  affected runner registration/session credential as a precaution (note in the
  PR; the operator decides).
- Document that `VELNOR_CONFIG_DIR` must not point at a shared/group-readable
  location (ties into plan 027's env-var docs).
- Reviewer: confirm the mode is enforced on both create and pre-existing file.
