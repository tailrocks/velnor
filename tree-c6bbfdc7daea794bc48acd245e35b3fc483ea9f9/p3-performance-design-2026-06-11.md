# Velnor-side performance design (P3 input) — 2026-06-11

Source: pipeline-structure analysis over the six estate workflows with
measured step timings from real Jun 10–11 runs (Velnor lane identified by
`velnor-*-slot-N` runner names) plus live sentry host inspection. GitHub
workflow YAML stays as-is (drop-in mandate); every win below is implemented
inside the runner.

## Measured Velnor-lane baseline

| Pipeline | Wall | Dominant measured costs |
|---|---|---|
| jm rust.yml (10 slots) | jobs 2m55s–5m18s, run ~6m04s | Tests 3m51s–5m04s; Clippy 2m04s; 10–16s pre-first-step gap; 34s trailing teardown gap |
| jm rust-docker.yml | 6m15s | bake 5m56s — `type=gha` layer cache round-trips over WAN |
| jm ansible.yml | 36s | 6s checkout+boot; 10s ansible-galaxy (cache ineffective) |
| bcn build-publish | 23s/job ×2 (GitHub lane: 8s) | 11–12s rust-script recompile per job (cache restores to wrong place) |
| brown ci | 74s | 32s build; 30s trailing teardown gap |
| fixture compat | 32s (GitHub: 21s) | 18s MSRV rustup download every job (`/root/.rustup` ephemeral) |

Pickup latency already ~2s (broker long-poll). Losses are state re-download,
per-slot cache fragmentation, WAN buildx cache, network checkout, synchronous
teardown.

## Cache-layer defects found (fix with #1/#8)

1. **Per-slot, not per-host stores**: `cache_store_dir`/`sccache_host`
   resolve under `work/slot-N` — sentry holds 10 × ~2 GB duplicate sccache
   dirs; empty slots always cold-miss (executor.rs `cache_store_dir`,
   container.rs `sccache_host`).
2. **Registry cache silent no-op**: no `cargo-registry-*` entries exist in
   any slot store despite the actions/cache step running (restore+save 0s).
3. **`hashFiles()` evaluating empty** in cache keys (`rust-script-…-` with a
   trailing dash) → key collapse → first save wins, stale forever.
4. **Container-internal paths restore to the HOST**: `resolve_host_path`
   maps `/root/.cache/...` to host `/root` — invisible to the job container
   (bcn's 11s recompile is exactly this).
5. Restore/save is full byte copy; sentry root is XFS → reflink/`FICLONE`
   would make it O(metadata).

## Ranked improvements

| # | Idea | Saving | Effort |
|---|---|---|---|
| 1 | Host-shared sccache + cache store (kill per-slot fragmentation) | rust.yml Tests 4–5m → 1.5–2.5m | S/M |
| 2 | Persistent per-repo volumes: CARGO_HOME registry+git, RUSTUP_HOME, /opt/mise, RUNNER_TOOL_CACHE | fixture −18s, bcn −11s/job, brown −5s, jm −10–30s/job | M |
| 3 | Persistent per-(repo×job) cargo target volume | warm rust.yml Tests → 40–80s (−3m/job) | M/L |
| 4 | buildx `type=gha` → host-local cache substitution on Velnor lane | bake 5m56s → 1.5–3m | M (=P3.7) |
| 5 | Per-repo bare git mirror on host (delta fetch, local clone) | checkouts 5–16s → <1s | M |
| 6 | Async job finalization (complete BEFORE docker rm/network rm/dir removal/log zip) | 2–8s typical, 30–34s contended | S/M |
| 7 | Per-slot persistent docker network + container pre-create at acquire (boot ∥ checkout) | 2–4s/job + removes contended iptables churn | M |
| 8 | Cache adapter correctness: reflink restore/save, container-path restore, fix empty hashFiles + registry save no-op | restores intended caching everywhere | M |
| 9 | Overlap JIT re-registration with completion (slot turnaround) | 1–3s/job | S |
| 10 | Log pipeline batching (100ms frames) + precomputed masks | CPU headroom on saturated host | S (=P3.2) |

Verdicts on candidate mechanisms: git mirror CONFIRMED (rank 5); persistent
volumes CONFIRMED top (the actions/cache layer they replace is broken today —
keep the cache steps reporting normally so YAML/UI stay identical); container
pre-warm CONFIRMED moderate (rank 7; image is local, win is boot∥checkout);
**speculative reordering of user steps REFUTED** (shared cargo locks,
GITHUB_ENV accumulation, failure short-circuit — the safe form of "early
work" is resource pre-warming); action-repo pre-fetch LOW (adapters already
native); log streaming SMALL (rank 10).

## Projected totals (items 1–8, warm host)

| Pipeline | Today | After | Δ |
|---|---|---|---|
| jm rust.yml | ~6m00s | ~2m00–2m30s | −60% |
| jm rust-docker.yml | ~6m30s | ~2–3m | −55–70% |
| jm ansible.yml | 36s | ~18–20s | −45% |
| bcn build-publish | 2×23s | 2×~9s | −55% |
| brown ci | 74s | ~35–40s | −47% |
| fixture compat | 32s | ~12–14s | −60% |

Sequencing: 1, 6, 9, 10 are independent quick wins; 2→3 share a persistent
volume-manager primitive (per-repo keyed mounts + flock + LRU reaper); 5 and
7 touch the job-start path together; 8 lands with 1.
