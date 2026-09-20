# Real GitHub API fixture corpus

Immutable read-only response fixtures. See `manifest.json` for exact request metadata and hashes; see `relationship-report.json` for entity relationships and unknowns.

This is provider evidence for checker/mapper regression only. It is not a gate, authority decision, workflow dispatch, or live-state claim. The Velnor run and the Homebrew Sonar response are separate provider fixtures; no Sonar check was present in the selected Velnor SHA capture.

Raw response files are copied byte-for-byte from the existing G0 captures, except `raw/metadata/org-tailrocks.response` and `raw/metadata/user-tailrocks.response`, which are direct read-only `gh api --method GET --include` envelopes captured at 2026-09-20T08:03:18Z. No credential material is included.
