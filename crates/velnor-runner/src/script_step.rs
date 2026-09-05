#![allow(dead_code)]

use crate::{
    command_files::{parse_command_file_contents, FileCommand},
    container::Shell,
    expression::{self, Node},
    fs_copy::NoFollowDestinationDir,
    job_message::{ActionReferenceType, ActionStep},
};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ScriptStep {
    pub id: String,
    pub display_name: String,
    pub script: String,
    pub shell: Shell,
    pub working_directory_container: String,
    pub env: Vec<(String, String)>,
    pub condition: Option<String>,
    pub continue_on_error: bool,
    pub timeout_minutes: Option<u64>,
}

pub fn github_script_steps(
    steps: &[ActionStep],
    workspace_container: &str,
) -> Result<Vec<ScriptStep>> {
    github_script_steps_with_defaults(steps, workspace_container, &[])
}

pub fn github_script_steps_with_defaults(
    steps: &[ActionStep],
    workspace_container: &str,
    defaults: &[Value],
) -> Result<Vec<ScriptStep>> {
    github_script_steps_with_context(steps, workspace_container, defaults, &[])
}

pub fn github_script_steps_with_context(
    steps: &[ActionStep],
    workspace_container: &str,
    defaults: &[Value],
    context_data: &[(String, Value)],
) -> Result<Vec<ScriptStep>> {
    let defaults = RunDefaults::from_job_defaults(defaults)?;
    let mut script_steps = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        if !step.enabled {
            continue;
        }
        if step.reference_type() != Some(ActionReferenceType::Script) {
            continue;
        }

        script_steps.push(github_script_step_with_context(
            step,
            index,
            workspace_container,
            &defaults,
            context_data,
        )?);
    }
    Ok(script_steps)
}

pub fn has_enabled_non_script_steps(steps: &[ActionStep]) -> bool {
    steps
        .iter()
        .any(|step| step.enabled && step.reference_type() != Some(ActionReferenceType::Script))
}

fn github_script_step(
    step: &ActionStep,
    index: usize,
    workspace_container: &str,
    defaults: &RunDefaults,
) -> Result<ScriptStep> {
    github_script_step_with_context(step, index, workspace_container, defaults, &[])
}

fn github_script_step_with_context(
    step: &ActionStep,
    index: usize,
    workspace_container: &str,
    defaults: &RunDefaults,
    context_data: &[(String, Value)],
) -> Result<ScriptStep> {
    let inputs = step
        .inputs
        .as_ref()
        .and_then(|value| value.as_object())
        .ok_or_else(|| anyhow::anyhow!("script step {} missing inputs object", index + 1))?;
    // GitHub sends script body as a plain literal OR as a format() expression when ${{ }} is used.
    let script: String = match string_input_field(inputs, &["script", "Script"]) {
        Some(script) => Some(script.to_string()),
        None => match expr_input_field(inputs, &["script", "Script"]) {
            Some(expr) => Some(render_setup_expression(expr, context_data)?),
            None => None,
        },
    }
    .ok_or_else(|| {
        anyhow::anyhow!(
            "script step {} missing script input; input keys: {}",
            index + 1,
            input_summary(inputs)
        )
    })?;
    let shell = string_input_field(inputs, &["shell", "Shell"])
        .or(defaults.shell.as_deref())
        .map(github_shell)
        .transpose()?
        .unwrap_or(Shell::BashDefault);
    const WORKING_DIRECTORY_NAMES: &[&str] = &[
        "workingDirectory",
        "working-directory",
        "WorkingDirectory",
        "Working-Directory",
    ];
    let working_directory = match string_input_field(inputs, WORKING_DIRECTORY_NAMES) {
        Some(directory) => Some(directory.to_string()),
        None => match expr_input_field(inputs, WORKING_DIRECTORY_NAMES) {
            Some(expr) => Some(render_setup_expression(expr, context_data)?),
            None => None,
        },
    }
    .or_else(|| defaults.working_directory.clone())
    .map(|path| workspace_path(workspace_container, &path))
    .unwrap_or_else(|| workspace_container.to_string());

    // Prefer the broker-sent DisplayName. GitHub's Name/ContextName fields
    // carry the step *id*, never a display name, and unnamed steps arrive as
    // internal placeholders such as "__run"; actions/runner then falls back to
    // `Run <first script line>` (ActionRunner.GenerateDisplayName), so any
    // id-based fallback here diverges from the GitHub-hosted lane.
    let display_name = step.display_name_template().unwrap_or_else(|| {
        let first_line = script.lines().next().unwrap_or("").trim();
        if first_line.is_empty() {
            String::new()
        } else {
            format!("Run {first_line}")
        }
    });
    Ok(ScriptStep {
        id: step_id(step, index),
        display_name,
        script,
        shell,
        working_directory_container: working_directory,
        env: step_environment(step)?,
        condition: step.condition.clone(),
        continue_on_error: step_continue_on_error(step),
        timeout_minutes: step_timeout_minutes(step),
    })
}

pub(crate) fn script_input_source_line(step: &ActionStep) -> Option<u64> {
    let inputs = step.inputs.as_ref()?.as_object()?;
    source_line_for_input(inputs, &["script", "Script"])
}

#[derive(Debug, Default)]
struct RunDefaults {
    shell: Option<String>,
    working_directory: Option<String>,
}

impl RunDefaults {
    fn from_job_defaults(defaults: &[Value]) -> Result<Self> {
        let mut run_defaults = Self::default();
        for value in defaults {
            run_defaults.merge_value(value)?;
        }
        Ok(run_defaults)
    }

    fn merge_value(&mut self, value: &Value) -> Result<()> {
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        let run = object_field(object, &["run", "Run", "RUN"]);
        if let Some(run) = run.and_then(Value::as_object) {
            if let Some(shell) = string_field(run, &["shell", "Shell"]) {
                github_shell(shell)?;
                self.shell = Some(shell.to_string());
            }
            if let Some(working_directory) = string_field(
                run,
                &[
                    "workingDirectory",
                    "working-directory",
                    "WorkingDirectory",
                    "Working-Directory",
                ],
            ) {
                self.working_directory = Some(working_directory.to_string());
            }
        }
        Ok(())
    }
}

fn object_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|name| object.get(*name))
        .or_else(|| typed_map_field(object, names))
}

fn string_field<'a>(object: &'a serde_json::Map<String, Value>, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(input_value_as_str))
        .or_else(|| typed_map_field(object, names).and_then(input_value_as_str))
}

fn typed_map_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a Value> {
    let map = object.get("map").or_else(|| object.get("Map"))?;
    if let Some(map) = map.as_object() {
        return names.iter().find_map(|name| map.get(*name));
    }
    map.as_array().and_then(|items| {
        items.iter().find_map(|item| {
            let item = item.as_object()?;
            let name = input_name_field(item)?;
            if !names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
            {
                return None;
            }
            item.get("value").or_else(|| item.get("Value"))
        })
    })
}

fn string_input_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    direct_string_input_field(object, names)
        .or_else(|| nested_map_string_input_field(object, names))
}

