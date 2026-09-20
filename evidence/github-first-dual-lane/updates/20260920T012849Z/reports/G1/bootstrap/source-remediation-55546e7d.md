# Author remediation note: `55546e7d`

- This note is separate from reviewer-owned `source-review-d60c0e.md`.
- Commit: `55546e7d64d77a3230c4e6ed52b12f433e3f36aa` (`test: require ZIP producer transport`).
- `fixture-contract.json` fixes producer transport to `zip`; `trusted-harness.sh`
  requires that contract field and asserts the exact producer census result has
  `.format == "zip"`.
- Generic TAR handling remains for clean source archives. Producer execution has
  no TAR fallback or alias.
- The negative suite creates a safe exact-member TAR, confirms generic checker
  acceptance, then confirms the producer ZIP assertion rejects it. Bash syntax,
  ShellCheck, diff check, and the full negative suite passed.
- No hostile probe, Docker/OrbStack/Velnor, network, or Mac-host runtime ran.
  This author note makes no G1 approval claim; independent reviewer verification
  and hosted Linux canary remain required.
