# Diagnostics lifecycle failure disposition

Historical failure was deterministic, in a superseded PR integration tree; no evidence of a current flaky test remains.

- Run35520025700, job106102422435 used PR977 head7a94393b but actually checked out synthetic merge **b9f7e19f86c3e62977aabd8c0dca73519a480eeb** (job log records checkout and reusable workflow SHA).
- `scaleset_daemon::crash_with_dead_workers_fails_explicitly` failed at line1544: `A's logs were exported before deletion`.
- Authenticated Contents API retrieved lane.rs, supervise.rs and unchanged test from that exact tested commit. Historical `lane.rs:1054-1068` calls `live.supervision.release_owned_state()` before releasing the permit. `supervise.rs:838+` implements this as removal of the whole worker state directory. Thus the assertion that runner.log still exists contradicts that candidate lifecycle.
- Current97bac4c4 excludes the candidate lifecycle extension. Current lane terminal path exports diagnostics/removes Docker resources/releases permit, but has no `release_owned_state` call. This is a source difference, not an unexplained successful rerun.
- The entire test source is byte-identical across historical/current revisions, SHA256 `c1ca44478d9963acfe0225d2c48e8868866af238fb1fd57d834e96d31d8701b7`. Source comparison assertions executed successfully; `diagnostics-source-proof.json.gz` records identities, and two compressed diffs preserve changed production behavior.
- Successor PR35522028476/job106107688847 explicitly reports `PASS [1.273s] velnor-runner::scaleset_daemon crash_with_dead_workers_fails_explicitly` at2026-09-20T16:21:46Z. Full raw successor log preserved compressed.

Disposition: obsolete candidate code/test contract mismatch, rejected by PR CI and absent from final integrated source. No runner product edit is justified by this particular failure. Current CI must retain the integration test; canonical pre-merge tests remain the prevention stage. This does not claim the discarded lifecycle extension was correctly implemented, nor certify all lifecycle behavior.