fn source_line_for_input(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<u64> {
    direct_source_line_for_input(object, names)
        .or_else(|| nested_map_source_line_for_input(object, names))
}

fn direct_source_line_for_input(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<u64> {
    let entry = object.iter().find_map(|(key, value)| {
        names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(key))
            .then_some(value)
    })?;
    input_source_line(entry)
}

fn nested_map_source_line_for_input(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<u64> {
    let map = object.get("map").or_else(|| object.get("Map"))?;
    if let Some(map) = map.as_object() {
        return source_line_for_input(map, names);
    }
    map.as_array().and_then(|items| {
        items.iter().find_map(|item| {
            let item = item.as_object()?;
            let name = input_name_field(item)?;
            if !names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
            {
                return None;
            }
            item.get("value")
                .or_else(|| item.get("Value"))
                .and_then(input_source_line)
        })
    })
}

fn input_source_line(value: &Value) -> Option<u64> {
    match value {
        Value::Object(object) => object
            .get("line")
            .or_else(|| object.get("Line"))
            .and_then(Value::as_u64)
            .or_else(|| {
                object
                    .get("value")
                    .or_else(|| object.get("Value"))
                    .and_then(input_source_line)
            }),
        _ => None,
    }
}

fn direct_string_input_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| input_value_as_str(object.get(*name)?))
}

fn nested_map_string_input_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    let map = object.get("map").or_else(|| object.get("Map"))?;
    if let Some(map) = map.as_object() {
        return string_input_field(map, names);
    }
    map.as_array().and_then(|items| {
        items.iter().find_map(|item| {
            let item = item.as_object()?;
            let name = input_name_field(item)?;
            if !names
                .iter()
                .any(|expected| name.eq_ignore_ascii_case(expected))
            {
                return None;
            }
            item.get("value")
                .or_else(|| item.get("Value"))
                .and_then(input_value_as_str)
        })
    })
}

fn input_name_field(object: &serde_json::Map<String, Value>) -> Option<&str> {
    ["name", "Name", "key", "Key"]
        .iter()
        .find_map(|name| object.get(*name).and_then(input_value_as_str))
}

/// The `{"expr": "..."}` form the broker sends for an input whose workflow
/// source contained `${{ }}`, looked up by input name in the inputs map.
fn expr_input_field<'a>(
    inputs: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    let items = inputs
        .get("map")
        .or_else(|| inputs.get("Map"))?
        .as_array()?;
    let entry = items.iter().find(|item| {
        item.as_object()
            .and_then(input_name_field)
            .is_some_and(|key| names.iter().any(|name| name.eq_ignore_ascii_case(key)))
    })?;
    let value = entry
        .as_object()
        .and_then(|object| object.get("value").or_else(|| object.get("Value")))?
        .as_object()?;
    value
        .get("expr")
        .or_else(|| value.get("Expr"))
        .and_then(Value::as_str)
}

/// Render a broker-sent step input expression at job setup.
///
/// Upstream evaluates a step's inputs in `ActionRunner.RunAsync`
/// (@397b032, src/Runner.Worker/ActionRunner.cs:174-185) via
/// `PipelineTemplateEvaluator.EvaluateStepInputs`
/// (src/Sdk/DTPipelines/Pipelines/ObjectTemplating/PipelineTemplateEvaluator.cs:166-191),
/// whose `context.Errors.Check()` throws a `TemplateValidationException` that
/// `StepsRunner` turns into a failed step
/// (src/Runner.Worker/StepsRunner.cs:335-344). Input rendering is therefore
/// **fail-closed** upstream, unlike `${{ }}` interpolation inside an already
/// rendered value. An `Err` here propagates out of `github_script_steps_*` and
/// fails the job (`runner.rs` `step_mapping`).
///
/// The one thing upstream never faces is a context that does not exist yet:
/// it renders at step-run time. Velnor renders once at setup, so a subtree
/// reading `env`/`steps`/`job`/`jobs`/`runner` or a runner function is handed
/// on verbatim as `${{ … }}` for the step-time pass, exactly as
/// `resolve_job_context_expressions` defers such spans in `executor.rs`.
fn render_setup_expression(expr: &str, context_data: &[(String, Value)]) -> Result<String> {
    let context = SetupExpressionContext { context_data };
    let node = expression::parse(expr, &context)
        .with_context(|| format!("evaluating step input expression `{expr}`"))?;
    let Some(node) = node else {
        // `ExpressionParser.cs:60-64` — an empty expression is null, and
        // `convert_to_string` of null is the empty string.
        return Ok(String::new());
    };

    // A `format()` call is the shape GitHub compiles a `run:` body into. Its
    // template carries the script's literal `{`/`}` as `{{`/`}}` escapes, so
    // deferring the whole call verbatim would both re-emit those escapes and
    // truncate the `${{ … }}` span at the first `}}`. Render the template now
    // and defer only the arguments that cannot be evaluated yet.
    if let Node::Function { name, args } = &node
        && name.eq_ignore_ascii_case("format")
        && !args.is_empty()
        && let Some(rendered) = render_format_call(expr, args, &context)?
    {
        return Ok(rendered);
    }

    if expression::reads_runtime_context(&node) {
        return Ok(format!("${{{{ {} }}}}", expr.trim()));
    }
    expression::evaluate_node(&node, &context)
        .map(|value| value.convert_to_string())
        .with_context(|| format!("evaluating step input expression `{expr}`"))
}

/// Render a `format(template, args…)` call, deferring individual arguments.
///
/// Returns `Ok(None)` when the argument spans cannot be recovered verbatim or
/// the template itself is not knowable yet; the caller then falls back to
/// deferring or evaluating the whole tree.
fn render_format_call(
    expr: &str,
    args: &[Node],
    context: &SetupExpressionContext<'_>,
) -> Result<Option<String>> {
    if expression::reads_runtime_context(&args[0]) {
        return Ok(None);
    }
    let Some((_, spans)) = expression::function_call_argument_spans(expr) else {
        return Ok(None);
    };
    if spans.len() != args.len() {
        return Ok(None);
    }
    let template = expression::evaluate_node(&args[0], context)
        .with_context(|| format!("evaluating step input expression `{expr}`"))?
        .convert_to_string();

    let rendered = expression::eval::format_template(&template, args.len() - 1, |index| {
        let argument = &args[index + 1];
        if expression::reads_runtime_context(argument) {
            // Verbatim hand-off to the step-time pass.
            Ok(expression::Value::string(format!(
                "${{{{ {} }}}}",
                spans[index + 1]
            )))
        } else {
            expression::evaluate_node(argument, context)
        }
    })
    .with_context(|| format!("evaluating step input expression `{expr}`"))?;
    Ok(Some(rendered))
}

/// The setup-time expression environment: the job message's context data, and
/// nothing that only exists once steps run.
struct SetupExpressionContext<'a> {
    context_data: &'a [(String, Value)],
}

impl expression::ParseEnvironment for SetupExpressionContext<'_> {
    fn is_named_value(&self, name: &str) -> bool {
        expression::ROOT_CONTEXTS
            .iter()
            .any(|root| root.eq_ignore_ascii_case(name))
    }

    fn function_arity(&self, name: &str) -> Option<(usize, usize)> {
        expression::RUNNER_FUNCTIONS
            .iter()
            .find(|(known, _, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, min, max)| (*min, *max))
    }
}

impl expression::EvaluationContext for SetupExpressionContext<'_> {
    /// A context the job message did not carry is null, which
    /// `convert_to_string` renders as `""` — upstream's coercion
    /// (`Sdk/Value.cs`), not the old resolver's "leave the text alone".
    fn named_value(&self, name: &str) -> expression::Value {
        self.context_data
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| expression::eval::from_serde_json(value))
            .unwrap_or(expression::Value::Null)
    }

    /// Never reached: `reads_runtime_context` defers any tree containing a
    /// runner function before evaluation starts.
    fn call_function(
        &self,
        name: &str,
        _args: &[expression::Value],
    ) -> std::result::Result<expression::Value, expression::ExpressionError> {
        Err(expression::ExpressionError::evaluation(format!(
            "{name}() cannot be evaluated before the job runs"
        )))
    }
}

