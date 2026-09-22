use std::fs;
use velnor_action_contract::{parse, ActionRuns, BooleanValue, CompositeStep, ParseError};

fn fixture(name: &str) -> Result<String, std::io::Error> {
    fs::read_to_string(format!("tests/fixtures/{name}.yml"))
}

#[test]
fn parses_node_metadata_and_runner_string_coercion() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = parse(&fixture("node")?)?;

    assert_eq!(metadata.name.as_deref(), Some("Fixture action"));
    assert_eq!(metadata.description.as_deref(), Some("7"));
    assert_eq!(metadata.inputs["version"].default.as_deref(), Some("7"));
    assert_eq!(
        metadata.inputs["version"].deprecation_message.as_deref(),
        Some("false")
    );
    assert_eq!(metadata.inputs["empty"].default.as_deref(), Some(""));
    assert_eq!(
        metadata.inputs["small"].default.as_deref(),
        Some("1.234E-05")
    );
    assert_eq!(metadata.inputs["large"].default.as_deref(), Some("1E+20"));
    assert_eq!(
        metadata.outputs["result"].description.as_deref(),
        Some("true")
    );
    assert_eq!(
        metadata.outputs["result"].value.as_deref(),
        Some("${{ steps.build.outputs.result }}")
    );

    let ActionRuns::Node(runs) = metadata.runs else {
        return Err("expected node runtime".into());
    };
    assert_eq!(runs.using, "node20");
    assert_eq!(runs.main, "dist/index.js");
    assert_eq!(runs.pre_if.as_deref(), Some("${{ inputs.version }}"));
    Ok(())
}

#[test]
fn parses_docker_runtime_scalars_and_strict_fields() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = parse(&fixture("docker")?)?;
    let ActionRuns::Docker(runs) = metadata.runs else {
        return Err("expected Docker runtime".into());
    };
    assert_eq!(runs.args, ["false", "7", ""]);
    assert_eq!(runs.env["FLAG"], "false");
    assert_eq!(runs.env["COUNT"], "7");
    assert_eq!(runs.pre_entrypoint.as_deref(), Some("/pre.sh"));
    assert_eq!(runs.post_if.as_deref(), Some("always()"));

    let Err(error) = parse("runs:\n  using: docker\n  image: Dockerfile\n  unknown: true\n") else {
        return Err("unknown runtime field was accepted".into());
    };
    assert!(matches!(error, ParseError::UnknownField { .. }), "{error}");
    Ok(())
}

#[test]
fn parses_composite_step_one_of_shape() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = parse(&fixture("composite")?)?;
    let ActionRuns::Composite(runs) = metadata.runs else {
        return Err("expected composite runtime".into());
    };
    assert_eq!(runs.steps.len(), 2);
    let CompositeStep::Run(run) = &runs.steps[0] else {
        return Err("expected run step".into());
    };
    assert_eq!(run.shell, "bash");
    assert_eq!(run.env["GREETING"], "${{ inputs.greeting }}");
    assert!(matches!(
        run.continue_on_error,
        Some(BooleanValue::Expression(_))
    ));
    let CompositeStep::Uses(uses) = &runs.steps[1] else {
        return Err("expected uses step".into());
    };
    assert_eq!(uses.uses, "actions/checkout@v4");
    assert_eq!(uses.with["fetch-depth"], "1");
    assert_eq!(uses.env["FLAG"], "true");
    Ok(())
}

#[test]
fn rejects_invalid_composite_shapes() {
    let both = "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo hi\n      uses: actions/checkout@v4\n";
    assert!(matches!(parse(both), Err(ParseError::Invalid { .. })));

    let neither = "runs:\n  using: composite\n  steps:\n    - name: incomplete\n";
    assert!(matches!(parse(neither), Err(ParseError::Invalid { .. })));

    let missing_shell = "runs:\n  using: composite\n  steps:\n    - run: echo hi\n";
    assert!(matches!(
        parse(missing_shell),
        Err(ParseError::Missing { .. })
    ));
}

#[test]
fn rejects_exact_and_case_insensitive_duplicates() {
    let exact = "runs: {using: node20, USING: node20, main: index.js}\n";
    assert!(parse(exact).is_err());

    let nested = "runs:\n  using: composite\n  steps:\n    - shell: bash\n      SHELL: sh\n      run: echo hi\n";
    assert!(matches!(
        parse(nested),
        Err(ParseError::DuplicateKey { .. })
    ));
}

#[test]
fn matches_runner_case_insensitive_property_lookup() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = parse("Runs:\n  USING: NODE20\n  MAIN: index.js\n")?;
    let ActionRuns::Node(runs) = metadata.runs else {
        return Err("expected node runtime".into());
    };
    assert_eq!(runs.using, "NODE20");
    assert_eq!(runs.main, "index.js");
    Ok(())
}

#[test]
fn rejects_unsupported_runtime_and_expression_shape() {
    let runtime = "runs:\n  using: wasm\n";
    assert!(matches!(parse(runtime), Err(ParseError::Invalid { .. })));

    let expression = "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo hi\n      continue-on-error: '${{ inputs.flag }'\n";
    assert!(matches!(parse(expression), Err(ParseError::Invalid { .. })));
}

#[test]
fn rejects_runner_unsupported_yaml_anchors_and_merge_keys() {
    let anchored = "runs:\n  using: node20\n  main: &entry index.js\n";
    assert!(parse(anchored).is_err());

    let merged = "defaults: &defaults\n  using: node20\nruns:\n  <<: *defaults\n  main: index.js\n";
    assert!(parse(merged).is_err());
}
