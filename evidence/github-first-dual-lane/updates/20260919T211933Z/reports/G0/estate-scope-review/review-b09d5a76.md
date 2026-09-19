# Exact strict-JSON source review — `b09d5a76133fb9d4e552e2d5253876b3988567f6`

Review timestamp: 2026-09-20 (Asia/Ho_Chi_Minh; UTC snapshot in command
evidence). Review tree: detached `/private/tmp/g2-strict-json-review-b09`.
Base: `e5b475b80110447469fdeb4a24b7967008b39aca`. No source files, owner
worktrees, branches, remotes, or merges were changed.

## Disposition

**APPROVE SOURCE.** The strict parser is correctly placed before typed
`EstateManifest` deserialization and rejects duplicate object keys at every
visited object depth. No source blocker found in this bounded review.

This is source approval only. It is **not** approval of G0, G2, or any G0-G7
gate, and it makes no fleet, identity, release, or runtime claim.

## Source proof

- `crates/velnor-tools/src/strict_json.rs:19-30` runs a token-stream visitor,
  checks `.end()` for trailing data, then performs typed deserialization from
  the original bytes.
- `strict_json.rs:98-118` recursively walks arrays and objects. Object keys
  are decoded into `String` before insertion into a per-object `BTreeSet`, so
  equal and conflicting duplicates, including escaped-equivalent keys, share
  the same comparison.
- `crates/velnor-tools/src/audit_ci.rs:363-371` routes the canonical
  `config/estate-repositories.json` through `strict_json::from_str`.
- `audit_ci.rs:681-687` routes caller `--estate` input through the same
  parser. The canonical parse occurs before caller parsing, so either source
  fails closed independently.
- No runtime `EstateManifest` parse remains on the `audit-ci` paths outside
  this wrapper; remaining direct parses are source unit-test fixtures, not
  audit input paths.

## Adversarial CLI evidence

| Case | Result |
| --- | --- |
| Root equal duplicate (`version`/`version`) | rejected with `duplicate JSON object key` |
| Root conflicting duplicate | rejected with `duplicate JSON object key` |
| Nested equal duplicate (`scope.role`) | rejected with `duplicate JSON object key` |
| Nested conflicting duplicate | rejected with `duplicate JSON object key` |
| Escaped-equivalent root (`version`/`vers\\u0069on`) | rejected; key decoded as `version` |
| Duplicate object inside array | rejected with `duplicate JSON object key` |
| Malformed/truncated JSON | rejected during first parse |
| Valid JSON plus trailing token | rejected by `Deserializer::end()` |
| Canonical path duplicate mutation | rejected before freshness guard |
| Caller `--estate` duplicate fixtures | rejected before scope/typed validation |
| Valid canonical/caller control | parser and scope pass; intentional offline freshness guard is reached |

Size/depth probes were fail-closed: 8 MiB and 32 MiB caller documents were
processed without panic and then rejected by typed schema validation; a 32 MiB
document with the duplicate key after the large value was rejected at the
duplicate key (not silently accepted). Nested array input reaches serde_json's
recursion guard and returns an error rather than overflowing the stack. The
implementation inherits `fs::read_to_string` and has no explicit byte budget;
this is an operational observation, not a duplicate-key bypass or source
approval blocker for this local audit CLI.

## Verification

- `rtk cargo test -p velnor-tools --all-targets` — **232 passed**.
- `rtk cargo test --workspace --all-targets` — **5,322 passed, 5 ignored**;
  2,342 filtered; 69 suites; 131.47 s.
- `rtk cargo clippy -p velnor-tools --all-targets --all-features -- -D warnings`
  — no issues.
- `rtk proxy cargo clippy --workspace --all-targets --all-features -- -D warnings`
  — exit 0; one existing `velnor-runner` release-build identity advisory,
  no lint error.
- `rtk cargo fmt --all -- --check` — pass.
- `rtk git diff --check e5b475b80110447469fdeb4a24b7967008b39aca b09d5a76133fb9d4e552e2d5253876b3988567f6` — pass.
- Detached tree clean at completion.
