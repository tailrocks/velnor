# tailrocks/velnor fleet snapshot

- Snapshot directory: `20260920T093002Z`
- Principal: `donbeave` (login only; no credential material saved)
- Default branch: `main`
- Current `main` commit, initial read: `9307861d2668424d27012c3c736d48ab47f9012e`
- Current `main` commit, reread: `9307861d2668424d27012c3c736d48ab47f9012e`
- Open PRs: `9` initially; `9` on reread
- Pagination: one page per open-PR read (`per_page=100`)
- API errors: none; all capture stderr files are empty and all calls exited 0

## Reread PR identities

`bot` is derived from `user.type == "Bot"`. `head_fork` and `base_fork` are the API repository `fork` booleans. Raw PR bodies remain in `initial/pulls.json`, `initial/pulls.http`, `reread/pulls.json`, and `reread/pulls.http`.

| PR | draft | bot | head ref / repo / fork / SHA | base ref / repo / fork / SHA | merge SHA |
|---:|:---:|:---:|---|---|---|
| 971 | false | false | `codex/fix-sccache-env-actionlint` / `tailrocks/velnor` / false / `311a3d2658657aabca3183a74bcd300d0a158956` | `main` / `tailrocks/velnor` / false / `9307861d2668424d27012c3c736d48ab47f9012e` | `502845e391d4508d89d852002bca4f4d715e119a` |
| 968 | true | false | `codex/ci-performance-campaign` / `tailrocks/velnor` / false / `2447cc8d96150a0efc71165d9cc5649fdabd9e96` | `main` / `tailrocks/velnor` / false / `89f82dd8b287f46a3cf4c0920f341f6ca6c736db` | null |
| 963 | false | false | `codex/github-first-g3-integration-signed` / `tailrocks/velnor` / false / `056362aadb738279924ef05597f9a014392f48bf` | `main` / `tailrocks/velnor` / false / `325719f1e05d3d46322c9fd3eeb9ad545e175638` | null |
| 962 | false | false | `codex/github-first-hosted-g1-security-3ae` / `tailrocks/velnor` / false / `43ba3b414f245bf3aa9176afce5bf97f2d1e5235` | `main` / `tailrocks/velnor` / false / `1048337062ea625fada1b4f7c07f2feed75f60c7` | null |
| 961 | false | false | `codex/github-first-g3-integration` / `tailrocks/velnor` / false / `5b9a16a620951b65bbfe0a5cf7b1ffe04a317303` | `main` / `tailrocks/velnor` / false / `b5a4b4afaa6ca807927cacc03659b570a895dd5c` | null |
| 960 | false | false | `codex/g1-preview-amd64-dirty-identity` / `tailrocks/velnor` / false / `c8f7a2b353f9d2d6a60d3ad8bb2dc6299a108ceb` | `main` / `tailrocks/velnor` / false / `1048337062ea625fada1b4f7c07f2feed75f60c7` | `b27461728395c6ba4ff228500ca863d0271e4c7b` |
| 957 | false | false | `codex/latest-macos-policy` / `tailrocks/velnor` / false / `92387e88c32f933a9061b819256e535662655cb2` | `main` / `tailrocks/velnor` / false / `b5a4b4afaa6ca807927cacc03659b570a895dd5c` | null |
| 952 | false | false | `fix/release-leg-seed-pin-fetch` / `tailrocks/velnor` / false / `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637` | `main` / `tailrocks/velnor` / false / `3123f8ae63a1fe98fe1d4dc86fb25b212defcd59` | null |
| 948 | true | false | `c1/bastion-host-setup` / `tailrocks/velnor` / false / `ab82b08746ec312558981f8461a32a1e3350d802` | `main` / `tailrocks/velnor` / false / `c2883fb3b1545127e89f290d853dd286e1ce805c` | null |

## Churn

- Main SHA: unchanged (`9307861d2668424d27012c3c736d48ab47f9012e` → same).
- Open-PR count: unchanged (`9` → `9`).
- Identity-key churn: PR `#971` only. Initial read had head `c541b95130676db88cc561eec8c20a84ec188918`, base `e717de39b97558117e7e4f55c20216c8a4e4b1fc`, merge `a3c10a112865e71dc19b1a2a9bf12883e544cf48`; reread had head `311a3d2658657aabca3183a74bcd300d0a158956`, base `9307861d2668424d27012c3c736d48ab47f9012e`, merge `502845e391d4508d89d852002bca4f4d715e119a`.
- PR `#971` `updated_at` changed from `2026-09-20T09:25:36Z` to `2026-09-20T09:31:57Z`.
- No PR additions/removals, draft changes, bot changes, or fork changes observed.

## Limitations and scope

- REST responses are point-in-time reads; the observed #971 change proves fleet churn during capture but does not establish its cause.
- No checks, logs, commit history, dispatches, merges, cancellations, or GitHub writes were requested or performed.
- Raw `--include` files retain response headers and bodies; slurped `.json` files retain response bodies for exact parsing. Derived identity files are projections of those raw responses.
