//! Owner fan-out and fail-closed aggregation tests.

mod common;

use velnor_actions_generator::RepositoryClass;
use velnor_actions_generator::model::OWNERS;
use velnor_actions_generator::render;

const DUMMY_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

fn code_template() -> String {
    render::consumer_template(RepositoryClass::Code)
}

#[test]
fn exactly_three_static_owner_calls() {
    let t = code_template();
    assert_eq!(OWNERS, ["jackin-project", "tailrocks", "ChainArgos"]);
    // Exactly three reusable-workflow calls.
    assert_eq!(t.matches("uses:").count(), 3);
    for owner in OWNERS {
        assert!(t.contains(&format!(
            "uses: {owner}/velnor-actions/.github/workflows/ci-code.yml@@FLEET_SHA@ # @CALVER@"
        )));
        assert!(t.contains(&format!(
            "if: ${{{{ github.repository_owner == '{owner}' }}}}"
        )));
    }
    // No dynamic `uses:` — the ref never contains an expression.
    for line in t.lines() {
        if line.contains("uses:") {
            assert!(!line.contains("${{"), "static uses only: {line}");
        }
    }
}

#[test]
fn ci_required_is_fail_closed_always() {
    let t = code_template();
    assert!(t.contains("ci-required:"));
    assert!(t.contains("if: ${{ always() }}"));
    // needs all three owner calls.
    for owner in OWNERS {
        assert!(t.contains(&format!("- {owner}")));
    }
}

#[test]
fn ci_required_uses_positive_truth_table() {
    let t = code_template();
    // Positive requirements.
    assert!(t.contains("expected 'success'"), "selected must be success");
    assert!(
        t.contains("expected both 'skipped'"),
        "others must be skipped"
    );
    assert!(
        t.contains("expected empty"),
        "others must have empty outputs"
    );
    assert!(t.contains("unrecognized owner"), "unknown owner rejected");
    // Forbidden negative "absence of failure" acceptance logic must be absent.
    assert!(!t.contains("!= \"failure\""));
    assert!(!t.contains("!= 'failure'"));
    assert!(!t.contains("|| true"));
}

#[test]
fn three_calls_share_one_sha_and_calver() {
    let t = code_template();
    let out = render::render_consumer(&t, DUMMY_SHA, "2026.7.0").unwrap();
    // Exactly three coherent, identical pins.
    assert_eq!(out.matches(&format!("@{DUMMY_SHA} # 2026.7.0")).count(), 3);
    // No mixed pin: the SHA never appears without its CalVer comment beside it.
    for line in out.lines() {
        if line.contains(&format!("@{DUMMY_SHA}")) {
            assert!(line.contains("# 2026.7.0"), "coherent pin on: {line}");
        }
    }
}

#[test]
fn contract_output_is_required_by_aggregator() {
    let t = code_template();
    // ci-required reads BOTH the selected result and its explicit contract output.
    assert!(t.contains("needs.jackin-project.result"));
    assert!(t.contains("needs.jackin-project.outputs.contract"));
    assert!(t.contains("needs.tailrocks.result"));
    assert!(t.contains("needs.tailrocks.outputs.contract"));
    assert!(t.contains("needs.ChainArgos.result"));
    assert!(t.contains("needs.ChainArgos.outputs.contract"));
}

#[test]
fn all_classes_expose_three_owner_calls() {
    use velnor_actions_generator::ALL_CLASSES;
    for class in ALL_CLASSES {
        let t = render::consumer_template(class);
        assert_eq!(
            t.matches("uses:").count(),
            3,
            "{} has 3 owner calls",
            class.code()
        );
        let file = render::callable_file_name(class);
        for owner in OWNERS {
            assert!(t.contains(&format!(
                "uses: {owner}/velnor-actions/.github/workflows/{file}@"
            )));
        }
    }
}
