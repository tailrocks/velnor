# G1 integration commands

All mutation commands target only `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-integration`.

```sh
rtk git status --short --branch
rtk git rev-parse HEAD
rtk git log --oneline --decorate -5
```

Combined source verification, after reviewed changes are integrated:

```sh
CARGO_TARGET_DIR=/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/integration-target \
CARGO_BUILD_JOBS=4 \
  rtk cargo test -p velnor-workflow --lib
CARGO_TARGET_DIR=/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/integration-target \
CARGO_BUILD_JOBS=4 \
  rtk cargo clippy -p velnor-workflow --lib --tests --locked -- -D warnings
rtk cargo fmt --all -- --check
```

Generated-output validation occurs only after the generator pin and config are selected:

```sh
cd crates/velnor-workflow
mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..
```

The final command must be run from a clean clone or a clean temporary checkout at the exact candidate revision. It must not be replaced by hand-editing generated files.
