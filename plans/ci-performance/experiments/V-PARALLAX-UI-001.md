# V-PARALLAX-UI-001: preserve absent abort options at fetch boundaries

Status: focused fix implemented; independent review and real CI validation
pending. No CI or performance claim is made.

## Root cause

Parallax UI enables TypeScript `exactOptionalPropertyTypes`. The loaders accept
an optional `AbortSignal`, then forward `{ signal }` directly to `fetch`. When
the argument is absent, that object still has an own `signal` property whose
value is `undefined`. `RequestInit.signal` permits an omitted property (or a
real signal), but does not permit an explicit `undefined` under this compiler
mode, producing TS2769. The same boundary shape exists in the app-health and
auth-status loaders.

This is an optional-property construction error, not an abort API error. The
enabling condition is forwarding an optional value into a strict request
object without normalizing presence. Existing GraphQL transport and widget
request constructors already assign `requestInit.signal` only when a signal
exists; dashboard navigation uses a conditional options object.

## Alternatives

1. **Preferred, minimal:** construct `RequestInit`/fetch options without a
   `signal` key, then add the exact signal only when `signal !== undefined`.
   This preserves the supplied signal and makes absence observable as absence.
2. Use a small shared optional-signal options helper and call it from both
   loaders. This centralizes the invariant but expands the change beyond two
   simple constructors without removing another enabling condition.
3. Change `exactOptionalPropertyTypes`, cast `{ signal }` to `RequestInit`, or
   make the request type accept explicit `undefined`. These suppress or widen
   the boundary and can send a present-but-undefined property, so they are
   rejected.

## Acceptance

The two loaders must pass an unchanged `AbortSignal` when provided and omit the
`signal` property when absent. Validation is the existing pinned Bun UI
`typecheck` and `test` commands; no generated workflow, compiler, fixture, or
unrelated UI change is included.

## Focused result

`loadAppStatus` and `loadAuthStatus` now construct an empty `RequestInit` and
assign `signal` only when it is defined. The source diff is limited to those
two loaders. With Bun `1.3.14`, `bun run typecheck` passed; the full UI suite
passed 157 files and 691 tests. Native and type-aware Oxlint passed on both
changed files. The repository-wide formatter check still reports the existing
status-file trailing-comma issue plus unrelated pre-existing files; no
unrelated formatting was included.

## Independent review and propagation

Parent reviewed the two constructors against strict optional-property semantics.
Pinned Bun 1.3.14 typecheck and existing auth tests (2/2) independently pass.
Signed commit `dcf1d08c` pushed to Parallax campaign branch; real CI pending.
This correctness repair is not yet counted as a completed optimization iteration.