fn input_value_as_str(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_object()
            .and_then(|object| string_field(object, &["value", "Value", "lit", "Lit"]))
    })
}

fn input_keys(object: &serde_json::Map<String, Value>) -> String {
    if object.is_empty() {
        return "none".to_string();
    }
    object.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn input_summary(object: &serde_json::Map<String, Value>) -> String {
    let mut summary = format!("top=[{}]", input_keys(object));
    if let Some(map) = object.get("map").or_else(|| object.get("Map")) {
        if let Some(map) = map.as_object() {
            summary.push_str(&format!(" map=[{}]", input_keys(map)));
        } else if let Some(items) = map.as_array() {
            let shapes = items
                .iter()
                .take(3)
                .filter_map(|item| item.as_object())
                .map(input_item_summary)
                .collect::<Vec<_>>()
                .join(" | ");
            summary.push_str(&format!(
                " map_array_len={} map_shapes=[{}]",
                items.len(),
                shapes
            ));
        } else {
            summary.push_str(&format!(" map_type={}", json_type(map)));
        }
    }
    summary
}

fn input_item_summary(object: &serde_json::Map<String, Value>) -> String {
    let mut parts = vec![input_keys(object)];
    for name in ["Key", "key", "Value", "value"] {
        let Some(value) = object.get(name) else {
            continue;
        };
        if let Some(inner) = value.as_object() {
            parts.push(format!("{name}=[{}]", input_keys(inner)));
        } else {
            parts.push(format!("{name}={}", json_type(value)));
        }
    }
    parts.join(" ")
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) fn step_continue_on_error(step: &ActionStep) -> bool {
    step.continue_on_error.as_ref().is_some_and(value_truthy)
}

pub(crate) fn step_timeout_minutes(step: &ActionStep) -> Option<u64> {
    step.timeout_in_minutes
        .as_ref()
        .and_then(timeout_minutes_value)
}

pub(crate) fn timeout_minutes_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64().filter(|value| *value > 0),
        Value::String(value) => value.trim().parse::<u64>().ok().filter(|value| *value > 0),
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("Value"))
            .or_else(|| object.get("lit"))
            .or_else(|| object.get("Lit"))
            .and_then(timeout_minutes_value),
        _ => None,
    }
}

pub(crate) fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => value.eq_ignore_ascii_case("true"),
        Value::Number(value) => value.as_i64() == Some(1),
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("Value"))
            .or_else(|| object.get("lit"))
            .or_else(|| object.get("Lit"))
            .is_some_and(value_truthy),
        _ => false,
    }
}

pub(crate) fn step_environment(step: &ActionStep) -> Result<Vec<(String, String)>> {
    environment_pairs(step.environment.as_ref())
}

fn environment_pairs(environment: Option<&Value>) -> Result<Vec<(String, String)>> {
    let Some(environment) = environment else {
        return Ok(Vec::new());
    };
    match environment {
        Value::Object(object) => {
            if object.get("type").or_else(|| object.get("Type")).is_some()
                && object.get("map").or_else(|| object.get("Map")).is_some()
            {
                return environment_pairs(object.get("map").or_else(|| object.get("Map")));
            }
            Ok(object
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("type"))
                .map(|(name, value)| (name.clone(), environment_value(value)))
                .collect())
        }
        Value::Array(items) => Ok(items.iter().filter_map(environment_pair_value).collect()),
        _ => bail!("step environment must be an object"),
    }
}

fn environment_pair_value(value: &Value) -> Option<(String, String)> {
    match value {
        Value::Object(object) => {
            let name = object
                .get("key")
                .or_else(|| object.get("Key"))
                .or_else(|| object.get("name"))
                .or_else(|| object.get("Name"))
                .and_then(input_value_as_str)?;
            let value = object
                .get("value")
                .or_else(|| object.get("Value"))
                .map(environment_value)
                .unwrap_or_default();
            Some((name.to_string(), value))
        }
        Value::Array(pair) if pair.len() == 2 => {
            let name = input_value_as_str(&pair[0])?;
            Some((name.to_string(), environment_value(&pair[1])))
        }
        _ => None,
    }
}

fn environment_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Object(object) => {
            if let Some(expr) = object
                .get("expr")
                .or_else(|| object.get("Expr"))
                .and_then(Value::as_str)
            {
                // Wrap in ${{ }} so resolve_env resolves it at step-run time
                return format!("${{{{ {expr} }}}}");
            }
            object
                .get("value")
                .or_else(|| object.get("Value"))
                .or_else(|| object.get("lit"))
                .or_else(|| object.get("Lit"))
                .map(environment_value)
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

pub(crate) fn github_shell(shell: &str) -> Result<Shell> {
    let shell = shell.split_whitespace().next().unwrap_or(shell);
    if shell.eq_ignore_ascii_case("bash") {
        Ok(Shell::Bash)
    } else if shell.eq_ignore_ascii_case("sh") {
        Ok(Shell::Sh)
    } else {
        bail!("unsupported run step shell '{shell}'")
    }
}

fn workspace_path(workspace_container: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!(
            "{}/{}",
            workspace_container.trim_end_matches('/'),
            path.trim_start_matches("./")
        )
    }
}

fn step_id(step: &ActionStep, index: usize) -> String {
    // Prefer context_name (the YAML `id:` field, e.g. "run-output") over the
    // internal UUID stored in step.id. Expressions like `steps.run-output.outputs.x`
    // reference the YAML id, so the step must be stored under that key.
    // Skip auto-generated names that start with "__" (e.g. "__run", "__run_2").
    step.context_name
        .as_deref()
        .filter(|n| !n.is_empty() && !n.starts_with("__"))
        .or(step.id.as_deref())
        .or(step.name.as_deref())
        .map(sanitize_step_id)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("step{}", index + 1))
}

fn sanitize_step_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ScriptStepPlan {
    pub script_host_path: PathBuf,
    pub script_container_path: String,
    pub shell: Shell,
    pub working_directory_container: String,
    pub env: Vec<(String, String)>,
    command_files: CommandFileSet,
}

impl ScriptStepPlan {
    pub fn prepare(step: &ScriptStep, temp_host: &Path) -> Result<Self> {
        Self::prepare_with_path(step, temp_host, &[])
    }

    pub fn prepare_with_path(
        step: &ScriptStep,
        temp_host: &Path,
        path_prepend: &[String],
    ) -> Result<Self> {
        fs::create_dir_all(temp_host).with_context(|| format!("create {}", temp_host.display()))?;
        let script_name = format!("{}.sh", step.id);
        let script_host_path = temp_host.join(&script_name);
        let script = script_with_path_prelude(&step.script, path_prepend);
        let temp_dir = open_step_file_dir(temp_host)?;
        write_step_file(&temp_dir, &script_name, &script)?;

        let command_files = CommandFileSet::new(&step.id, temp_host);
        command_files.create_empty_files(&temp_dir)?;

        Ok(Self {
            script_host_path,
            script_container_path: format!("/__t/{script_name}"),
            shell: step.shell,
            working_directory_container: step.working_directory_container.clone(),
            env: command_files.env(),
            command_files,
        })
    }

