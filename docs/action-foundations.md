# Action foundations

`velnor-action-model`, `velnor-cas`, and `velnor-action-journal` are the
dependency-free foundation for supersession-safe physical actions.

## Boundaries

- `velnor-action-model` owns canonical BLAKE3-256 digests, action keys, result
  provenance, platform identity, execution policy, and the standard action/state
  vocabularies. It performs no I/O and knows nothing about GitHub.
- `velnor-cas` owns digest-addressed bytes and immutable tree manifests. Writes
  use fsync plus atomic rename. Reads verify the requested digest and reject
  symlinked CAS components; corruption is reported as
  `corrupt_entry_rejected`. `SubsetSelector` provides lazy
  executables/runtime/bundle/metadata materialization. Each materialized file
  probes APFS clone/reflink support and falls back to a verified private copy;
  hardlinks are intentionally excluded because they would create mutable
  aliases. `put_with_budget` is the bounded-store hook; policy/GC ownership is
  TASK-028.
- `velnor-action-journal` owns append-only lifecycle events. It uses the pinned
  bundled SQLite already used by Velnor, with WAL and `synchronous=FULL`. The
  record JSON and BLAKE3 checksum are committed in one transaction and verified
  on replay.

## Migration map

| Existing mechanism | Future home | Migration task |
| --- | --- | --- |
| `velnor-runner/src/execution/cache_transport.rs` digest-checked payloads | `velnor-cas` stream/get API | 028/029 |
| `velnor-runner/src/compiler_cache.rs` compiler-cache seam | `velnor-cas` plus compiler-cache service | 023 |
| `velnor-control/src/journal.rs` daemon slot/job/outbox journal | `velnor-action-journal` for physical actions; control journal remains for fleet state | 021/031 |
| Existing target snapshots and trust-scoped stores | `TreeManifest` and typed-store layers | 028/029 |

The foundation crates do not rewire existing executors. Adapter and service
migrations remain in tasks 021–033.
