# Pool pre-investigation: Plans 074/075/076 baselines

Session: third peer (Plans 074-078 pool owner). Read-only explore subagent at
HEAD `041079c`, 2026-08-24 ~12:35Z. Orientation only; fresh per-leaf
investigator revalidates drift at claim time.

## 1. Plan 076 surface (Debian package transition)

Core module: `crates/velnor-runner/src/release.rs` (1150 ln) + tests
`crates/velnor-runner/src/release/tests.rs`.

| Item | Location | Class |
|---|---|---|
| Release schemas (ReleaseRecord/PublicationRecord/DeployedIdentity) | release.rs:261-356 | PACKAGE-PRODUCTION |
| Coherence verify (`verify`, `verify_record_bytes`, `verify_installed`, `verify_publication_binds`) | release.rs:455-648 | HOST-VERSION-MGMT |
| Record assembly/emit guards (`assemble`, `emit_record`) | release.rs:666-699 | PACKAGE-PRODUCTION |
| Atomic write/symlink primitives (`write_atomic`, `write_atomic_symlink`) | release.rs:708-731, 880-895 | HOST-VERSION-MGMT |
| ReleaseStore: records/<tag>/record.json+deployed.json, active/previous symlinks, activate/rollback | release.rs:752-867 | HOST-VERSION-MGMT (079 delete candidate) |
| OCI pull/tag verification via docker CLI (`verify_and_tag_release_image`) | release.rs:1020-1081 | HOST-VERSION-MGMT |
| CLI variants Emit/Assemble/VerifyRecord/VerifyInstalled/Activate/Rollback/Export | cli.rs:56-73; dispatch release.rs:901-911, lib.rs:101 | mixed |
| Default host state paths /var/lib/velnor/release/... | cli.rs:44-46 | HOST-VERSION-MGMT |
| Startup hook systemd ExecStartPre `release verify-installed` | debian/velnor-daemon.service:35 (+ @.service) | HOST-VERSION-MGMT |
| Maintainer scripts postinst/preinst/postrm/prerm | crates/velnor-runner/debian/* | PACKAGE-PRODUCTION |
| Unit assets daemon/daemon@/doctor/doctor@ + timers + velnor.env (10 files) | crates/velnor-runner/debian/* | PACKAGE-PRODUCTION |
| Embedded build identity build.rs VELNOR_SOURCE_SHA/TAG/KIND | release.rs:58-84 | PACKAGE-PRODUCTION |
| deb build/sign/publish workflow (identity/metadata/image/build/release/parity jobs; deb guard 396-431, signer 556-565, publish 583-603) | .github/workflows/release.yml | PACKAGE-PRODUCTION |
| cargo-deb asset mapping incl usr/share/velnor/build-identity.json | crates/velnor-runner/Cargo.toml:58-59 | PACKAGE-PRODUCTION |
| Dockerfile builds runner+tools, ENTRYPOINT velnor-runner | Dockerfile:26,42,46 | PACKAGE-CONSUMPTION |

No sqlite/custom version DB found: installed-version state is ReleaseStore
files only.

## 2. Plan 074 surface (GitHub Actions client)

| Item | Location | Reusable? |
|---|---|---|
| GitHubScope URL builder (jit-config/runners/groups/cancel/queued-runs/org-repos) | protocol.rs:182-330 | reusable |
| Endpoints: generate-jitconfig 208; runners CRUD 897/1178/1189; groups 946; queued fan-out repos→runs→jobs 984-1063; cancel run 1148; pagination per_page=100 loop 1030-1062 | protocol.rs | reusable |
| Broker/V2 DTOs + poll classify + curl_json_request shell-out | protocol.rs:2403+, 1466, 1263 | reusable (runner-owned) |
| Results Service artifacts client + zip download/filter | protocol.rs:3332-3338, 4012-4200 | reusable |
| Retry/rate-limit hints Retry-After/X-RateLimit-Reset | protocol.rs:165-180 | reusable |
| Auth GitHub App JWT + OAuth exchange + PAT bearer | protocol.rs:714-728, 611-700 | reusable |
| DTOs ListedWorkflowJob/RunnerGroup/DecodedJitConfig/ListedRunner/OAuthTokenResponse | protocol.rs:289,379,385,392,564 | reusable |
| FleetHttp seam + ReqwestFleetHttp retry/ratelimit/pagination | fleet_policy_client.rs:50-194,305-320,402-405,894-942 | maintainer-only seam template |
| Lane compare via gh api subprocess + HTML via curl | lane_compare.rs:14-18,454,488,557-592 | maintainer-only |
| Dispatch+run-id discovery via `gh run list` inference | main.rs:3065-3109, helpers 754,3372-3399 | maintainer-only; Plan 074 prohibits this pattern for product |

Gaps for Plan 074 (NOT FOUND): rerun endpoint, native workflow-dispatch POST
(exact run ID from HTTP 200 body required), logs-delete, queue-time metrics.

## 3. Plan 075 surface (storage control)

| Item | Location | Notes |
|---|---|---|
| StorageLayout resolve, canonical /var layout | storage.rs:42-79 | typed |
| Legacy-path shims + trust-scope suffix | storage.rs:81-119 | typed |
| `storage paths`/`status` println direct | storage.rs:24-38 | PUBLIC-PRINTS to extract |
| Catalog/dir-size walker | storage.rs:128-177 | typed |
| Cache stores/budgets du prints | cache.rs:59-109 | PUBLIC-PRINTS |
| accounting_summary logical/physical | cache.rs:111-120 | typed |
| GC run_gc --yes gate, GcLeaderLock + FilesystemCoordinator exclusive, EvictionPolicy/select candidates, gc-history.jsonl append | cache.rs:127-149,232-240,762+ | mixed |
| Lease bypass `force_no_lease_check` → empty scopes + warning | cli.rs:258; cache.rs:141-149 | MUST DELETE per leaf |
| reclaim/reclaim_work_root priority + builder prune fallback | cache.rs:442-525,572,535 | typed ReclaimReport 436-440 |
| Physical st_blocks*512 accounting sparse/reflink-aware | cache.rs:620-652 | typed |
| Capacity lease/reservation records, flock shared/excl, stale detection, CapacityController.reserve_with_free_bytes | capacity.rs:247-315,317,325,352-360,364-430,432-465 | typed; state = JSON files under run_root, NO DB yet |

Plan 075 requires authoritative `/var/lib/velnor/storage.db` catalog:
JSON-file state needs migration to the new catalog in that leaf.

## Crate layout at HEAD 041079c

- crates/velnor-runner — legacy monolith (executor, runner, protocol, release,
  cache, capacity, storage, debian assets)
- crates/velnor-tools — maintainer automation (main.rs 4728 ln, audit_ci,
  fleet_policy(+_client), lane_compare)
- crates/velnorctl — operator CLI scaffold (globals parse, metadata/help/man/
  schema, legacy-command rejection list lib.rs:36-48); no real leaves yet
- crates/velnor-model — resources/time/error_envelope/since/condition;
  CRATE_VERSION
- crates/velnor-render — output rendering (lib.rs 677 ln)
- crates/velnor-control — stub marker const + 1 test
- crates/velnor-client — stub transport-contract marker const

Approx 1797 test attributes across crates/*/src.
