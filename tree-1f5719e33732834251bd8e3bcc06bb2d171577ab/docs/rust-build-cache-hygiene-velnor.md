# Rust Build Cache Hygiene → Velnor Improvements

Status: analysis (2026-07-18)  
Source: [jackin `rust-build-cache-hygiene.mdx`](https://raw.githubusercontent.com/jackin-project/jackin/0cbada6dc0cd2adfc603bffd17287145520d374c/docs/content/docs/roadmap/rust-build-cache-hygiene.mdx)  
Related: [storage-and-disk-pressure-2026-07-18.md](storage-and-disk-pressure-2026-07-18.md), [cache-gc-design.md](cache-gc-design.md), [perf-instant-cache-plan-2026-06-11.md](perf-instant-cache-plan-2026-06-11.md), [VELNOR_PROJECTS_SETUP.md](../VELNOR_PROJECTS_SETUP.md)

This note maps jackin❯’s disk-hygiene research onto **Velnor the runner**, not onto jackin’s product prune UX. The bug class is the same: warm-path state grows without ownership, budgets, or automatic reclamation.

---

## 1. What the jackin research is really about

The July 2026 audit is not “rebuilds are slow.” It is:

| Failure mode | Observed effect |
|--------------|-----------------|
| Unowned build outputs | Host caches → hundreds of GiB |
| Shared / overlapping `CARGO_TARGET_DIR` | Concurrent cargo sessions × incremental dirs (3k+ sessions, tens of GiB) |
| Multiple agent-set target overrides | Near-identical full copies of the same crates (100+ GiB) |
| Compiler cache without size policy | `sccache` speeds rebuilds but **does not** shrink or dedupe `target/` |
| No doctor / prune surface | Operators delete directories by hand |

Design answer in jackin: **inventory + ownership + budgets + out-of-box compiler cache + prune/doctor + optional kache for content-addressed / reflink dedup**.

---

## 2. What Velnor already got right

Velnor already implements the **three-cache model** jackin proposes for containers, adapted to CI jobs:

| Jackin layer | Velnor today |
|--------------|--------------|
| Shared downloads (registry/git) | Host `_velnor_cargo/registry/{cache,index}` and `_velnor_cargo/git/db` mounted into every job (`HOME` truthful); mutable `registry/src` and `git/checkouts` stay job-local |
| Shared compiler results | Host `_velnor_sccache` → container `SCCACHE_DIR=/var/cache/sccache` |
| Scoped build outputs | Opt-in `VELNOR_CARGO_TARGET_PERSIST` → `_velnor_targets/<trust>/<repo>/<workflow>/<job-bucket>` |
| Trust isolation | Executable cargo/mise bins scoped by trust+repo; targets include trust scope |
| CI hygiene env (workflows) | Estate standard: `CARGO_INCREMENTAL=0` + `RUSTC_WRAPPER=sccache` |
| Cache GC design | [cache-gc-design.md](cache-gc-design.md) + `velnor-runner cache du` / `cache gc --dry-run` spike |

Instant-cache work (0.1.18+) fixed the structural “cache never saves” / HOME-lie class. Shared sccache across slots fixed the “10× duplicate sccache stores” class.

**Velnor is ahead of unhygienic local multi-agent setups** on registry + sccache sharing. The remaining risk is **long-lived host store growth on a multi-repo fleet**, not cold-start latency.

---

## 3. Gap analysis — improve Velnor here

### P0 — Stop unbounded growth on production fleets

| ID | Gap | Why it hurts | Improvement |
|----|-----|--------------|-------------|
| **H0.1** | **No default `SCCACHE_CACHE_SIZE`** (or equivalent) injected by the runner | sccache default is modest per process, but long-lived daemon-shared `_velnor_sccache` can still grow without operator policy; multi-repo fleets amplify writes | Inject `SCCACHE_CACHE_SIZE` (daemon env, default e.g. `20G`–`50G` tunable) on every job; document in `runner-usage.md` |
| **H0.2** | **Cache GC destructive path unfinished** | Design says: GC before disk-floor park; code still dry-run only; sccache marked “report only” | Ship reaper: lock + in-use scopes → GC targets/caches/artifacts/cargo ceilings → then disk check; optionally stop sccache and trim `SCCACHE_DIR` when over budget. **In-use set must survive the stuck-process class**: jackin's audit found ~23 GiB undeletable for hours while a `cargo test` deadlocked on PTY I/O held target files open — GC of a disposable bucket under an active slot waits only the drain timeout, then the supervisor reaps the process so files release; never block indefinitely on a hung job. |
| **H0.3** | **No operator-visible inventory** | jackin’s core lesson: every cache class needs owner + purpose + budget + cleanup command | Extend `velnor-runner doctor` / `cache du` with a fixed table of store classes, bytes, budget, and exact prune command |
| **H0.4** | **Disk-floor parks slots without reclaim** | Full disk → capacity death looks like “no runners” (stability class) | Always run GC (or soft prune) before `disk_space_problem` park |

### P1 — Make warm targets safe when enabled

| ID | Gap | Why it hurts | Improvement |
|----|-----|--------------|-------------|
| **H1.1** | `VELNOR_CARGO_TARGET_PERSIST` is powerful but **opt-in with weak lifetime policy** | Shared target buckets = cargo lock contention + residual incremental dirs if workflows forget `CARGO_INCREMENTAL=0` + unbounded deps growth | When persist is on: force `CARGO_INCREMENTAL=0` at container env layer (workflow may not set it); enforce keep-newest-N + age/byte ceilings from GC design |
| **H1.2** | **No path normalization for sccache** | Different workspace paths / slot layouts can reduce hits (`SCCACHE_BASEDIRS` missing) | Set `SCCACHE_BASEDIRS` to workspace + known mount roots (`/__w`, `/github/home`, …) so keys are portable across jobs |
| **H1.3** | **Job-end accounting missing** | Warm-run audits (jackin #810 style) need mechanical “was this warm?” signals | After compile-heavy jobs (or always): emit structured `sccache --show-stats` (and later kache report) into step log / forensic log; fail optional doctor mode on “should-be-warm but cold markers” |
| **H1.4** | **Target bucket key may be too coarse or too fine** | Coarse → lock + pollution; fine → many full trees (jackin multi-target explosion) | Document key formula; add metrics (bytes per bucket, hit rate); consider profile-aware buckets (`debug` vs `release`) without exploding cardinality |
| **H1.5** | **No automatic `CARGO_INCREMENTAL=0` from runner** | Workflows that omit it recreate the incremental session bomb **inside** persistent targets | Default job env: `CARGO_INCREMENTAL=0` unless operator overrides; document that incremental is hostile to shared targets |

### P2 — Compiler-cache product evolution (sccache vs kache)

Jackin recommendation: **kache for host/containers (dedup), sccache kept for CI until proven**. Velnor is itself the CI host. Translation:

| Topic | sccache (current Velnor) | kache (evaluate) | Velnor recommendation |
|-------|--------------------------|------------------|------------------------|
| Hit rate for same rustc inputs | Proven on estate | Needs soak | Keep sccache as default product path |
| Disk dedup across target buckets | **No** — each `_velnor_targets/...` full copy | Content-addressed store + hardlink/reflink restore | **High value** if multi-bucket persist is widely enabled |
| Linux job FS | Proven on Sentry XFS | Sentry has XFS reflink, but actual container mount path is unproven | Verify the real bind/overlay topology, not only host capability |
| Remote backends | GHA + many | S3-like | Fleet multi-host later: sccache multi-level or kache S3 |
| Maturity | High | Young (≈0.10, ~390★) | Trial behind feature flag, not flip default yet |
| Adapter surface | `mozilla-actions/sccache-action` native | Would need `kache-action` native if estate switches | Do not block on kache; design env surface (`RUSTC_WRAPPER`) to stay tool-agnostic |

**Concrete Velnor work for kache (optional track):**

1. Build the filesystem-wide disk controller first; kache only bounds itself.
2. Feature `VELNOR_RUSTC_WRAPPER=sccache|kache` (default sccache).
3. Bake a pinned stable version into a canary image; do not depend on the Node
   `kache-action` product path.
4. Resolve kache's documented warning about host cache bind mounts and SQLite
   WAL before sharing one store across concurrent job containers.
5. Soak 1/2/4/max slots, GC races, cancellation, restart and reboot on one
   trusted pool; verify physical reflink behavior through the actual mounts.
6. Keep sccache default until representative estate A/B measurements pass.

Both tools may be installed and different jobs/pools may select either one.
Do not nest both compiler caches around the same rustc invocation. Add one
runner-owned `CompilerCacheBackend` seam (`sccache | kache | off`) with separate
stores, budgets, leases, health and statistics. The estate workflow must avoid
explicit cache-CLI commands so Velnor's native setup adapter can select kache
while the identical GitHub-hosted lane continues to select sccache. Full design
and acceptance gates:
[storage-and-disk-pressure-2026-07-18.md](storage-and-disk-pressure-2026-07-18.md#supporting-both-compiler-caches).

**Do not** treat kache as a substitute for GC. jackin is explicit: even with kache, target dirs and stores need budgets and prune.

### P3 — Structural ownership model (port jackin’s program into Velnor ops)

Jackin’s five layers → Velnor equivalents:

| Jackin layer | Velnor equivalent |
|--------------|-------------------|
| 1. Inventory & ownership | Single table in `docs/` + `cache du` schema: path, creator, safe-delete rule, default budget |
| 2. Out-of-box compiler cache | Already: image sccache + mount + env; strengthen defaults (size, base dirs, incremental off) |
| 3. Bounded defaults | Daemon env: max sizes for sccache, cargo, mise, targets; refuse unbounded new store classes |
| 4. Prune & doctor | `velnor-runner cache gc` (destructive) + doctor section; apt package documents operator flow |
| 5. Automatic guardrails | Soft warn threshold in doctor timer; hard GC before park; never delete non-`_velnor_*` trees |

**Rejected defaults (align with jackin):**

- One global `CARGO_TARGET_DIR` for all repos → already rejected (trust/repo/workflow/job bucketing is correct).
- sccache alone as full answer → correct; Velnor still needs target/GC policy.
- Leaving size policy to operators only → fleets will fill disks; ship bounded defaults.

---

## 4. Velnor-specific threat model (differs from jackin local agents)

| jackin local | Velnor fleet |
|--------------|--------------|
| Many agent containers + host `target/` + macOS APFS | Many **job containers**, ephemeral workspace, **persistent host stores** |
| External tools invent `CARGO_TARGET_DIR` names | Workflows + optional persist buckets; different **repos/workflows** share one host |
| Operator laptop disk pain | **Slot park / zombie capacity** + multi-tenant trust isolation |
| Reflink wins on APFS | Production hosts usually **Linux ext4/xfs**; hardlink/copy matter more than CoW |

Implications:

1. **Hygiene is a stability feature**, not only a disk convenience (ties to P1.9 / never-silent-degradation).
2. **Trust scope must stay in every GC and store path** — never LRU-merge across `trusted` vs `public-forks`.
3. **Multi-repo sccache sharing is a feature** (same toolchain → hits) but needs **size caps** and maybe optional per-org partitions if noisy neighbors thrash the cache.
4. **Ephemeral job workspaces** already drop local `./target` unless persist is on — the dangerous growth is almost entirely under `_velnor_*`.

---

## 5. Recommended implementation sequence

### Phase A — defaults and visibility (small, high leverage)

1. Inject defaults on job containers:
   - `CARGO_INCREMENTAL=0` (unless already set)
   - `SCCACHE_CACHE_SIZE` from daemon env (default set, not unlimited)
   - `SCCACHE_BASEDIRS` covering `/__w` and job home roots
2. Expand `velnor-runner cache du` / doctor to print the ownership table + budgets.
3. Document operator knobs in `runner-usage.md`.

### Phase B — real GC

1. Implement daemon-shared GC lock.
2. Wire active-job in-use set from slot bookkeeping.
3. Enable destructive GC for `_velnor_targets`, `_velnor_caches`, `_velnor_artifacts`, coarse cargo/mise ceilings; optional sccache dir trim when over budget.
4. Order: GC → re-check free space → only then park on disk floor.
5. Forensic log + tracing spans for every deletion class.
6. **Reap stuck/zombie job processes before GC of their bucket.** A process that
   outlives slot bookkeeping (PTY-deadlocked `cargo test`, segfault-hung
   linker) keeps target files open and blocks unlink — the jackin audit's 23 GiB
   held-hostage class. The drain-timeout reap (already the SIGTERM path) must
   run to completion before a bucket is considered free, so GC never waits
   forever or skips a disposable bucket because a dead process still holds a
   file handle.

### Phase C — target-persist safety

1. When `VELNOR_CARGO_TARGET_PERSIST=true`, force incremental off and log bucket key.
2. Enforce keep-newest-N per scope (design already specified).
3. Optional: surface bucket size in job start log (“using 12.4 GiB target bucket …”).

### Phase D — kache evaluation (non-blocking)

1. Spike mount + wrapper env on a fixture lane.
2. Measure disk under multi-bucket compile vs sccache-only.
3. If clear win on Linux fleet disks, add adapter + image option; keep sccache default until soak passes.

---

## 6. Workflow / estate implications (not runner code, but required)

Even with perfect Velnor hygiene, workflows must not fight the model:

| Workflow rule | Reason |
|---------------|--------|
| Always `CARGO_INCREMENTAL=0` on CI | Matches jackin + sccache/kache requirements |
| Always sccache (or future kache) on compile jobs | Rerun-idempotency mandate |
| Do not invent extra `CARGO_TARGET_DIR` values per step | Multiplies trees; rely on Velnor bucket or default `./target` |
| Lane-scoped GHA cache keys | Avoid GitHub/Velnor cache corruption |
| Prefer one complete crate job over many profiles | Fewer target variants (jackin #810 semantic rule) |

Capture these in estate AGENTS / `VELNOR_PROJECTS_SETUP.md` when standardizing repos.

---

## 7. What not to copy blindly from jackin

| Jackin idea | Velnor stance |
|-------------|----------------|
| `jackin prune` / construct image APT kache | Different product; use `velnor-runner cache *` + job image |
| kache default on macOS host APFS | Velnor jobs are Linux containers; measure hardlink path first |
| CI keeps sccache while local uses kache | Velnor **is** CI — dual-tool only if fleet soak justifies it |
| Intercept external agent `CARGO_TARGET_DIR` | Out of scope; Velnor does not host Codex agents |

---

## 8. Success criteria

| Signal | Target |
|--------|--------|
| Host `_velnor_*` growth | Bounded by configured budgets; GC reclaims before slot park |
| Warm same-commit rerun | Still no registry download / dep compile wall (instant-cache preserved) |
| Doctor | Explains bytes by class + exact cleanup command without SSH archaeology |
| Target persist enabled | No multi-hundred-GiB single bucket; incremental session dirs absent |
| Tool switch | `RUSTC_WRAPPER` change does not require redesigning mounts |

---

## 9. Summary

The jackin research validates Velnor’s **shared registry downloads + shared compiler cache + scoped targets** architecture, and exposes the missing half: **hygiene**. Extracted Cargo sources and git checkouts are job-local because concurrent container materialization is not safe in one mutable tree.

**Highest-value Velnor improvements:**

1. **Bounded compiler cache + path normalization** (`SCCACHE_CACHE_SIZE`, `SCCACHE_BASEDIRS`).
2. **Finish cache GC** and run it before disk-floor degradation.
3. **Doctor/du inventory** for every `_velnor_*` class.
4. **Force `CARGO_INCREMENTAL=0`** when persistent targets (or always in CI containers).
5. **Evaluate kache** as an optional storage-layer win for multi-bucket fleets — after GC and budgets exist, not instead of them.

Root-cause class (matches Velnor operating principles): the architecture allowed **warmth without lifetime ownership**. Symptom patches (manual `rm -rf` on Sentry) must not remain the product path; the reaper + budgets remove the enabling condition.
