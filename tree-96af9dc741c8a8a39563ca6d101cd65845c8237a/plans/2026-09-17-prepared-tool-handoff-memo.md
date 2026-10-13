# Prepared-tool handoff: design-challenge memo

Date: 2026-09-17. Area: `velnor-workflow` verified prepared-tool handoff.
Base: HEAD `33688938`. This memo is written before the implementation.

## Challenge 1 — Is the requested handoff generic?

Yes, if it reasons about shapes and never about names. The crate rule is
absolute: no estate name, path, pin, or consumer list may enter
`velnor-workflow`. So the handoff is defined over this shape only:

- a **tool bundle**: one tool id, one inputs digest, one platform ABI,
  one producer identity, one outcome, a file list with digests;
- a **request**: the tool id, inputs digest, and platform ABI a consumer
  job needs, plus the authorized-producer set and the current run id;
- a **resolution**: exact current-run output preferred; otherwise the
  newest historical bundle that fully validates — with the requested
  identity and the resolved identity carried as two distinct values.

Nothing in that shape names a repository, a lane, a tool, or a backend.
Lockfile discovery takes the scan's file list plus a unit root and finds
governing locks by walking ancestors; filenames (`mise.lock`,
`Cargo.lock`, …) are kind vocabulary, never paths. Any repository's
prepared tools — prebuilt test runners, policy binaries, linkers — fit
the same verifier.

## Challenge 2 — What is already supported?

Most of the pattern, none of the generality:

- `primitives/runtime_products.rs`: owner-only immutable binaries with
  attestation, manifest, digest, and self-report gates, never-overwrite
  publish, and a smoke test of the exact consumer flow. The handoff
  reuses the manifest-first shape (digest decides reuse; any
  self-report only confirms) but must not inherit the owner-only
  routing: prepared tools are per-repository producer/consumer pairs.
- `closure.rs`: canonical digest over content plus a footer. The
  handoff's inputs digest follows the same canonical-bytes discipline.
- `primitives/snapshot.rs`: compatibility digest (schema, payload,
  toolchain, image, linker, flags, cargo inputs, recipe) kept distinct
  from freshness (`hashFiles` segments); retention with per-class
  budgets, generation bounds, and producer-activity eligibility. The
  handoff reuses the compat/freshness split for its keys
  (`snapshot_class_prefix`-style class + runtime segments) and gains a
  retention class so cached bundles are bounded like any rolling state.
- `primitives/cache.rs`: lexical path validation and host-persistence
  classification. The handoff's extraction gate reuses the same
  lexical discipline (no `..`, no absolute paths, no globs).
- `workflow_runtime_download` / `workflow_runtime_artifact_upload`
  (`lib.rs`): the Planning artifact handoff already proves the exact
  shape — manifest fields bound to revision, repository, platform, and
  run id, digests verified before install, PATH never trusted. The
  prepared-tool handoff generalizes that pair from one hardcoded
  product to any tool bundle.
- `hosted_cargo_bin_toolchain_*`: restore/verify/save with a stale-hit
  repair gate. The handoff keeps the repair instinct (verify after
  restore) but replaces `--version` probing with manifest + digest
  proof.
- `config/mod.rs` mise lock handling: strict TOML key parsing and
  shape-before-membership validation. The handoff extends it to nested
  locks; the `mise_lock_keys_for_root` doc comment already names the
  per-unit gap.

What is genuinely missing: a single verified-identity type joining
request, manifest, bytes, and install; requested-vs-resolved key
discipline; a failure taxonomy; bounded transfer; ancestor lockfile
discovery; atomic install.

## Challenge 3 — What bug class is removed, structurally?

The enabling condition today: identity, transport, and install are
decided in three places with no shared type. The downloader accepts by
name (and at best expiry); the cache stores whatever bytes arrived
under whatever key was requested; the installer trusts the directory
it finds. Each historical defect is one facet of that split:

- name-only acceptance → the verifier requires tool id + inputs
  digest + platform ABI + authorized producer + success outcome +
  manifest/byte integrity, all-or-nothing;
- fallback bytes saved under the requested exact key → save keys are
  derived from the bundle's own producer identity, and
  `save_is_legal` refuses any save whose key names a run the manifest
  does not; requested and resolved keys are separate fields that
  tests pin apart;
- unbounded waits/retries/pagination → one `TransferBounds` value
  (attempts, pages, wait) threads through every transfer loop, and
  exceeding a bound is a typed `Transient` failure, never a hang;
- hardcoded lock paths → discovery over scan output; a unit is
  governed by its nearest ancestor lock per kind, so workspace,
  standalone, and nested layouts resolve without literals;
- traversal/stale/partial install → the installer validates every
  manifest path lexically, requires every listed file with matching
  digest, stages into a fresh temp dir, and renames atomically; the
  destination is never mutated in place and never partially satisfied.

The structural fix is that no path from "bytes arrived" to "tool on
PATH" bypasses the verified type: resolve, validate, and install are
one function chain over one manifest, and the failure taxonomy (miss
vs corrupt vs denied vs transient) forces callers to handle each
outcome explicitly instead of collapsing them into "retry" or
"rebuild".

## Deliberate non-goals

- No new network client semantics are invented here; transfer bounds
  are pure values the existing transports (cache steps, artifact
  download) render as literals. A follow-up renders consumer steps
  from this core; this change keeps generated bytes identical (no
  `GENERATOR_REVISION` bump).
- No per-repository policy: the authorized-producer set comes from
  the repo-owned generation config, validated as slugs.
- Retention gains one class and marker for the new key namespace;
  existing classes, budgets, and goldens are untouched.
