# Complete Node/Bun package inputs

Date: 2026-09-20

## Root cause and intervention

Both scanners encoded an extension whitelist despite arbitrary package scripts
and bundler inputs. Unknown assets or configuration could therefore miss the
package, falling back to unrelated work or incorrectly omitting obligations.
The generator now watches each complete package directory; nested packages retain
their own boundary and scoped manifest/lock metadata. Explicit watch overrides
remain responsible for declaring a complete input closure.

## Independent review

Reviewer: `/root/jackin_inventory`, isolated clean `845d474`.
Reviewed patch SHA-256:
`576ac4d13d59cb1a39404cf39df62965fcef995a5e1e52db09290515a007ca76`.
Scanner package tests, opaque rename/delete selection, both watch helpers,
formatting and strict Clippy passed. Review found an existing S2 test still
expecting extension globs. Updating exactly those two assertions to `**` and
`ui/**` passed the focused watch tests. The parent integrated that correction
over `4f70cf74`, retaining current typed tool-boundary changes.

An archived-tree full-suite attempt lacked Git metadata and failed unrelated
identity/runtime fixtures; it is not acceptance evidence. Parent validation
uses the real integration worktree with full history.

This is selection correctness work. No speedup, iteration, or plateau credit
is assigned before controlled execution and independent result verification.
