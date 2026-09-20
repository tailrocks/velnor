# Authenticated App/check-suite fixture subset

Exact body bytes copied from the read-only source supplement
`G0/real-api-fixture-supplement-20260920T085311Z-corrected-20260920T090934Z`.

- Source manifest SHA-256: `fea65b61ff1668ff58762066b207ac1c9534452d328080f1f0680e7cb571447e`.
- Source status: `read_only_source_supplement_not_gate`.
- Source capture: authenticated GitHub CLI `GET`; no credentials copied.
- This subset contains the three original Velnor chain suites and three
  provider App envelopes used by the real check-runs corpus, plus the
  original Homebrew Sonar suite.
- Homebrew queued/list-only suites and Claude are intentionally excluded from
  the positive chain. They remain source evidence, not execution success.

The test binds each body to its exact endpoint, request page, source commit,
suite ID, App ID/slug, and recomputed digest. It does not assert a live gate.