    pub fn collect_state(&self) -> Result<StepCommandState> {
        self.command_files.collect_state()
    }
}

#[derive(Debug, Clone)]
struct CommandFileSet {
    temp_host: PathBuf,
    output: PathMapping,
    env: PathMapping,
    path: PathMapping,
    state: PathMapping,
    summary: PathMapping,
}

impl CommandFileSet {
    fn new(step_id: &str, temp_host: &Path) -> Self {
        Self {
            temp_host: temp_host.to_path_buf(),
            output: PathMapping::new(temp_host, step_id, "output"),
            env: PathMapping::new(temp_host, step_id, "env"),
            path: PathMapping::new(temp_host, step_id, "path"),
            state: PathMapping::new(temp_host, step_id, "state"),
            summary: PathMapping::new(temp_host, step_id, "summary"),
        }
    }

    fn create_empty_files(&self, temp_dir: &NoFollowDestinationDir) -> Result<()> {
        for mapping in [
            &self.output,
            &self.env,
            &self.path,
            &self.state,
            &self.summary,
        ] {
            write_step_file(temp_dir, &mapping.name, "")?;
        }
        Ok(())
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("GITHUB_OUTPUT".into(), self.output.container.clone()),
            ("GITHUB_ENV".into(), self.env.container.clone()),
            ("GITHUB_PATH".into(), self.path.container.clone()),
            ("GITHUB_STATE".into(), self.state.container.clone()),
            ("GITHUB_STEP_SUMMARY".into(), self.summary.container.clone()),
        ]
    }

    fn collect_state(&self) -> Result<StepCommandState> {
        let temp_dir = open_step_file_dir(&self.temp_host)?;
        let mut state = StepCommandState {
            outputs: commands_to_map(parse_command_file_contents(&read_step_file(
                &temp_dir,
                &self.output.name,
            )?)?),
            env: BTreeMap::new(),
            path: read_step_file(&temp_dir, &self.path.name)?
                .lines()
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            state: commands_to_map(parse_command_file_contents(&read_step_file(
                &temp_dir,
                &self.state.name,
            )?)?),
            summary: String::new(),
            masks: Vec::new(),
            annotations: Vec::new(),
            telemetry: Vec::new(),
            error_count: 0,
            warning_count: 0,
            notice_count: 0,
        };
        self.collect_env(&temp_dir, &mut state)?;
        self.collect_summary(&temp_dir, &mut state)?;
        Ok(state)
    }

    /// Read `GITHUB_ENV`.
    ///
    /// Mirrors `SetEnvFileCommand.ProcessCommand` in actions/runner
    /// `src/Runner.Worker/FileCommandManager.cs` (@397b032): every name is
    /// applied except the ones on `_setEnvBlockList`, which holds `NODE_OPTIONS`
    /// alone, and a blocked name is reported as a step error rather than
    /// dropped. Velnor used to drop every `GITHUB_*` and `RUNNER_*` name too,
    /// and silently: a step that exported `GITHUB_TOKEN` for later steps — which
    /// GitHub allows — saw the value vanish with nothing in the log to say why.
    fn collect_env(
        &self,
        temp_dir: &NoFollowDestinationDir,
        state: &mut StepCommandState,
    ) -> Result<()> {
        for command in parse_command_file_contents(&read_step_file(temp_dir, &self.env.name)?)? {
            if let Some(blocked) = blocked_env_file_name(&command.name) {
                state.error_count += 1;
                state.annotations.push(StepAnnotation {
                    level: StepAnnotationLevel::Failure,
                    message: format!(
                        "Can't store {blocked} output parameter using '$GITHUB_ENV' command."
                    ),
                    title: None,
                    path: None,
                    start_line: None,
                    end_line: None,
                    start_column: None,
                    end_column: None,
                });
                continue;
            }
            state.env.insert(command.name, command.value);
        }
        Ok(())
    }

    /// Read `GITHUB_STEP_SUMMARY`, refusing anything over the upload limit.
    ///
    /// Mirrors `CreateStepSummaryCommand.ProcessCommand` in actions/runner
    /// `src/Runner.Worker/FileCommandManager.cs`: a missing or empty file
    /// attaches nothing, and a file larger than `AttachmentSizeLimit` is not
    /// uploaded at all — the step is failed with `UnsupportedSummarySize`
    /// instead, so an oversized summary can never reach the durable blob.
    fn collect_summary(
        &self,
        temp_dir: &NoFollowDestinationDir,
        state: &mut StepCommandState,
    ) -> Result<()> {
        let Some(mut file) = open_step_file(temp_dir, &self.summary.name)? else {
            return Ok(());
        };
        // The size comes from the open descriptor, not from a second lookup by
        // path, so the file that is measured is the file that is read.
        let size = file
            .metadata()
            .with_context(|| format!("stat step file {}", self.summary.name))?
            .len();
        if size == 0 {
            return Ok(());
        }
        if size > STEP_SUMMARY_SIZE_LIMIT {
            state.error_count += 1;
            state.annotations.push(StepAnnotation {
                level: StepAnnotationLevel::Failure,
                message: unsupported_summary_size_message(size),
                title: None,
                path: None,
                start_line: None,
                end_line: None,
                start_column: None,
                end_column: None,
            });
            return Ok(());
        }
        let mut summary = String::new();
        Read::read_to_string(&mut file, &mut summary)
            .with_context(|| format!("read step file {}", self.summary.name))?;
        state.summary = summary;
        Ok(())
    }
}

/// Bind `RUNNER_TEMP` to a directory descriptor, so every later step-file
/// operation is relative to that descriptor rather than to a path a step can
/// re-point.
///
/// `RUNNER_TEMP` itself is a runner-configured root and is treated as trusted:
/// its own path is canonicalized once, which is what lets it sit under a
/// symlinked prefix such as macOS `/var`. Everything *below* it is
/// step-writable and is therefore only ever reached descriptor-relative and
/// `O_NOFOLLOW`.
fn open_step_file_dir(temp_host: &Path) -> Result<NoFollowDestinationDir> {
    NoFollowDestinationDir::open_trusted_rooted_destination(temp_host, Path::new("")).with_context(
        || {
            format!(
                "open step file directory {} without following symlinks",
                temp_host.display()
            )
        },
    )
}

/// Create or replace one step file inside `RUNNER_TEMP`.
///
/// `RUNNER_TEMP` is mode 1777 and every step-file name is derived from the
/// step id, so the path a step will be handed is already predictable to the
/// step running before it. Planting a symlink there and letting the runner's
/// own `fs::write` follow it is an arbitrary host-file write, read and
/// truncate as the runner user, with no race to win.
///
/// `write_file_from_reader` refuses a destination that is not a regular file,
/// stages the content in an `O_CREAT | O_EXCL | O_NOFOLLOW` temporary it
/// created itself, and renames that over the name. A planted symlink is
/// therefore either reported or replaced, never written through.
fn write_step_file(temp_dir: &NoFollowDestinationDir, name: &str, contents: &str) -> Result<()> {
    temp_dir
        .write_file_from_reader(
            &mut contents.as_bytes(),
            Path::new(name),
            contents.len() as u64,
            STEP_FILE_MODE,
        )
        .with_context(|| format!("write step file {name}"))?;
    Ok(())
}

