# G1 scan-integrity structural-design review

Decision: **CONDITIONAL REJECT — feasible direction, not implementation-ready**.

Input: `structural-design.md`, SHA-256
`afeb5adde9b8a4bb8ef1f853a1a87e5f84307ab0f715a9bbc1f8d6dfdf975232`.
The design was read in full. This review is read-only; no source, generated
file, host, or Docker change occurred. It makes no G1 gate claim.

## What passes

- Correctly rejects sidecar/header/path/hash self-authorization
  (`structural-design.md:30-72`).
- Correctly separates scan visibility from mutation authority and keeps
  modified/unknown files unwriteable, including under `--force`
  (`:74-106`, `:121-133`).
- Explicit no-baseline behavior is fail-closed for stale deletion, adoption,
  and proof-bearing sidecar refresh (`:61-68`, `:110-116`).
- Positive/negative integrated fixtures cover current exact, config drift,
  current regeneration, stale removal, forged content/digest, forged paths and
  headers, no baseline, races, and symlink/traversal cases (`:139-185`).
- It preserves the required implementation gates: supported primitive,
  parent config-drift behavior, release snapshot, fmt/diff/clippy, full frozen
  SHA review (`:187-198`).

## Must-fix design rows

### SD-1 — specify actual protected-base acquisition, not only its name

`TrustedGenerationBaseline` is the right shape (`:37-48`), but source 1 is
still an assertion: “protected base Git commit/object set” and “validated by
base policy/D19” do not say how a trusted caller obtains or proves it
(`:50-59`). A PR-controlled `[generator]` value, sidecar, branch name, or
current `HEAD` must not select the baseline.

Before code, add a concrete `ProtectedBaseApi` acquisition contract:

1. Trusted policy context supplies repository identity and base ref; never read
   these from PR config/worktree.
2. Read the protected base tip through fixed GitHub API calls: `GET
   /repos/{owner}/{repo}/git/ref/heads/{base}`, then the returned commit,
   tree, and required blobs through `/git/commits/{sha}`,
   `/git/trees/{sha}?recursive=1`, and `/git/blobs/{sha}`. GraphQL is
   acceptable only if its operation, variables, cursors, raw hashes, viewer
   identity, and terminal page are retained. Capture the full commit/tree IDs
   and raw response references for the baseline, generator pin, config, and
   every typed output.
3. Independently read `GET /repos/{owner}/{repo}/branches/{base}/protection`
   and the repository ruleset list/details, then prove that the base
   policy/D19 validator is the required protected producer. A 404, missing App,
   unavailable ruleset, 401/403, pagination error, or transport failure is
   `baseline_unavailable`, never “protected.”
4. Validate the pinned generator revision/source closure from the protected
   base object set, then regenerate/compare the baseline bytes. Do not accept a
   revision merely because the mutable tree names it.
5. Re-read the base ref after acquisition. Any movement invalidates the
   baseline and all derived plans.

The implementation handoff must name exact API calls/records and the
fail-closed error states. The published-release option (`:55-56`) is not
available evidence merely because an “existing product/attestation boundary”
is mentioned; either identify its immutable published baseline/path artifact
or remove that option from this implementation.

### SD-2 — define local CLI ergonomics and lifecycle

The design permits an explicit operator baseline (`:57-59`) but does not say
how the current CLI acquires it. Without a concrete path, a local generator
becomes unusable whenever config changes or a stale output must be removed.
Before code, specify one external CLI mode, for example a full-object
`--baseline-revision <sha>` (or an equivalently explicit operator mode), and
report `protected-base-api`, `operator-commit`, or `none` in diagnostics. The
baseline must come from CLI/trusted caller state, never config or sidecar.

Required behavior:

- First generation with no baseline may render current exact outputs, but may
  not claim stale ownership. Existing foreign files remain visible/conflicting.
- Repeated local generation with unchanged current outputs must remain
  usable without committing a sidecar-only trust claim.
- A config/source/generator change or stale deletion without a baseline must
  return a bounded “baseline unavailable; supply immutable revision” result,
  with no mutation. It must not silently fail every ordinary current-exact
  generation.
- `--force` remains operator authorization only. It cannot turn `none` into a
  trusted baseline, cannot bless unknown bytes, and cannot bypass a stale
  preimage mismatch.
- Document the intentional breaking change from the old broad “replace
  unowned workflows” behavior. No compatibility shim or implicit adoption.

Add CLI-level tests for first run, repeated run, config drift with `HEAD`
baseline, missing baseline, and explicit stale removal. Helper-only tests are
insufficient.

### SD-3 — resolve raw-audit versus detector-input ordering

The class table keeps `GeneratedCurrentPriorExact` and `GeneratedStaleExact`
visible in the “raw audit” and allows only `GeneratedCurrentExact` into the
later exclusion pass (`:80-93`). That is safe against self-metadata, but the
design does not define whether generated old workflow/action bytes can alter
the detector shape before SI-P3/SI-P4 rendering. If they do, intentional
regeneration or stale removal can become circular or impossible.

Define two explicit views:

- `raw_inventory`: all files, including trusted prior/stale outputs, retained
  for audit and diagnostics;
- `detector_inputs`: after immutable baseline proof, exclude exact generated
  current/prior/stale outputs from capability inference. This exclusion is
  classification-based, not sidecar-based, and still grants no mutation
  authority.

If the owner intentionally keeps prior/stale bytes in detector inputs, SI-P3
and SI-P4 must prove the actual scanner cannot be influenced by those bytes for
every workflow/action detector. Do not leave “raw audit” versus “scan” as a
terminology-only distinction.

### SD-4 — make force semantics and ownership classes exact

The intended rule is sound: `ModifiedGenerated` and `ForeignOrUnknown` are
never overwritten/deleted, including with `--force` (`:80-86`, `:125-129`).
Make the fixture wording match the classes. In SI-N1 (`:168-172`), an
arbitrary untracked composite action may be `ForeignOrUnknown`, not
`ModifiedGenerated`, unless the trusted/current typed output contract actually
contains that path. Assert the class per path.

Specify SI-P3's “explicit generation” (`:155-159`): whether `--force` is
required for a current-prior exact replacement, what the plan prints, and that
the old exact preimage—not the sidecar digest—is the proof. Specify SI-P4's
`--force` stale deletion (`:160-164`) likewise.

## Required acceptance evidence

The design's fixture list is adequate only if the implementation records:

- protected-base API proof and post-acquisition ref equality for the baseline;
- local CLI mode and source identity in every result;
- SI-P2 config drift reporting before obsolete primitive/render errors, with no
  sidecar or output mutation on failure;
- SI-P3 replacement of exact trusted prior bytes and refusal after editing;
- SI-P4 integrated stale deletion from exact trusted baseline bytes, with both
  normal and forced refusal after editing;
- SI-N1 path-specific `ModifiedGenerated` versus `ForeignOrUnknown` classes,
  forged digest ignored, body preserved, and no force bypass;
- SI-N3 no-baseline behavior and explicit no sidecar proof refresh;
- full-suite, release snapshot, fmt/diff, clippy, and hostile matrix on one
  immutable candidate SHA.

Until SD-1 through SD-4 are amended, do not start source implementation and do
not approve G1. The design is structurally promising, but the missing
baseline acquisition protocol and local lifecycle are material correctness
gaps, not documentation polish.
