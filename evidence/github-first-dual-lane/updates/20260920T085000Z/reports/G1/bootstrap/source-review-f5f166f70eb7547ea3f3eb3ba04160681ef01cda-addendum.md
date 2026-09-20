# f5 bootstrap source-review addendum — full-source boundary

Observed 2026-09-20 Asia/Ho_Chi_Minh. This addendum narrows the prior f5 review to the approved `de1e63` design requirements requested by the owner. It does not reopen generic design or execute Docker/candidate code.

## Exact classification

| Requirement | f5 source evidence | Classification |
| --- | --- | --- |
| Service digest equals downloaded raw ZIP | Acquire compares `raw_zip_sha256` to the REST digest (`crates/velnor-workflow/src/s2/mod.rs:5267-5280`); verifier repeats it for result (`5844-5860`), handoff (`5943-5959`), and producer (`6012-6030`). | **Verified statically.** This closes the b61 service/raw-digest defect; it is not live endpoint evidence. |
| Target tree API proof and fresh exact objects | Acquire checks repository/commit/tree API responses, recursive non-truncated tree digests, and a fresh bare `--no-tags --no-replace-objects` object store (`s2/mod.rs:5146-5178`). Verifier repeats the API/tree/object checks in a new store (`6064-6097`) and archives the exact head object, comparing bytes with the handoff archive (`6111-6114`). | **Verified statically.** This closes the b61 local-checkout/source-authority defect; no live API/object run was authorized. |
| Run/job/attempt, freshness, ambiguity | Run and job queries require one eligible object and matching attempt (`s2/mod.rs:5223-5250`); artifact selection requires one exact name, run, size, expiry, and run/job time window (`5252-5265`). Final verification repeats the producer run/job identity (`6117-6148`). | **Verified statically** for the implemented producer path. |
| Fixed UID and environment allowlist | Execute sets `uid=65532; gid=65532`, passes `--user "$uid:$gid"`, and inspects exactly `HOME`, `PATH`, and the four `SOURCE_*` names (`s2/mod.rs:5602-5610`, `5620-5651`). The transport fixture asserts `65532:65532`, exact six names, and no `HOSTNAME` (`tests/bootstrap_transport.rs:609-651`). | **Verified statically/offline fixture.** No hostile process-environment canary was run. |
| Archive traversal and output surface | Candidate ZIP rejects unsafe/special/duplicate members and requires exactly two files (`s2/mod.rs:5292-5307`); source TAR rejects unsafe names, symlink/hardlink/special members and bounds members/bytes before extraction (`5573-5599`); result output and result ZIP reject links/specials and bound files/bytes (`5665-5669`, `5867-5893`). Producer/handoff ZIPs enforce safe names and exact surfaces (`5966-5979`, `6037-6050`). | **Partial only; blocker below.** The checks exist, but not every archive has an effective size/quota gate. |

## Remaining source blockers

### F5-B1 — no fixed host scratch quota; several ZIP/TAR paths are unbounded

The approved design requires a fixed-size host scratch filesystem and rejects unbounded runner directories (`G1/bootstrap/isolation-design.md:269-273`). f5 uses `RUNNER_TEMP` directly for acquisition, verification, and sandbox staging (`s2/mod.rs:5267-5284`, `5566-5573`, `5861-5863`, `5949-5962`, `6021-6033`). The candidate acquire path checks only free space (`5267-5269`) and bounds the REST candidate artifact's compressed `size_in_bytes` to 256 MiB (`5252-5255`); it does not cap ZIP member `file_size`/total expansion before `payload.extract` (`5292-5307`). A highly compressed candidate ZIP can therefore expand beyond the free-space check.

The verifier's result extractor bounds member count and uncompressed bytes (`5867-5889`), but the result and handoff REST checks do not bound `size_in_bytes` before download (`5844-5849`, `5943-5948`). Handoff and producer exact-surface extractors also have no per-member/total byte cap (`5966-5979`, `6037-6050`). The independently generated verifier source TAR is hashed and compared but has no explicit size check before `cmp` (`6111-6114`). This leaves archive download/extraction and host staging availability outside the approved fixed-quota contract.

Required closure: establish bounded host scratch before any download/extraction; enforce API compressed-size plus pre-extraction member count/declared-uncompressed-byte limits for candidate, producer, handoff, and result transports; enforce verifier source-archive size; preserve post-extraction link/special/inode checks. A `df` free-space threshold and container tmpfs limit are not substitutes for a fixed host quota.

### F5-B2 — full provenance record is enforced partly, but not carried completely

The approved design requires `upload_step_id` and `artifact_binding_method=static-single-uploader-v1` to be recorded after the unique-uploader proof (`isolation-design.md:117-125`), plus the full action/output/REST/raw tuple for each transport (`202-209`). The f5 handoff JSON contains IDs, names, service/raw hashes, timestamps, contract digests, and source identity (`s2/mod.rs:5341-5387`), and the result JSON carries the producer tuple plus execution run/attempt/job (`5676-5730`), but neither carries `upload_step_id`, `artifact_binding_method`, `run_status`, `run_conclusion`, or `object-format`. The producer upload step has an `id` but its action `artifact-id`/`artifact-digest` outputs are not asserted or exposed as producer-job outputs (`crates/velnor-workflow/src/s2/primitives/ir.rs:3130-3137`). Handoff/result outputs are exposed (`s2/mod.rs:5438-5439`, `5455-5457`) and later regex-validated, but the producer path relies on a later REST lookup.

The static workflow contract and final API checks materially bind the current producer artifact, so this is not the old b61 “candidate JSON selects bytes” defect. It is nevertheless a full-source/design acceptance blocker until the schema records the binding method/step and all required status/object-format fields, and each upload action output is fail-closed at its owning job (or the approved design is explicitly amended).

### F5-B3 — required hostile proof remains unavailable

The approved design makes the base-pinned hostile producer canary mandatory before source approval (`isolation-design.md:129-171`) and marks H1-H9—including archive tricks, forged provenance, wrong image, quota/timeout failure, and network/process probing—as blocked without hosted evidence (`287-301`). The four f5 transport tests are valuable generated-shell fixtures, but they use fake tools and harmless archives; they are not that canary. Therefore static controls above cannot be promoted to full bootstrap security approval.

## Design-conformance note

The design's example mandatory sandbox resource tuple specifies `--pids-limit=64` and `--cpus=2` (`isolation-design.md:220-234`); f5 renders 128 PIDs and 1 CPU (`s2/mod.rs:5610`, with exact inspection at `5620-5651`). The fixed UID/env/mount checks are present, but this resource deviation needs an explicit design decision or correction before claiming exact conformance. The final sandbox digest is empty in the checked-in generated surface and the offline fixture supplies a local digest; that known limitation is documented in the main f5 report.

## Delta versus full approval

The c013 delta is narrowly positive: two checkout roles, trusted control cwd, source-only producer mount, action allowlist, PR-head contract equality, static upload namespace, and executable offline transport fixtures are present. It does **not** waive F5-B1/F5-B2/F5-B3, generated pin/drift, image pin, or any hosted G1 gate. No source/generated files were changed in this review.
