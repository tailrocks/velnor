//! Lane-mode tests: selector, independence, platform-only applicability, and
//! public-PR routing.

mod common;

use velnor_actions_generator::RepositoryClass;
use velnor_actions_generator::model::{Applicability, FleetManifest, Lane, resolve_lanes};
use velnor_actions_generator::render;

const DUMMY_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

#[test]
fn lane_selector_accepts_only_three_values() {
    assert_eq!(Lane::from_token("velnor"), Some(Lane::Velnor));
    assert_eq!(Lane::from_token("github"), Some(Lane::GitHub));
    assert_eq!(Lane::from_token("both"), Some(Lane::Both));
    assert_eq!(Lane::from_token("VELNOR"), None);
    assert_eq!(Lane::from_token("nightly"), None);
    assert_eq!(Lane::from_token(""), None);
}

#[test]
fn resolve_lanes_expands_both_independently() {
    assert_eq!(resolve_lanes(Lane::Velnor), vec![Lane::Velnor]);
    assert_eq!(resolve_lanes(Lane::GitHub), vec![Lane::GitHub]);
    assert_eq!(resolve_lanes(Lane::Both), vec![Lane::Velnor, Lane::GitHub]);
}

#[test]
fn default_lane_is_velnor_in_template() {
    let t = render::consumer_template(RepositoryClass::Code);
    assert!(t.contains("default: velnor"));
    assert!(
        t.contains("|| 'velnor'"),
        "non-dispatch events default to velnor"
    );
    // The choice input exposes exactly the three ordered options.
    let opts = t.find("options:").expect("options present");
    let tail = &t[opts..];
    let v = tail.find("- velnor").unwrap();
    let g = tail.find("- github").unwrap();
    let b = tail.find("- both").unwrap();
    assert!(v < g && g < b, "options ordered velnor, github, both");
}

#[test]
fn tap_has_explicit_platform_only_gate() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let tap = m.class(RepositoryClass::Tap);
    assert!(tap.platform_only, "tap declares a platform-only gate");
    let test_gate = tap.gates.iter().find(|g| g.name == "test").unwrap();
    assert_eq!(test_gate.applicability, Applicability::Github);
    // Velnor lane excludes the platform-only gate; GitHub lane includes it.
    let velnor: Vec<_> = tap
        .applicable_gates(Lane::Velnor)
        .iter()
        .map(|g| g.name.clone())
        .collect();
    let github: Vec<_> = tap
        .applicable_gates(Lane::GitHub)
        .iter()
        .map(|g| g.name.clone())
        .collect();
    assert!(
        !velnor.contains(&"test".to_string()),
        "velnor lane omits platform-only test"
    );
    assert!(
        github.contains(&"test".to_string()),
        "github lane runs platform-only test"
    );
}

#[test]
fn code_standard_project_command_is_mise_run_ci() {
    assert_eq!(
        velnor_actions_generator::model::CODE_STANDARD_COMMAND,
        "mise run ci"
    );
}

#[test]
fn code_class_is_lane_portable() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let code = m.class(RepositoryClass::Code);
    assert!(!code.platform_only);
    assert_eq!(code.applicable_gates(Lane::Velnor).len(), 5);
    assert_eq!(code.applicable_gates(Lane::GitHub).len(), 5);
    for gate in &code.gates {
        assert_eq!(gate.applicability, Applicability::Both);
        assert!(!gate.command.trim().is_empty(), "gate command non-empty");
    }
}

#[test]
fn public_unmerged_routes_velnor_lane_to_github_hosted() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let wf = render::callable_workflow(m.class(RepositoryClass::Code), DUMMY_SHA);
    assert!(
        wf.contains(
            "runs-on: ${{ (github.event_name == 'pull_request' || github.event_name == 'merge_group') && 'ubuntu-latest' || 'velnor-trusted' }}"
        ),
        "velnor lane routes public unmerged code to GitHub-hosted"
    );
}

#[test]
fn both_lanes_are_independent_and_neither_substitutes() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let wf = render::callable_workflow(m.class(RepositoryClass::Code), DUMMY_SHA);
    // Two separately named lane jobs; both required in `both`.
    assert!(wf.contains("velnor-lane:"));
    assert!(wf.contains("github-lane:"));
    // The `both` branch requires success on BOTH lanes; no lane substitution.
    assert!(wf.contains("one lane never substitutes for the other"));
    // Skipped or failed selected lanes are never credited as success.
    assert!(wf.contains("is never"));
}

#[test]
fn same_gate_semantics_on_both_lanes_for_portable_gates() {
    let m = FleetManifest::load(&common::repo_root()).unwrap();
    let wf = render::callable_workflow(m.class(RepositoryClass::Code), DUMMY_SHA);
    // The install/build/test/lint/format gate commands appear on both lanes.
    for cmd in [
        "mise install --locked",
        "mise run build",
        "mise run test",
        "mise run lint",
        "mise run fmt",
    ] {
        assert_eq!(
            wf.matches(&format!("command: {cmd}")).count(),
            2,
            "{cmd} on both lanes"
        );
    }
}
