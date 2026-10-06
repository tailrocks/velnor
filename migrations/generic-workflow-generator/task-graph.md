# Task graph

1. Preflight + inventory (done).
2. GitHub-default generator contract (done locally).
3. Escape-hatch removal: `--adopt`, templates, command arrays, `pull_request_on_velnor` (done). `static-workflow` still renders this repo's `release.yml`.
4. Generic native-release / Homebrew / APT / signed-archive capabilities (next).
5. Pin generator revision on `tailrocks/velnor` after merge.
6. Migrate remaining 19 targets in structure groups.
7. Retirement audit of three `velnor-actions` repos. Outside-scope live `uses:` currently block deletion: `ChainArgos/blockchain-nodes`, `ChainArgos/jackin-agent-brown`, `jackin-project/homebrew-tap`, `jackin-project/jackin-dev`, `tailrocks/homebrew-parallax`.
8. Fleet completion matrix.
