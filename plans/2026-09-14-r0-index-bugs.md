# Plan 2026-09-14: indexing BUGs + hardening (r0-index-bugs)

Base: origin/main tip 05a23ed2 at fetch time. Branch: r0-index-bugs.
Worktree: /tmp/velnor-idx.

## Problem

2 indexing BUGs plus 3 hardening notes, all panic-on-untrusted-input
shapes:

1. MEDIUM `runner/docker/engine.rs` `decode_chunked`: `data_end + 2`
   overflowed when a daemon-sent chunk size landed `data_end`
   (`cursor + size`) at `usize::MAX - 1`. Debug panics on the `+`;
   release wraps and panics on the out-of-bounds slice.
2. LOW `tools/main.rs` `find_hardcoded_lane_strings`: the 30-byte
   context window `&cleaned[start..end]` split multi-byte chars.
3. `runner/expression/parser.rs` `push_operand`: `unreachable!` on a
   non-operand `TokenKind` — a future variant panics the parse.
4. `runner/docker/client.rs` `container_rm_args_with_claimed_ids`:
   `pub(crate)` helper indexed `args[0]` with no empty guard.
5. `runner/execution/unix_api.rs` `read_http_response`:
   `header_end + 4 + content_length` overflowed on a huge
   peer-sent Content-Length.

## Fix (this branch, strict scope)

1. `checked_add` for the trailing-CRLF offset (`after_data`); overflow
   is `EngineFaultKind::Framing`, which the facade answers with its CLI
   fallback. The cursor advance reuses `after_data`.
2. Window edges floor/ceil onto `is_char_boundary`.
3. The `unreachable!` arm becomes `ParseError::internal` (fail closed).
4. Same-scope `let Some(first) = args.first() else return ids.to_vec()`.
   Production callers already only pass a non-empty `rm` argv (a claim
   exists only when `args.first()` is `Some("rm")`).
5. `checked_add` chain; overflow is a bounded
   "content-length overflows the response bound" error.

One focused regression test each (5 total).

## Verification

- Old-code proof for (1): the new boundary test (`data_end` at
  `usize::MAX - 1`) run against the pre-fix lines panics with
  `attempt to add with overflow` at `engine.rs:1004:27`; with the fix
  it returns `Framing`. The prescribed `FFFFFFFFFFFFFFFE` size asserts
  `Framing` on both (it trips the earlier `cursor + size` check).
- Old-code proof for (2) by byte math: with 19 `é` pads both window
  edges (14, 80) land on `é` continuation bytes, so the old slice
  panicked; the test passes with the fix.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --locked -p velnor-tools --all-targets --all-features
  -- -D warnings`: clean. `velnor-runner` has 6 pre-existing
  `unwrap_used`/`expect_used` errors in the untouched
  `execution/command_output.rs` test module — reproduced identical on
  the stashed base, so this branch adds zero new clippy fires.
- Affected suites green: `docker::engine` 21 passed,
  `execution::unix_api` 4 passed, `expression::` 39 passed,
  `docker::client claimed_rm_args` 1 passed, `velnor-tools
  hardcoded_lane` 1 passed.
