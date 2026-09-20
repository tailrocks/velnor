# V-WATCH-001: preserve scanner-derived Bun and Docker inputs

Status: **candidate parser correction implemented; awaiting parent integration and generated-consumer review**.

## Root cause

`crates/velnor-workflow/src/s2/primitives/watch.rs` treats Bun, Docs,
OpenTofu, and root Docker as `derived` units. The derived branch starts with
an empty set, so a scanner-produced `Unit.watch` list is discarded. Bun then
adds hardcoded root paths (`src/**` and `scripts/**`), which are wrong for a
nested package such as Parallax's `ui` unit and omit valid package files such
as `ui/codegen.ts` when a source glob is narrower. The correction preserves
those two paths for a root Bun package because they are existing required
obligations, while retaining the scanner closure for assets and generated
inputs. Docker separately adds Cargo, Rust toolchain, and mise paths for every Docker unit. The root
`docker_watch_paths` helper repeats those Rust defaults even when the
Dockerfile never copies or uses them. This is a primitive boundary error: the
scanner already owns repository facts, while the primitive replaces them with
kind assumptions.

Observed migration differences: Parallax schema 1 watched
`ui/**/*.ts/tsx/js/jsx/json` and package locks; schema 2 emitted root `src/**`
and `scripts/**`. Maple's prior Docker watch set was its own source tree;
schema 2 added global Cargo/mise/toolchain paths. The changes can schedule
irrelevant expensive jobs and can miss a real UI edit.

## Remedies challenged

1. **Primitive correction (selected first experiment).** Start every unit from
   its scanner-derived watch facts, retain workspace broad-watch filtering,
   keep the two existing root-Bun source families until the scanner proves
   their closure, and add only package-manager files that are true universal
   inputs. Restrict Rust defaults to Rust units. For
   Docker, retain Dockerfile/`.dockerignore`/explicit `COPY` and `ADD` sources,
   dependency-closure roots, and declared named BuildKit context paths; remove
   unconditional Rust inputs. Add fixtures for root/nested Bun, non-Rust Docker,
   Rust Docker, named contexts, and negative unrelated edits. This is small and
   preserves all currently inferred paths.
2. **Typed input provenance.** Extend the scanner shape with typed input facts
   (`source`, `manifest`, `lock`, `docker-context`, `toolchain`) and have the
   primitive merge facts by role. This removes future kind-specific path
   guesses but is a larger schema/runtime migration; it is a follow-up if the
   first experiment exposes ambiguous ownership.
3. **Build-context graph compiler.** Parse Dockerfile `COPY`/`ADD`, named
   `--build-context` declarations, `.dockerignore`, and command-selected
   contexts into a Docker product closure. Reject dynamic or broad sources
   unless a typed declaration supplies their complete closure. This gives the
   strongest Docker correctness but cannot replace scanner preservation and
   requires source/config changes for context declarations.

The first experiment must not prefix the current Bun defaults onto nested
packages and must not add repository-specific watch overrides. The scanner's
path facts are the source of truth; root `src/**` and `scripts/**` remain as
explicit baseline obligations until scanner ownership covers all root Bun
inputs. A Docker context path is an input only when
the generated command declares or the Dockerfile consumes that context; the
mutable cache seed is excluded from relevance because it is performance state,
not source.

## Validation

Before accepting the fix, compare generated watch plans for docs-only, nested
Bun source/config/codegen, root Bun source, isolated Rust, shared Rust, a
Dockerfile edit, a Docker-context edit, `.dockerignore`, a Rust Docker image
source edit, and an unrelated global Cargo/toolchain edit. Record selected
units and reasons. Validate that no required unit loses a scanner watch and
that a non-Rust Docker unit no longer inherits Rust-only inputs.

## Candidate evidence

The candidate changes only the S2 watch primitive. Bun units retain the
scanner's source closure and receive package-manager/config metadata rooted at
the unit; Docker units retain scanner inputs and receive only declared context
roots. Root Docker parsing now handles local stages and named contexts, and
falls back to `**` with an explicit diagnostic for dynamic or incomplete
`COPY`/`ADD` closure instead of silently under-selecting. Rust-wide defaults
are no longer injected into Docker units.

The candidate preserves scanner paths for nested and root Bun units, retains
the root `src/**`/`scripts/**` obligations, and roots `tsconfig.json` and
package-manager files at the package. Its Docker parser
normalizes absolute context paths, joins continuation lines, inspects
`RUN --mount=type=bind`, recognizes local stages and named contexts, and falls
back to `**` when a source is dynamic, missing, or syntactically incomplete.
Cache-seed mounts remain performance state and do not affect source relevance.

The parser's supported mount grammar is deliberately static: unquoted
`--mount=type=bind[,source|src=...][,from=...],target=...` with the documented
static option forms, plus known non-source `cache`, `tmpfs`, `secret`, and
`ssh` mounts. Quotes, interpolation, duplicate options, unknown options, and
malformed fields select the complete context. Docker comments are recognized
only on full logical lines; an inline `#` remains a possible source filename,
and a comment-only line inside a continued instruction is skipped without
terminating that instruction.

Parser directives now follow Docker's case-insensitive key and whitespace
rules for `escape`; invalid or unknown top-of-file directives, unsupported
syntax frontends, and duplicate directives force the full-context fallback.
`ONBUILD COPY` and `ONBUILD RUN --mount` are inspected as nested instructions,
so a source copied by a local stage that a later `FROM` consumes remains in
the image's watch closure. See the [Dockerfile parser-directive
reference](https://docs.docker.com/reference/dockerfile/#parser-directives)
for the builder semantics this parser mirrors.

In the shared checkout, the focused watch suite passed 16 tests (1,790
filtered). New fixtures cover inline-hash COPY sources, comment-only
continuation lines, quoted and interpolated mounts, and a planning-level
selection where a Docker source is also watched by a peer unit; the selection
must retain the Docker image instead of letting the peer's match mask it.
Additional fixtures cover case/whitespace-tolerant escape directives,
unknown/invalid directive fallback, and ONBUILD COPY from a local stage.
`rustfmt` and `git diff --check` pass for the touched file. Full-workspace
formatting remains coupled to other parent edits; parent must rerun the full
library/Clippy and generated-consumer checks in the clean integration checkout.
