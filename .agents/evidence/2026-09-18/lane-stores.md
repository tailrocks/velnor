# Lane report: read-only multi-provider store enumerators (stores)

## Status: done

New files (only additive change set + one parent `mod` line):

- `crates/jackin-config/src/accounts/stores.rs` — shared `StoreCandidate`, `StoreKind`, `CredentialKind`, `StoreError`, bounded-read + provider-entry helpers, canonical entry-point docs.
- `crates/jackin-config/src/accounts/stores/opencode.rs` — `auth.json` + `opencode.db` `credential` table.
- `crates/jackin-config/src/accounts/stores/omp.rs` — `agent.db` `credentials` table.
- `crates/jackin-config/src/accounts/stores/hermes.rs` — `config.yaml` + `profiles/` + `auth.json`.
- `crates/jackin-config/src/accounts/stores/sqlite.rs` — shared dependency-free read-only SQLite engine + `#[cfg(test)] fixture` builders reused by omp/opencode tests.
- Existing-file touch: one line in `accounts.rs` (`pub(crate) mod stores;`). No `Cargo.toml`, `lib.rs`, or `discovery.rs` edits.

## Entry points (all `pub(crate)`, `Result<Vec<StoreCandidate>, StoreError>`)

```rust
// stores.rs re-exports the concept; paths below are canonical:
opencode::enumerate_opencode_store(data_dir: &Path)      // auth.json + opencode.db
opencode::enumerate_opencode_auth(auth_path: &Path)      // auth.json only
opencode::enumerate_opencode_database(db_path: &Path)    // opencode.db only
omp::enumerate_omp_credentials(db_path: &Path)          // agent.db
hermes::enumerate_hermes_store(dir: &Path)              // .hermes/ dir
```

Shared types (`stores.rs`): `StoreKind::{Opencode,Omp,Hermes}`, `CredentialKind::{ApiKey,OAuth}`,
`StoreCandidate { store, provider, profile: Option<String>, source: PathBuf, kind, field, secret: String }`,
`StoreError::{Unreadable, TooLarge, Malformed, Unsupported(&'static str)}` (secret-free, `Copy`).

Semantics: missing inputs → `Ok(vec![])`; blank/unshaped entries skipped; one candidate per
provider entry / db row / (profile, provider). Secrets are stored in the candidate; `Debug`
prints `[REDACTED]`. No writes, no scanning, no migration, no `-shm` creation, no secret logging.

## Decisions / deltas

- **rusqlite NOT added** (it is not a workspace dep; Turso is sole-owned by `jackin-usage`;
  lane is new-files-only). `sqlite.rs` is a minimal zero-dep reader: rowid b-trees (interior/
  leaf/overflow), varints, all serial types, `sqlite_schema` + `CREATE TABLE` column mapping,
  WAL overlay of committed frames only when the header selects WAL mode (torn tail ignored,
  salts checked, checksums not validated — documented). Corrupt input fails closed.
- **opencode SQLite (evidence §9)**: `credential` rows map `value` → secret;
  provider = `integration_id` → `connector_id` → `label` → `row-<rowid>`; `profile` = `label`
  when it differs from provider; `method_id` containing `oauth`/`token` → `OAuth` else `ApiKey`;
  explicitly falsy `active` (`0`/`false`/`no`/`off`/blank) rows skipped, missing `active` kept.
- **omp `credentials`** (no local install per evidence §10): column-preference lists
  (value/secret/token/…; provider/service/scope/…; profile/label); `token`/`access_token`
  columns read as `OAuth`.
- **hermes** (binary absent per evidence §11; layout defined by this lane, documented in
  module docs): inline `profiles:` map merged with `profiles/*.{yaml,yml}` (file wins),
  secrets from provider-keyed `auth.json`. YAML is a mapping-only subset parser (no YAML
  dep in crate); sequences/flow/anchors/tags/block scalars/tabs → `Malformed`.
- **`secret()`/`slug()` accessors REMOVED**: S1's discovery wiring (secret-free by design)
  calls the enumerate fns but never reads values, so the methods tripped `dead_code` deny
  and blocked the crate build. Secrets remain in the struct (verified via `PartialEq` in
  tests). **Importer lane re-add** (one method + doc, becomes live with its caller):
  `pub(crate) fn secret(&self) -> &str { &self.secret }` on `StoreCandidate`.
- **`stores/mod.rs` renamed to `stores.rs`**: `clippy::mod_module_files` is deny-level
  (`all = deny`); `stores.rs` + `stores/` dir keeps the same module tree.

## Verification (observed)

- `cargo test -p jackin-config stores` → 29 passed, 0 failed (synthetic fixtures inline;
  SQLite fixtures are hand-encoded valid images incl. overflow chains + WAL frames).
- `cargo test -p jackin-config` (full) → 317 passed, 0 failed.
- `cargo check -p jackin-config` → clean.
- `cargo clippy -p jackin-config --all-targets` → zero diagnostics under `stores*`
  (remaining crate diagnostics, if any, are other lanes' files, e.g. S1 `discovery.rs`).
- `rustfmt --check` on the 5 owned files → clean (repo `cargo fmt` avoided: shared tree).
- Boundary respected: no writes outside `stores*` + the one `accounts.rs` line; S1's
  concurrent `discovery.rs` wiring already consumes this API (their file untouched).