/// Open one step file for reading, or `None` when the step removed it.
///
/// The open is descriptor-relative and `O_NOFOLLOW`, and a name that is not a
/// regular file is an error rather than a redirect: a step cannot make the
/// runner read a host file by leaving a symlink behind.
fn open_step_file(temp_dir: &NoFollowDestinationDir, name: &str) -> Result<Option<fs::File>> {
    temp_dir
        .open_relative_file_if_exists(Path::new(name))
        .with_context(|| format!("open step file {name}"))
}

fn read_step_file(temp_dir: &NoFollowDestinationDir, name: &str) -> Result<String> {
    let Some(mut file) = open_step_file(temp_dir, name)? else {
        return Ok(String::new());
    };
    let mut contents = String::new();
    Read::read_to_string(&mut file, &mut contents)
        .with_context(|| format!("read step file {name}"))?;
    Ok(contents)
}

/// Step files keep the owner-writable, world-readable mode the runner's umask
/// already produced for them.
const STEP_FILE_MODE: u16 = 0o644;

/// `CreateStepSummaryCommand.AttachmentSizeLimit` in actions/runner
/// `src/Runner.Worker/FileCommandManager.cs`: 1 MiB.
pub const STEP_SUMMARY_SIZE_LIMIT: u64 = 1024 * 1024;

/// `Constants.Runner.UnsupportedSummarySize` in actions/runner
/// `src/Runner.Common/Constants.cs`, formatted with the same two operands:
/// the limit and the observed size, both in whole kibibytes.
fn unsupported_summary_size_message(size: u64) -> String {
    format!(
        "$GITHUB_STEP_SUMMARY upload aborted, supports content up to a size of {}k, got {}k. \
         For more information see: \
         https://docs.github.com/actions/using-workflows/workflow-commands-for-github-actions#adding-a-markdown-summary",
        STEP_SUMMARY_SIZE_LIMIT / 1024,
        size / 1024
    )
}

#[derive(Debug, Clone)]
struct PathMapping {
    /// File name inside `RUNNER_TEMP`. Every access goes through the directory
    /// descriptor by this name; `host` is only for display and for callers
    /// that need the mount-side path.
    name: String,
    host: PathBuf,
    container: String,
}

