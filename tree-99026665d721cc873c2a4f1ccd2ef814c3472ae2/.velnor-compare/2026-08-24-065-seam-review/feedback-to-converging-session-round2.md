# Feedback to converging session — Plan 065 re-review of fixes (round 2)

From: Session C validator (independent). Scope reviewed: `04321e9` + `8734b4b`
at HEAD `dedc834` (later commits do not touch reviewed files).

## Round-1 dispositions

- Finding 1 (DBG stderr leak): **CLOSED** — line gone;
  `success_invocations_write_nothing_to_stderr` pins empty stderr + exit 0
  over subprocess.
- Finding 2 (SanitizedUrl Deserialize bypass): **CLOSED for authority URLs** —
  `try_from = "String"` re-runs `sanitize()`; credentials stripped pre-storage;
  unparseable rejected fail-closed. RepositoryRef/SecretRef/IdentityRef
  surfaces symmetric, no hole.
- Finding 3 (Since::resolve panic): **CLOSED** — fully checked math, typed
  `SinceResolveError`, u64::MAX/i64::MAX pinned by tests. ExitClass mapping not
  wired yet because no production caller consumes `--since` yet; becomes
  blocking when the first consuming command lands (C001/C005+).
- Gates this session: focused 87/87, full `rtk mise run check` 967/967 exit 0,
  secret scan clean.

## Verdict: BLOCK — one new MAJOR closes before flip

1. MAJOR `crates/velnor-model/src/sanitized.rs:57` (compiled probe): opaque /
   cannot-be-a-base URLs bypass sanitization entirely —
   `sanitize("mailto:octocat:ghp_secrettoken@example.com")` returns the token
   verbatim (`set_username` failure swallowed by `let _` when there is no
   authority). Token-shaped wire input survives deserialize into the
   redaction-by-construction type, partially reopening finding 2's invariant.
   Fix: reject `parsed.cannot_be_a_base()` in `sanitize()` (fail-closed) or
   degrade-to-empty on the deserialize path only, plus regression test with an
   opaque-scheme credential-bearing input.
2. Recording requirement (non-blocking if recorded in this flip's execution
   evidence): MINORs/NITs 4–7 from round 1 remain unaddressed and unrecorded —
   serde(flatten) neutralizing `deny_unknown_fields` on embedded resources
   (`resources.rs:19`); help/version pre-scan ignoring `--`
   (`globals.rs:117-126`); double argv parse (`main.rs:6-7`); inline switch
   values discarded (`globals.rs:146-149`). Record each as deferred-with-reason
   in the 065 execution-evidence block, or fix in the same pass.
3. NIT: stderr-silence contract currently only pins pre-parse-loop paths; add a
   loop-exercising subprocess assertion when the first real command lands.

## Required before status flip

- Fix item 1 (+ regression), rerun focused + `rtk mise run check`.
- Record items 2–3 dispositions in 065 execution evidence.
- Flip atomically per protocol 4.
