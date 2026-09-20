# Evidence checkpoint 20260920T062845Z

- Capture cutoff: **2026-09-20T06:28:45Z** UTC.
- Parent ref tip before copy: `38269cf0ebe2c6c007f80fa78458fc63d40f76a5`.
- Prior cutoff: **2026-09-20T05:32:10Z** UTC.
- Source base observation: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Included: **332** stable regular files, **3615060** source bytes (G0=290, G1=42); JSON files: **73**, JSONL records: **153**.
- Attestation: **none**. Gate status: **not-evaluated**. This snapshot proves no gate.

The batch freezes v11 with its complete generated bundle and canonical root closure. Root digest is `deb19280afeb9bf17b4c80ca52d808a1115822e7f8d48ff28f204e9a7388c017`; raw root-manifest SHA-256 is `35f8ae8a20cfe3ce774ed0d63ece8ed06d3e66d4e66693933ea5e5825e32a96d`; all 20 bound files and bundle-index anchors matched. The owner audit remains 59/59; earlier independent audit remains 19/19; Luna is separate at 24/38 with 9 failures and 5 unimplemented, all authority claims false. v11 remains `proposal_only_external_blocked`, pending independent authority review.

The corrected 061548Z skills raw partition is included in full (**281 files / 2345007 bytes**) with its independent review. The review explicitly says the partition is inventory-only, `complete: false`, `claimable: false`, and workflow/check/status coverage incomplete.

Native 578, APT f069/71a, bootstrap bba2, recovery 0e344, raw-store 024, checkout/CAS 81f, and runtime c704/5b reports are immutable read-only evidence with local verdicts retained. The 38269 packaging review is retained as the parent integrity result. No disposition is promoted to G0-G7 authority.

Live ruleset captures/ledgers, active owner reports, v12, older duplicate docs/rust-scan and checkout reports, builds, raw logs, secrets, caches, clones, and symlinks are excluded. All selected sources were regular, rehashed before/after copy, and destination bytes matched. JSON parsing and secret scan passed with **0** matches. Prior snapshots remain untouched; no source or remote authority state changed.

`git diff --check` reports whitespace already present in preserved external Markdown/API-response bytes (including Markdown hard-break spaces and blank EOF lines); no normalization was applied because byte identity is part of this evidence record.