impl PathMapping {
    fn new(temp_host: &Path, step_id: &str, name: &str) -> Self {
        let file_name = format!("{step_id}_{name}");
        Self {
            host: temp_host.join(&file_name),
            container: format!("/__t/{file_name}"),
            name: file_name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StepCommandState {
    pub outputs: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub path: Vec<String>,
    pub state: BTreeMap<String, String>,
    pub summary: String,
    pub masks: Vec<String>,
    pub annotations: Vec<StepAnnotation>,
    pub telemetry: Vec<StepCommandTelemetry>,
    pub error_count: i32,
    pub warning_count: i32,
    pub notice_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCommandTelemetry {
    pub message: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAnnotation {
    pub level: StepAnnotationLevel,
    pub message: String,
    pub title: Option<String>,
    pub path: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub start_column: Option<i64>,
    pub end_column: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepAnnotationLevel {
    Notice,
    Warning,
    Failure,
}

impl StepCommandState {
    pub fn merge(&mut self, other: StepCommandState) {
        self.outputs.extend(other.outputs);
        self.env.extend(other.env);
        self.path.extend(other.path);
        self.state.extend(other.state);
        self.masks.extend(other.masks);
        self.annotations.extend(other.annotations);
        for telemetry in other.telemetry {
            if !self.telemetry.contains(&telemetry) {
                self.telemetry.push(telemetry);
            }
        }
        self.error_count += other.error_count;
        self.warning_count += other.warning_count;
        self.notice_count += other.notice_count;
        if !other.summary.is_empty() {
            self.summary.push_str(&other.summary);
        }
    }
}

fn commands_to_map(commands: Vec<FileCommand>) -> BTreeMap<String, String> {
    commands
        .into_iter()
        .map(|command| (command.name, command.value))
        .collect()
}

/// `SetEnvFileCommand._setEnvBlockList` (@397b032,
/// src/Runner.Worker/FileCommandManager.cs).
const SET_ENV_FILE_BLOCK_LIST: &[&str] = &["NODE_OPTIONS"];

fn blocked_env_file_name(name: &str) -> Option<&'static str> {
    SET_ENV_FILE_BLOCK_LIST
        .iter()
        .find(|blocked| blocked.eq_ignore_ascii_case(name))
        .copied()
}

fn fix_script(script: &str) -> String {
    let mut fixed = script.replace("\r\n", "\n");
    if !fixed.ends_with('\n') {
        fixed.push('\n');
    }
    fixed
}

fn script_with_path_prelude(script: &str, path_prepend: &[String]) -> String {
    let fixed = fix_script(script);
    // HOME/RUSTUP_HOME/CARGO_HOME are asserted as explicit `docker exec -e`
    // base env (JobContainerSpec::append_base_exec_env): HOME=/github/home is
    // the bind-mounted job home, so `~` state written by steps persists on the
    // host and the actions/cache adapter sees it. The old prelude here
    // exported HOME=/root + CARGO_HOME=/root/.cargo (an OrbStack dev-host
    // workaround), which silently redirected every cargo download into the
    // unmounted container /root — the cargo-registry cache could never save
    // and warm restores were invisible to steps. Native mise setup prepends its
    // repository-selected shims/tool bins ahead of the image-baked rustup
    // fallback. The exec base environment independently asserts a safe PATH
    // with the real rustup proxy before mise's generic shims.
    let mut prelude = Vec::new();
    if !path_prepend.is_empty() {
        let joined = path_prepend
            .iter()
            .map(|path| shell_single_quote(path))
            .collect::<Vec<_>>()
            .join(":");
        prelude.push(format!("export PATH={joined}:\"$PATH\""));
    }
    if prelude.is_empty() {
        return fixed;
    }
    format!("{}\n{fixed}", prelude.join("\n"))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_step_dir() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("velnor-step-test-{}-{}", std::process::id(), id))
    }

    fn sample_step() -> ScriptStep {
        ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo test".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        }
    }

    /// `RUNNER_TEMP` is mode 1777 and step-file names are derived from the step
    /// id, so one step can plant a symlink at the path the next step's files
    /// will use. Neither the write nor the read may follow it.
    #[cfg(unix)]
    #[test]
    fn step_files_never_follow_a_planted_symlink() {
        let temp = temp_step_dir();
        fs::create_dir_all(&temp).unwrap();
        let victim = temp.join("victim");
        fs::write(&victim, "host secret").unwrap();

        for name in ["step1.sh", "step1_output", "step1_summary"] {
            let planted = temp.join(name);
            let _ = fs::remove_file(&planted);
            std::os::unix::fs::symlink(&victim, &planted).unwrap();

            let error = ScriptStepPlan::prepare(&sample_step(), &temp)
                .expect_err("preparing a step over a planted symlink must not write through it");
            assert!(
                format!("{error:#}").contains("symlink"),
                "unexpected error: {error:#}"
            );
            assert_eq!(fs::read_to_string(&victim).unwrap(), "host secret");
            assert!(fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink());
            fs::remove_file(&planted).unwrap();
        }

        // A step that replaces its own command file with a symlink after the
        // runner created it must not make the runner read the target back.
        let plan = ScriptStepPlan::prepare(&sample_step(), &temp).unwrap();
        let summary = temp.join("step1_summary");
        fs::remove_file(&summary).unwrap();
        std::os::unix::fs::symlink(&victim, &summary).unwrap();
        let error = plan
            .collect_state()
            .expect_err("collecting state through a planted symlink must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("not a regular file"),
            "unexpected error: {rendered}"
        );
        assert!(!rendered.contains("host secret"));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn prepares_script_and_command_file_env() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo hello".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };

        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        assert_eq!(
            fs::read_to_string(&plan.script_host_path).unwrap(),
            "echo hello\n"
        );
        assert!(plan
            .env
            .contains(&("GITHUB_OUTPUT".into(), "/__t/step1_output".into())));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn collects_command_file_state() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo test".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        fs::write(
            temp.join("step1_output"),
            "answer=42\nmulti<<EOF\none\ntwo\nEOF\n",
        )
        .unwrap();
        fs::write(
            temp.join("step1_env"),
            "NAME=value\nGITHUB_REF=evil\nRUNNER_TEMP=/bad\nNODE_OPTIONS=--require bad\nACTIONS_RUNTIME_URL=https://runtime\n",
        )
        .unwrap();
        fs::write(temp.join("step1_path"), "/opt/tool\n\n").unwrap();
        fs::write(temp.join("step1_state"), "cleanup=yes\n").unwrap();
        fs::write(temp.join("step1_summary"), "summary text").unwrap();

        let state = plan.collect_state().unwrap();

        assert_eq!(state.outputs["answer"], "42");
        assert_eq!(state.outputs["multi"], "one\ntwo");
        assert_eq!(state.env["NAME"], "value");
        assert_eq!(state.env["ACTIONS_RUNTIME_URL"], "https://runtime");
        // Upstream blocks NODE_OPTIONS alone, loudly; every other name applies.
        assert_eq!(state.env["GITHUB_REF"], "evil");
        assert_eq!(state.env["RUNNER_TEMP"], "/bad");
        assert!(!state.env.contains_key("NODE_OPTIONS"));
        assert_eq!(state.error_count, 1);
        assert_eq!(
            state.annotations[0].message,
            "Can't store NODE_OPTIONS output parameter using '$GITHUB_ENV' command."
        );
        assert_eq!(state.annotations[0].level, StepAnnotationLevel::Failure);
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.state["cleanup"], "yes");
        assert_eq!(state.summary, "summary text");
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn oversized_step_summary_is_refused_and_fails_the_step() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "echo test".into(),
            shell: Shell::Sh,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let plan = ScriptStepPlan::prepare(&step, &temp).unwrap();

        // Exactly at the limit is still uploaded.
        let at_limit = "a".repeat(STEP_SUMMARY_SIZE_LIMIT as usize);
        fs::write(temp.join("step1_summary"), &at_limit).unwrap();
        let state = plan.collect_state().unwrap();
        assert_eq!(state.summary.len(), STEP_SUMMARY_SIZE_LIMIT as usize);
        assert_eq!(state.error_count, 0);
        assert!(state.annotations.is_empty());

        // One byte over is dropped entirely and reported as a step error,
        // matching CreateStepSummaryCommand.ProcessCommand.
        fs::write(temp.join("step1_summary"), format!("{at_limit}a")).unwrap();
        let state = plan.collect_state().unwrap();
        assert_eq!(state.summary, "");
        assert_eq!(state.error_count, 1);
        assert_eq!(state.annotations.len(), 1);
        assert_eq!(state.annotations[0].level, StepAnnotationLevel::Failure);
        assert!(state.annotations[0]
            .message
            .starts_with("$GITHUB_STEP_SUMMARY upload aborted, supports content up to a size of 1024k, got 1024k."));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn path_prelude_keeps_existing_shell_path() {
        let temp = temp_step_dir();
        let step = ScriptStep {
            id: "step1".into(),
            display_name: String::new(),
            script: "tool --version".into(),
            shell: Shell::Bash,
            working_directory_container: "/__w/repo".into(),
            env: Vec::new(),
            condition: None,
            continue_on_error: false,
            timeout_minutes: None,
        };
        let plan = ScriptStepPlan::prepare_with_path(
            &step,
            &temp,
            &["/opt/bin".to_string(), "/path/with'quote".to_string()],
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&plan.script_host_path).unwrap(),
            "export PATH='/opt/bin':'/path/with'\\''quote':\"$PATH\"\ntool --version\n"
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn maps_github_script_steps_to_internal_steps() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "displayName": "Run tests",
                "enabled": true,
                "continueOnError": true,
                "timeoutInMinutes": { "value": "7" },
                "reference": { "type": "Script" },
                "inputs": {
                    "script": "cargo test",
                    "shell": "bash",
                    "workingDirectory": "./crates"
                },
                "environment": {
                    "CARGO_TERM_COLOR": "always",
                    "CARGO_INCREMENTAL": 0,
                    "RENOVATE_ONBOARDING": false,
                    "TOKEN": "${{ github.token }}"
                }
            },
            {
                "id": "checkout",
                "reference": { "type": "Repository", "name": "actions/checkout" }
            },
            {
                "id": "disabled",
                "enabled": false,
                "reference": { "type": "Script" },
                "inputs": { "script": "echo skip" }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].id, "run-1");
        assert_eq!(mapped[0].display_name, "Run tests");
        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
        assert_eq!(
            mapped[0].env,
            vec![
                ("CARGO_INCREMENTAL".into(), "0".into()),
                ("CARGO_TERM_COLOR".into(), "always".into()),
                ("RENOVATE_ONBOARDING".into(), "false".into()),
                ("TOKEN".into(), "${{ github.token }}".into()),
            ]
        );
        assert!(mapped[0].continue_on_error);
        assert_eq!(mapped[0].timeout_minutes, Some(7));
    }

    #[test]
    fn maps_timeout_minutes_from_run_service_value_shapes() {
        assert_eq!(timeout_minutes_value(&serde_json::json!(12)), Some(12));
        assert_eq!(timeout_minutes_value(&serde_json::json!("15")), Some(15));
        assert_eq!(
            timeout_minutes_value(&serde_json::json!({"Value": 30})),
            Some(30)
        );
        assert_eq!(timeout_minutes_value(&serde_json::json!("0")), None);
        assert_eq!(
            timeout_minutes_value(&serde_json::json!("not-a-number")),
            None
        );
    }

