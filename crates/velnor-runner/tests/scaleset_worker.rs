//! Worker-lane conformance: the recorded profile matches the compiled pins.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
#![cfg(feature = "test-support")]

use velnor_runner::scaleset::worker::{
    PinnedImage, DIND_DIGEST_AMD64, DIND_DIGEST_ARM64, DIND_INDEX_DIGEST, DIND_REPOSITORY,
    DIND_VERSION, RUNNER_DIGEST_AMD64, RUNNER_DIGEST_ARM64, RUNNER_INDEX_DIGEST, RUNNER_REPOSITORY,
    RUNNER_VERSION,
};
use velnor_runner::scaleset::Fixtures;

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scaleset-worker")
}

#[test]
fn recorded_profile_matches_compiled_pins() {
    let fixtures = Fixtures::load(&fixture_dir()).unwrap();
    assert_eq!(fixtures.manifest().files.len(), 1);
    let profile: serde_json::Value = fixtures.parse("worker_profile.json").unwrap();
    assert_eq!(profile["profile"], "homogeneous");

    let runner = &profile["runner"];
    assert_eq!(runner["repository"], RUNNER_REPOSITORY);
    assert_eq!(runner["version"], RUNNER_VERSION);
    assert_eq!(runner["index_digest"], RUNNER_INDEX_DIGEST);
    assert_eq!(runner["digest_amd64"], RUNNER_DIGEST_AMD64);
    assert_eq!(runner["digest_arm64"], RUNNER_DIGEST_ARM64);
    // Every recorded digest parses as a pin (no tags in the fixture).
    for digest in ["index_digest", "digest_amd64", "digest_arm64"] {
        let reference = format!(
            "{}@{}",
            runner["repository"].as_str().unwrap(),
            runner[digest].as_str().unwrap()
        );
        assert!(PinnedImage::parse(&reference).is_ok(), "{reference}");
    }

    let dind = &profile["dind"];
    assert_eq!(dind["repository"], DIND_REPOSITORY);
    assert_eq!(dind["version"], DIND_VERSION);
    assert_eq!(dind["index_digest"], DIND_INDEX_DIGEST);
    assert_eq!(dind["digest_amd64"], DIND_DIGEST_AMD64);
    assert_eq!(dind["digest_arm64"], DIND_DIGEST_ARM64);

    assert_eq!(profile["socket"], "/velnor/scaleset/dind.sock");
    assert_eq!(profile["jit_env"], "ACTIONS_RUNNER_INPUT_JITCONFIG");
}
