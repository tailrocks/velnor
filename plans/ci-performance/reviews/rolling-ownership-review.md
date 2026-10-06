# Velnor rolling/evidence review

## Scope

Reviewed published evidence commit `3273394847abce2875a556b6b4d14000536dc4e0`,
the exact generated-tree checkpoint, and the recorded complete PR run.

## Run 35511815559

- [Run](https://github.com/tailrocks/velnor/actions/runs/35511815559)
- [Generator job](https://github.com/tailrocks/velnor/actions/runs/35511815559/job/106080949689)
- [Policy run](https://github.com/tailrocks/velnor/actions/runs/35511814394)
- Head SHA: `27bfb54bdecce09a71f320885bb64f5492083744`
- Raw jobs: 68 total; 20 successful, 48 skipped, no failures; every job attempt is 1.
- Created `12:51:26Z`; latest required job completed `13:00:45Z`; trigger-to-required = 559 s.
- Successful-job execution sum = 1,359 s; this is an aggregate, not a critical-path claim.
- Maximum successful runner job = 525 s (`106080949562`); generator job = 184 s.
- Generator log: 1,952 tests, 1,952 passed, 0 skipped.
- Runner log: 2,516 tests passed, 1 leaky, 5 skipped. API timestamps remain the timing source.

The package-release recovery change in `27bfb54b` has one race defect found by
the later independent challenge. Existing-release lookup paginates and requires
one exact tag match; mutation requires the exact expected source commit. But in
the no-previous-release rollback path, `27bfb54b` deletes a current tag when its
SHA matches the candidate, even though release creation does not prove that
this run created the tag. The corrected fixture
`/tmp/velnor-tag-race-before.log` fails on `27bfb54b` with
`rollback deleted the external writer tag`.

PR973 head `04da35e41e8ce806ff7d7f59ebba7b204b15cb95` removes that destructive
delete branch. It retains the tag and publication lock for manual recovery;
existing-release restoration and empty-tag cleanup remain unchanged. This is a
PASS for the narrow race fix, subject to rerunning the focused fixture after
integration onto `27bfb54b` because the PR base is older. The run above proves
generator and runner tests, not a performance improvement.

## Subsequent race finding and independent challenge

The earlier no-blocking-finding verdict was bounded to the reviewed diff; it
is superseded for newly observed tag ownership by PR #973 head
`04da35e41e8ce806ff7d7f59ebba7b204b15cb95`. Parent added its executable
regression fixture to the current integration source and supplied the existing
rollback helper stub needed by this newer base. Before intervention it failed
explicitly: `rollback deleted the external writer tag`.

Independent reviewer `/root/parallax_inventory` approved the narrow remedy:
remove the unsafe tag-delete branch, retain the tag and publication lock, and
leave existing-release restoration unchanged. Parent applied the actual
upstream production delta; the same fixture then passed. Parent strict Clippy, formatting, actionlint and all 1,953 generator tests
pass. Exact pushed-revision CI remains pending for this new unit.