    #[test]
    fn script_step_ignores_step_id_in_name_for_display_name() {
        // GitHub's Name field carries the YAML step id (e.g. `msrv`), never a
        // display name; actions/runner shows `Run <first script line>` for
        // unnamed steps (verified against the live GitHub-hosted lane).
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "tests",
                "name": "msrv",
                "reference": { "type": "Script" },
                "inputs": { "script": "set -euo pipefail\ncargo check" }
            },
            {
                "id": "fallback",
                "name": "__run",
                "reference": { "type": "Script" },
                "inputs": { "script": "echo fallback" }
            },
            {
                "id": "named",
                "name": "__run_2",
                "displayName": "Tests",
                "reference": { "type": "Script" },
                "inputs": { "script": "cargo nextest run -p app --profile ci" }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].display_name, "Run set -euo pipefail");
        assert_eq!(mapped[1].display_name, "Run echo fallback");
        assert_eq!(mapped[2].display_name, "Tests");
    }

    #[test]
    fn script_step_input_source_line_reads_broker_literal_line() {
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "name": "__run",
            "reference": { "type": "Script" },
            "inputs": {
                "map": [{
                    "Key": { "lit": "script", "type": 0 },
                    "Value": {
                        "col": 14,
                        "file": 1,
                        "line": 71,
                        "lit": "pip install ansible-core",
                        "type": 0
                    }
                }],
                "type": 2
            }
        }))
        .unwrap();

        assert_eq!(script_input_source_line(&step), Some(71));
    }

    #[test]
    fn maps_run_service_typed_continue_on_error() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "soft",
                "reference": { "type": "Script" },
                "inputs": { "script": "cargo install sccache" },
                "continueOnError": { "value": true }
            },
            {
                "id": "strict",
                "reference": { "type": "Script" },
                "inputs": { "script": "cargo test" },
                "continueOnError": { "lit": "false" }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert!(mapped[0].continue_on_error);
        assert!(!mapped[1].continue_on_error);
    }

    #[test]
    fn maps_run_service_capitalized_script_inputs() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "Script": { "Value": "cargo test" },
                    "Shell": { "Value": "bash" },
                    "WorkingDirectory": { "Value": "./crates" }
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
    }

    #[test]
    fn maps_run_service_nested_input_map() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": {
                        "script": "cargo test",
                        "shell": "bash",
                        "workingDirectory": "./crates"
                    }
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
    }

    #[test]
    fn maps_run_service_input_map_array() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "script", "type": 0 }, "Value": { "lit": "cargo test", "type": 0 } },
                        { "Key": { "lit": "shell", "type": 0 }, "Value": { "lit": "bash", "type": 0 } },
                        { "Key": { "lit": "workingDirectory", "type": 0 }, "Value": { "lit": "./crates", "type": 0 } }
                    ]
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(mapped[0].script, "cargo test");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(mapped[0].working_directory_container, "/__w/repo/crates");
    }

    #[test]
    fn maps_run_service_typed_step_environment() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "run-1",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "script" }, "Value": { "lit": "cargo test" } }
                    ]
                },
                "environment": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "CARGO_TERM_COLOR" }, "Value": { "lit": "always" } },
                        { "Key": { "lit": "CARGO_INCREMENTAL" }, "Value": { "value": 0 } },
                        { "Key": { "lit": "RENOVATE_ONBOARDING" }, "Value": { "value": false } },
                        { "Key": { "lit": "TOKEN" }, "Value": { "lit": "${{ github.token }}" } }
                    ]
                }
            }
        ]))
        .unwrap();

        let mapped = github_script_steps(&steps, "/__w/repo").unwrap();

        assert_eq!(
            mapped[0].env,
            vec![
                ("CARGO_TERM_COLOR".into(), "always".into()),
                ("CARGO_INCREMENTAL".into(), "0".into()),
                ("RENOVATE_ONBOARDING".into(), "false".into()),
                ("TOKEN".into(), "${{ github.token }}".into()),
            ]
        );
    }

    #[test]
    fn applies_job_run_defaults_to_script_steps() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "ansible",
                "reference": { "type": "Script" },
                "inputs": { "script": "ansible-playbook --syntax-check site.yml" }
            },
            {
                "id": "override",
                "reference": { "type": "Script" },
                "inputs": {
                    "script": "cargo test",
                    "shell": "sh",
                    "workingDirectory": "./backend-rust"
                }
            }
        ]))
        .unwrap();
        let defaults = vec![serde_json::json!({
            "run": {
                "shell": "bash",
                "working-directory": "./ansible-configs"
            }
        })];

        let mapped = github_script_steps_with_defaults(&steps, "/__w/repo", &defaults).unwrap();

        assert_eq!(mapped[0].id, "ansible");
        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(
            mapped[0].working_directory_container,
            "/__w/repo/ansible-configs"
        );
        assert!(matches!(mapped[1].shell, Shell::Sh));
        assert_eq!(
            mapped[1].working_directory_container,
            "/__w/repo/backend-rust"
        );
    }

    #[test]
    fn applies_run_service_typed_job_run_defaults() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "id": "kestra",
                "reference": { "type": "Script" },
                "inputs": {
                    "type": "map",
                    "map": [
                        { "Key": { "lit": "script" }, "Value": { "lit": "just build-jvm-base" } }
                    ]
                }
            }
        ]))
        .unwrap();
        let defaults = vec![serde_json::json!({
            "type": "map",
            "map": [
                {
                    "Key": { "lit": "run" },
                    "Value": {
                        "type": "map",
                        "map": [
                            { "Key": { "lit": "shell" }, "Value": { "lit": "bash" } },
                            { "Key": { "lit": "workingDirectory" }, "Value": { "lit": "./kestra-docker-containers" } }
                        ]
                    }
                }
            ]
        })];

        let mapped = github_script_steps_with_defaults(&steps, "/__w/repo", &defaults).unwrap();

        assert!(matches!(mapped[0].shell, Shell::Bash));
        assert_eq!(
            mapped[0].working_directory_container,
            "/__w/repo/kestra-docker-containers"
        );
    }

    #[test]
    fn rejects_unsupported_github_shell() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "reference": { "type": "Script" },
                "inputs": { "script": "echo hi", "shell": "pwsh" }
            }
        ]))
        .unwrap();

        let error = github_script_steps(&steps, "/__w/repo").unwrap_err();

        assert!(error.to_string().contains("unsupported run step shell"));
    }

    #[test]
    fn detects_enabled_non_script_steps() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "enabled": false,
                "reference": { "type": "Repository", "name": "actions/checkout" }
            },
            {
                "enabled": true,
                "reference": { "type": "Script" },
                "inputs": { "script": "echo hi" }
            }
        ]))
        .unwrap();

        assert!(!has_enabled_non_script_steps(&steps));

        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "enabled": true,
                "reference": { "type": "Repository", "name": "actions/checkout" }
            }
        ]))
        .unwrap();

        assert!(has_enabled_non_script_steps(&steps));
    }

    #[test]
    fn evaluates_format_expr_with_context() {
        let result = render_setup_expression(
            "format('just clippy \"{0}\"', matrix.package)",
            &[(
                "matrix".to_string(),
                serde_json::json!({"package": "app-b"}),
            )],
        )
        .unwrap();
        assert_eq!(result, "just clippy \"app-b\"");
    }

    #[test]
    fn evaluates_format_expr_with_escaped_braces() {
        // {{ and }} in GitHub format() are escape sequences for literal { and }
        let expr = "format('{{\\n  echo stats ({0})\\n}} >> \"${{ENV}}\"\\n', matrix.config.lane)";
        let result = render_setup_expression(
            expr,
            &[(
                "matrix".to_string(),
                serde_json::json!({"config": {"lane": "velnor"}}),
            )],
        )
        .unwrap();
        let expected = "{\\n  echo stats (velnor)\\n} >> \"${ENV}\"\\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn evaluates_format_expr_multi_arg() {
        let result = render_setup_expression(
            "format('python3 write.py \"{0}\" \"{1}\"', matrix.package, matrix.config.lane)",
            &[(
                "matrix".to_string(),
                serde_json::json!({"package": "app-a", "config": {"lane": "velnor"}}),
            )],
        )
        .unwrap();
        assert_eq!(result, "python3 write.py \"app-a\" \"velnor\"");
    }

    fn matrix_context(value: serde_json::Value) -> Vec<(String, Value)> {
        vec![("matrix".to_string(), value)]
    }

    /// D3 — `Sdk/Value.cs` truthiness: every non-empty string is truthy,
    /// `"0"` and `"false"` included. The deleted resolver had no truthiness at
    /// all, so a `&&`/`||` in a step input silently rewrote to text.
    #[test]
    fn setup_expression_string_truthiness_matches_upstream() {
        let context = matrix_context(serde_json::json!({"zero": "0", "no": "false", "empty": ""}));
        assert_eq!(
            render_setup_expression("matrix.zero && 'yes'", &context).unwrap(),
            "yes"
        );
        assert_eq!(
            render_setup_expression("matrix.no && 'yes'", &context).unwrap(),
            "yes"
        );
        assert_eq!(
            render_setup_expression("matrix.empty && 'yes'", &context).unwrap(),
            ""
        );
    }

    /// D4 — a value the job message never carried is null, and null converts
    /// to the empty string (`Sdk/Value.cs`). The deleted resolver returned
    /// `None` for a missing path, which the caller turned into a literal
    /// `${{ … }}` left in the script.
    #[test]
    fn setup_expression_missing_value_coerces_to_empty_string() {
        let context = matrix_context(serde_json::json!({"package": "app"}));
        assert_eq!(
            render_setup_expression("format('[{0}]', matrix.absent)", &context).unwrap(),
            "[]"
        );
        assert_eq!(
            render_setup_expression("format('[{0}]', matrix.absent.deeper)", &context).unwrap(),
            "[]"
        );
    }

    /// D5 — relational operators. The deleted resolver had none; `>` was just
    /// text it could not resolve.
    #[test]
    fn setup_expression_supports_relational_operators() {
        let context = matrix_context(serde_json::json!({"version": 14, "name": "beta"}));
        assert_eq!(
            render_setup_expression("matrix.version >= 12", &context).unwrap(),
            "true"
        );
        assert_eq!(
            render_setup_expression("matrix.version < 12", &context).unwrap(),
            "false"
        );
        // Upstream compares a string to a number by coercing the string
        // (`Sdk/Value.cs`), so a non-numeric string is NaN and every relation
        // is false.
        assert_eq!(
            render_setup_expression("matrix.name > 1", &context).unwrap(),
            "false"
        );
    }

    /// D6 — the function set the deleted resolver did not have.
    #[test]
    fn setup_expression_supports_the_full_function_set() {
        let context = matrix_context(serde_json::json!({
            "package": "velnor-runner",
            "list": ["a", "b"],
            "json": "{\"lane\": \"fast\"}"
        }));
        assert_eq!(
            render_setup_expression("startsWith(matrix.package, 'velnor')", &context).unwrap(),
            "true"
        );
        assert_eq!(
            render_setup_expression("endsWith(matrix.package, 'runner')", &context).unwrap(),
            "true"
        );
        assert_eq!(
            render_setup_expression("join(matrix.list, '+')", &context).unwrap(),
            "a+b"
        );
        assert_eq!(
            render_setup_expression("fromJSON(matrix.json).lane", &context).unwrap(),
            "fast"
        );
        assert_eq!(
            render_setup_expression("contains(matrix.package, 'run')", &context).unwrap(),
            "true"
        );
    }

    /// A span that reads a context which does not exist until steps run is
    /// deferred verbatim to the step-time pass — per argument, so the
    /// template's `{{`/`}}` escapes are still resolved now and the deferred
    /// span carries no nested `}}`.
    #[test]
    fn setup_expression_defers_runtime_context_arguments_verbatim() {
        let context = matrix_context(serde_json::json!({"package": "app"}));
        assert_eq!(
            render_setup_expression(
                "format('echo {0} ${{{0}}} {1}', matrix.package, steps.build.outputs.sha)",
                &context
            )
            .unwrap(),
            "echo app ${app} ${{ steps.build.outputs.sha }}"
        );
        // A whole tree that reads runtime state and is not a format() call is
        // handed on unchanged.
        assert_eq!(
            render_setup_expression("env.MODE == 'release'", &context).unwrap(),
            "${{ env.MODE == 'release' }}"
        );
    }

    /// Fail-closed: upstream's `EvaluateStepInputs` runs
    /// `context.Errors.Check()`, which throws and fails the step
    /// (`PipelineTemplateEvaluator.cs:166-191`,
    /// `StepsRunner.cs:335-344`). An unevaluatable span must not be passed
    /// through as text the shell then runs.
    #[test]
    fn setup_expression_is_fail_closed() {
        let context = matrix_context(serde_json::json!({"package": "app"}));
        // Unrecognized named-value (`ExpressionParser.cs:144-147`).
        assert!(render_setup_expression("nope.value", &context).is_err());
        // Unrecognized function.
        assert!(render_setup_expression("bogus('x')", &context).is_err());
        // Unexpected end of expression.
        assert!(render_setup_expression("format('{0}', ", &context).is_err());
        // A format string referencing an argument that was not supplied
        // (`Format.cs:41`).
        assert!(render_setup_expression("format('{0} {1}', matrix.package)", &context).is_err());

        // And it fails the step mapping, not just the expression.
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([{
            "enabled": true,
            "reference": { "type": "Script" },
            "inputs": {
                "map": [{
                    "Key": {"lit": "script", "type": 0},
                    "Value": {"expr": "format('{0} {1}', matrix.package)", "type": 3}
                }],
                "type": 2
            }
        }]))
        .unwrap();
        assert!(github_script_steps_with_context(&steps, "/__w", &[], &context).is_err());
    }

    #[test]
    fn maps_script_steps_with_format_expr() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([{
            "enabled": true,
            "reference": { "type": "Script" },
            "inputs": {
                "map": [{
                    "Key": {"lit": "script", "type": 0},
                    "Value": {
                        "col": 14,
                        "expr": "format('just clippy \"{0}\"', matrix.package)",
                        "type": 3
                    }
                }],
                "type": 2
            }
        }]))
        .unwrap();

        let context = vec![(
            "matrix".to_string(),
            serde_json::json!({"package": "app-b"}),
        )];
        let script_steps = github_script_steps_with_context(&steps, "/__w", &[], &context).unwrap();
        assert_eq!(script_steps.len(), 1);
        // matrix.package is resolvable from context_data → eagerly substituted.
        assert_eq!(script_steps[0].script, "just clippy \"app-b\"");
    }

    #[test]
    fn maps_working_directory_with_matrix_format_expr() {
        let steps: Vec<ActionStep> = serde_json::from_value(serde_json::json!([
            {
                "enabled": true,
                "reference": { "type": "Script" },
                "inputs": {
                    "map": [
                        {
                            "Key": { "lit": "script", "type": 0 },
                            "Value": { "lit": "./gradlew test", "type": 0 }
                        },
                        {
                            "Key": { "lit": "workingDirectory", "type": 0 },
                            "Value": {
                                "expr": "format('services/{0}', matrix.service)",
                                "type": 3
                            }
                        }
                    ],
                    "type": 2
                }
            },
            {
                "enabled": true,
                "reference": { "type": "Script" },
                "inputs": {
                    "script": "./gradlew test",
                    "workingDirectory": "services/literal"
                }
            }
        ]))
        .unwrap();

        let context = vec![(
            "matrix".to_string(),
            serde_json::json!({"service": "catalog"}),
        )];
        let defaults = vec![serde_json::json!({
            "run": { "working-directory": "services/default" }
        })];
        let script_steps =
            github_script_steps_with_context(&steps, "/__w", &defaults, &context).unwrap();

        // Step input expressions are evaluated first and override job defaults,
        // matching actions/runner's evaluated input -> default lookup order.
        assert_eq!(
            script_steps[0].working_directory_container,
            "/__w/services/catalog"
        );
        assert_eq!(
            script_steps[1].working_directory_container,
            "/__w/services/literal"
        );
    }
}
