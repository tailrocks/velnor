# Strict Capability and Local Compiler-Cache Contract

Status: accepted direction; manifest/ref/input gates implemented by plan 033
Date: 2026-07-18

## Law

Velnor supports only behavior declared in its versioned Rust capability
manifest. A received job is validated in full before checkout, cache mutation,
service startup, or job-container creation. If an action, immutable ref, input,
value, expression shape, backend, or runtime behavior is outside the manifest,
Velnor fails immediately with a precise unsupported-capability error. It never
falls through, approximates, silently ignores an input, or pretends compatibility.

New capabilities require explicit operator approval after their exact surface,
reason, trust implications, storage/network effects, and fixture proof are
documented. A root-cause fix that makes an already-approved capability conform
does not expand the surface.

## Current architectural gaps

Plan 033 closed the first product-path enforcement gaps:

- native adapters are selected by repository name without enforcing an approved
  action ref **(closed: plan 033)**;
- the native sccache adapter ignores invocation inputs **(closed: plan 033)**;
- unknown JavaScript actions may execute in a Node sidecar **(closed: plan 033)**;
- input allowlists and allowed-value schemas are not centralized
  **(closed: plan 033)**.

These are enabling structures for silent divergence. Replace repository-name
dispatch with a typed, versioned manifest and remove unknown-action fallback
from the product path.

## Validation boundary and manifest

GitHub sends Velnor an expanded job message. Before side effects Velnor validates
job containers, services, options, shells, expressions, every `uses:` repository
and immutable commit, all provided inputs and values, companion environment,
trust requirements, mounts, network, stores, setup/main/post behavior and outputs.

Capability-affecting inputs must be literal or statically resolvable during
preflight. A runtime-dependent value is rejected unless the manifest declares a
safe validation point before the affected action performs a side effect.
Arbitrary scripts cannot be understood statically; the strict guarantee covers
runner-managed job/action capabilities, while isolation and trust policy govern
script behavior.

The compiled Rust manifest is also exportable as JSON. Each entry declares:

```text
repository + allowed immutable commits
  -> native Rust adapter
  -> allowed input names, types, literal values and defaults
  -> prohibited combinations and accepted environment
  -> trust, mounts, network and persistent-store effects
  -> setup/main/post outputs and failure semantics
  -> fixture cases and upstream source commit
```

`velnor-runner capabilities check <job-dump>` reports all violations;
`capabilities export` lets estate CI audit workflows before merge. Errors name
the step, action/ref, field, received value, accepted alternatives, reason, and
manifest version.

## Approved compiler-cache topology

Both tools support local caching. Sccache defaults to local disk when no remote
backend is enabled. Kache supports a local content-addressed store; its Action
must set `github-cache: false` to prevent GitHub cache transfer. No S3, GCS,
Azure, GitHub cache, remote daemon, or other remote compiler-cache backend is in
the approved Velnor surface.

Velnor mounts one selected persistent store:

```text
/var/cache/velnor/v1/<trust>/compiler/sccache
/var/cache/velnor/v1/<trust>/compiler/kache
```

Exactly one of `sccache | kache | off` is selected. Both actions in one job,
nested wrappers, remote backends, or conflicting cache environment are preflight
errors. Stores have separate leases and equal experiment budgets.

## Exact proposed configurations

Verified against upstream releases on 2026-07-18.

### Sccache local

```yaml
env:
  CARGO_INCREMENTAL: "0"
  RUSTC_WRAPPER: sccache
  SCCACHE_CACHE_SIZE: 20G
  SCCACHE_GHA_ENABLED: "false"

steps:
  - name: Set up local sccache
    uses: mozilla-actions/sccache-action@1583d6b38d7be47f593cb472781bbb21cab4321e # v0.0.10
    with:
      version: v0.16.0
      disable_annotations: "false"
```

Only `version=v0.16.0` and `disable_annotations=false` may be explicitly
provided. `token` may use the upstream implicit default on GitHub-hosted but may
not be supplied. On Velnor the pinned image binary is used. All remote/multi-level
sccache environment variables are rejected; Velnor owns `SCCACHE_DIR`.

### Kache local

```yaml
env:
  CARGO_INCREMENTAL: "0"

steps:
  - name: Set up local Kache
    uses: kunobi-ninja/kache-action@49398d37113c616fdb61be434cb497e3c2c8f3e6 # v1
    with:
      version: v0.10.0
      github-cache: "false"
      cache-executables: "false"
      pr-comment: "false"
      max-size: 20GiB
```

Only those five inputs at those exact values are allowed. Reject every other
input, including all `s3-*`, `sync`, `warm`, `manifest-key`, `namespace`,
`min-compile-ms`, `cache-key-prefix`, and explicit `token`. The native Rust
adapter sets `RUSTC_WRAPPER=kache`, mounts the local store, and provides local
setup/post reporting without executing Node or `@actions/cache`.

The equal 20 GiB ceiling is for fair initial measurement, not eternal tuning.
Changing it versions the manifest. The filesystem controller may reclaim an
inactive store below its backend ceiling under pressure.

## Standard estate use

Production workflows select one backend and contain one setup action. Sccache
remains the initial default because it is proven. A comparison fixture and small
representative canary use literal `off`, `sccache`, and `kache` jobs with two
separately pinned conditional action steps; both never run in one job.

The GitHub-hosted lane uses the same local-only configuration. Its store lasts
only for that job, so this is a compatibility/cold-baseline lane, not cross-run
persistence. GitHub-cache support is a separate feature requiring explicit
approval. Workflow build steps do not call cache-specific reporting CLIs; the
action post step owns reporting.

## Implementation and proof gates

1. Add typed ref/input/value validation before every execution side effect.
   **Implemented by plan 033.** Remaining job/container/expression dimensions
   extend the same manifest in subsequent plans.
2. Remove unknown-action sidecar fallback from the product path.
   **Implemented by plan 033;** both diagnostic flags are required to reach it.
3. Make every native adapter declare and test its exact surface; an ignored
   provided input is a failure. **Implemented by plan 033.** Surface changes
   require a manifest version bump.
4. Add native Rust Kache setup/post/store/report support and refactor sccache
   through the common backend seam.
5. Bake both pinned binaries into the Ubuntu image; never compile during a job.
6. Test every allowed value and every rejected input, value, ref, remote env,
   expression, dual-wrapper combination, and store override.
7. Audit the estate with `capabilities check`; propose missing features instead
   of silently expanding support.
8. Run the matched local and disk-pressure experiments in
   [storage-and-disk-pressure-2026-07-18.md](storage-and-disk-pressure-2026-07-18.md).
