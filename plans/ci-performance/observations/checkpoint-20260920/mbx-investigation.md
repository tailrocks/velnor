# Independent MBX cache diagnosis, 2026-09-20

The observed runner job 106091578914 restored 1,726 actions and 6,517 objects (913.6 MiB unpacked, 281,431,913 compressed bytes). Its 3-hit reports therefore do not establish an empty store: almost all compilations could not derive a usable prediction lookup. Precise missing-prediction causes still need `mbx explain --last`/session evidence; do not diagnose them from hit counts alone.

Its final grouped export emitted 3,593 actions and 13,304 objects (8.5 GiB), taking 65.57 seconds (14:23:37.158 to14:24:42.728 UTC). GitHub compression/reservation followed for25.75 seconds, then cache reservation failed because another writer already held the immutable key. The action nevertheless printed `Saved ... (ID -1)`. This is an inaccurate upstream save diagnostic, not evidence that this job uploaded its cache successfully. The original log remains `/tmp/velnor-hooks-performance/runner-106091578914.log`.

Pinned primary-source inspections:

- jdx/mr-boxington-action v1.4.0, commit867fc530102eec5b756075d70d850dc8330d2272, cloned `/tmp/velnor-mbx-action-1.4.0`.
- jdx/mr-boxington v1.12.0, commit a35ac252988359ea8882b5a2766e767e005409c0, cloned `/tmp/velnor-mbx-source-1.12.0`.
- action `src/index.ts:450-494` checks only the restore-time exact hit, then performs grouped export and cache.saveCache, and reports every returned ID as saved. No save-time existence check precedes the expensive export.
- action README distinguishes default target-tree transport from explicit portable objects. Target mode disables managed target views/native link object caching and transports fingerprints, dependencies, build-script state, and registry. Objects mode remains justified for changed checkout/target layouts; no measured Velnor comparison yet.
- mbx `crates/mbx-cache-cargo/src/lib.rs:194-267`: build identity ignores Cargo command, derives workspace/OS/arch identity; predictions are bound to Cargo.lock digest and can inherit previous lockfile states through reachable Git history. Actual invocation digest still separates compiler/target/features/profile. Clippy/test recipe order alone cannot prove artifact reuse.

Next bounded proof: obtain current runner successor diagnostics; compare existing object mode with target mode under the same workload/toolchain/runner and deliberate cold/warm states, preserving source/feature/profile/trust identity and all checks. Collect full job times including post actions. Existing checked-in cache policy may require an explicit typed output-cache justification; inspect it before changing mode.

## Current successor e92520c8

PR CI35526473126 passed first attempt. Runner job106119499702 used github-hosted Linux X64 and restored the exact historical source-key suffix9db6b44b from main as a prefix hit for this changed source. Imported8actions/973objects,836.5MiB; importer additionally restored Cargo workspace state986referencedfiles/882.7MiB. Clippy completed144seconds; nextest compilation217seconds. Reports1879 then1807not-looked-up invocations,3hits each. Whole check wall432seconds, pre-post job marker456seconds. No post-save for restore-only PR. These are naturally occurring workloads, not controlled before/after cohorts. Rawlog `/tmp/velnor-integration/runner-106119499702.log`.

This narrows the hypothesis: restored workspace state and object predictions are present but do not yield broad reuse. Do not attribute absence of transfer counters to absent GitHub transport or assume missing target-state transport; the importer explicitly reports it. Need actual invocation/prediction identity diagnostics.

## Causal producer identity, proven

REST cache metadata names the actual winning cache ID7902037869 (main, created2026-09-20T14:16:36.316130Z,272175196bytes). Preview metadata job106091539484 saved that exact ID/key at14:16:36.384; its export was8actions/973objects836.5MiB. Preview guest job106091539371 and CI test job106091578914 use the same exact key despite different binaries/features/profiles/commands; both later returnedID-1 after exporting distinct closures. The current PR imported exactly metadata's8actions/973objects. Files: metadata-106091539484.log, mbx-cache-seed-metadata.json, retained guest106091539371.log, historical/current runner logs.

Architectural defect: immutable snapshot writer identity omits actual helper workload. Unit identity alone names metadata, guest release compilation and test validation alike. First successful snapshot freezes a partial/different workload; subsequent CI cannot replace it and spends export/compression time losing the same key. Remedy must namespace typed execution workload/profile/features/targets while retaining only compatible CAS restore fallback. This does not yet prove all low reuse causes, nor a measured speedup. It establishes this collision and wrong snapshot selection independently of command ordering.
