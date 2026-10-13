# Immediate progress checkpoint

Created at the user’s request to commit and push all current work.
The active branch remains `fix/ci-validation-contract`.

Implemented source is committed separately: package channel mapping
`0348729f`, exact renderer bootstrap `e1c589eb`, protocol fixture framing
`c405d37`, and isolated staged hooks `4d3e55d9`. Parent integration checks
for the bootstrap passed formatting, strict generator Clippy and 1,966 tests;
the single generated-file comparison remains pending regeneration.

The compressed typed-stage and package-preflight patches preserve unfinished
implementation without presenting it as accepted executable source. Their
status files record bases, verification and remaining work. The preflight
baseline manifest binds its delta to the corrected bootstrap source.
Merge-queue work has an approved design but no source changes yet.

`manifest.json` records exact source and stored hashes. Decompress patches
with gzip for review and three-way integration; do not apply them blindly.
Hook measurements are observed cold/warm feedback times, not an optimization
comparison. MBX evidence proves a conflicting cache-writer identity; its
repair and comparable performance measurements remain outstanding.

The branch is a draft. Renderer pin/generation, Linux hook verification,
ordered CI stages, feature correspondence, package preflight, merge queue,
cache repairs and complete retained-history dispositions remain unfinished.
No merge or six-nines reliability claim follows from this checkpoint.

The immutable inventory and later collection checkpoint are archived intact
in `inventory-snapshot.tar.gz` and `collection-checkpoint.tar.gz`; extract
them separately to recover their original hash manifests, raw/gzip catalogues,
collection scripts and precise coverage cutoffs. Original evidence is preserved
byte for byte inside those archives.
