//! Generic package-update rendering for adopted workflow templates.
//!
//! Everything here is config-driven: the providers, the selectors, and the
//! channel grants all come from the scanned repository's own generation
//! config. The crate carries no repository catalog.

use crate::s2::provider::{ProviderId, ProviderSet, SelectorMap};
use crate::s2::ProjectConfig;

/// The runner selector the generator's earliest generated surfaces embedded
/// before the selectors moved into each repository's generation config.
/// Static-template adoption replaces it with the declaring repository's own
/// selector.
pub(crate) const LEGACY_VELNOR_RUNNER_SELECTOR: &str =
    "fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')";

/// The owners that mirror the `velnor-actions` fleet. A reusable workflow
/// from a fleet mirror, pinned by full commit SHA, is content-addressed
/// exactly like a SHA-pinned external action; anything else reusable stays
/// rejected. The mirrors are deployment facts of the adopting estate, so
/// they live at this admitted boundary instead of the generic engine.
pub(crate) const FLEET_VELNOR_ACTION_OWNERS: &[&str] =
    &["jackin-project", "tailrocks", "ChainArgos"];

/// Adopted provider-selection `runs-on` shapes, whitespace-normalized for
/// comparison. Every shape resolves to either the hosted `ubuntu-26.04`
/// label or the adopting estate's declared local labels; the provider
/// shapes additionally map every `pull_request` evaluation to the hosted
/// label, so untrusted pull requests never resolve to the persistent pool.
/// Selectors reference only the event name and the manual `providers` input,
/// and matrix shapes reference only the job matrix the repository's own
/// producer jobs compute from the same trusted inputs. Anything else
/// dynamic stays rejected.
pub(crate) const APPROVED_DYNAMIC_RUNNERS: &[&str] = &[
    "((github.event_name=='workflow_dispatch'&&contains(format(',{0},',inputs.providers),',github-hosted,'))||github.event_name=='pull_request'||github.event_name=='push')&&'ubuntu-26.04'||fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')",
    "((github.event_name=='workflow_dispatch'&&!contains(format(',{0},',inputs.providers),',github-self-hosted,'))||github.event_name=='pull_request'||github.event_name=='push')&&'ubuntu-26.04'||fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')",
    "((github.event_name=='workflow_dispatch'&&contains(format(',{0},',inputs.providers),',github-hosted,'))||github.event_name=='pull_request'||github.event_name=='merge_group'||github.event_name=='push')&&'ubuntu-26.04'||fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')",
    "matrix.config.runner",
    "fromJSON(matrix.config.runner)",
];

pub(crate) fn render_apt_package_updater_template(
    template: &str,
    default_branch: &str,
    selectors: &SelectorMap,
    universe: &ProviderSet,
) -> String {
    let Some((prefix, jobs)) = template.split_once("\n  verify:\n") else {
        return normalize_apt_package_updater_static_template(template, selectors);
    };
    let Some((verify_body, mutate_body)) = jobs.split_once("\n  mutate:\n") else {
        return template.to_owned();
    };
    let trusted_gate = apt_local_trusted_gate(default_branch);
    let mut rendered = String::new();
    for (body, job) in [
        (verify_body, AptPackageUpdaterJob::Verify),
        (mutate_body, AptPackageUpdaterJob::Mutate),
    ] {
        for provider in universe {
            let id = format!(
                "{}-{}",
                match job {
                    AptPackageUpdaterJob::Verify => "verify",
                    AptPackageUpdaterJob::Mutate => "mutate",
                },
                provider.as_str()
            );
            rendered.push_str(&render_apt_package_updater_job(
                &id,
                body,
                job,
                *provider,
                &trusted_gate,
                selectors,
            ));
            rendered.push('\n');
        }
    }
    format!("{prefix}\n{}", rendered.trim_end_matches('\n'))
}

fn normalize_apt_package_updater_static_template(
    template: &str,
    selectors: &SelectorMap,
) -> String {
    let hosted = selectors.get(&ProviderId::GithubHosted);
    let mut output = String::with_capacity(template.len());
    let mut hosted_job = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        if indentation == 2 && trimmed.ends_with(':') {
            hosted_job = matches!(trimmed, "verify-github-hosted:" | "mutate-github-hosted:");
        }
        if hosted_job
            && indentation == 4
            && trimmed.starts_with("runs-on:")
            && trimmed != "runs-on:"
        {
            let Some(selector) = hosted else {
                return template.to_owned();
            };
            output.push_str(&line[..indentation]);
            output.push_str("runs-on: ");
            output.push_str(&crate::s2::yaml_scalar(
                &selector.runs_on.first().cloned().unwrap_or_default(),
            ));
            if segment.ends_with('\n') {
                output.push('\n');
            }
        } else {
            output.push_str(segment);
        }
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AptPackageUpdaterJob {
    Verify,
    Mutate,
}

/// The runner placement every provider of a rendered `package-update.yml`
/// selects from: the per-provider selectors the repo declares.
fn render_apt_package_updater_job(
    id: &str,
    body: &str,
    job: AptPackageUpdaterJob,
    provider: ProviderId,
    trusted_gate: &str,
    selectors: &SelectorMap,
) -> String {
    let provider_name = provider.as_str();
    let trusted_condition = if provider.is_local() {
        format!(" && {trusted_gate}")
    } else {
        String::new()
    };
    let mut body = body.to_owned();
    match job {
        AptPackageUpdaterJob::Verify => {
            body = body.replace(
                "    if: ${{ inputs.consumer-repository != '' }}",
                &format!(
                    "    if: ${{{{ inputs.consumer-repository != '' && inputs.provider == '{provider_name}'{trusted_condition} }}}}"
                ),
            );
            // Adopted templates that predate the provider schema select on
            // `inputs.lane`; rewrite the same predicate onto `inputs.provider`.
            body = body.replace(
                &format!("inputs.lane == '{provider_name}'"),
                &format!("inputs.provider == '{provider_name}'"),
            );
        }
        AptPackageUpdaterJob::Mutate => {
            body = body.replace(
                "    needs: verify",
                &format!("    needs: verify-{provider_name}"),
            );
            body = body.replace(
                "    if: ${{ needs.verify.outputs.available == 'true' && inputs.writer }}",
                &format!(
                    "    if: ${{{{ needs.verify-{provider_name}.outputs.available == 'true' && inputs.writer{trusted_condition} }}}}"
                ),
            );
            // The mutate body can consume more than the availability output.
            // Rewrite every remaining dependency reference after renaming the
            // verify job so split providers cannot retain a dangling needs.verify.
            body = body.replace("needs.verify.", &format!("needs.verify-{provider_name}."));
        }
    }
    format!(
        "  {id}:\n{}",
        replace_apt_package_updater_runner(&body, provider, selectors)
    )
}

fn replace_apt_package_updater_runner(
    body: &str,
    provider: ProviderId,
    selectors: &SelectorMap,
) -> String {
    let mut output = String::with_capacity(body.len() + 96);
    let mut replaced = false;
    for segment in body.split_inclusive('\n') {
        let runner_line = segment.strip_suffix('\n').unwrap_or(segment);
        if runner_line.trim_start().starts_with("runs-on:")
            && runner_line.contains("${{")
            && (runner_line.contains("inputs.provider") || runner_line.contains("inputs.lane"))
        {
            let Some(selector) = selectors.get(&provider) else {
                return body.to_owned();
            };
            let labels = selector
                .runs_on
                .iter()
                .map(|label| crate::s2::yaml_scalar(label))
                .collect::<Vec<_>>()
                .join(", ");
            output.push_str("    runs-on: ");
            if selector.runs_on.len() == 1 {
                output.push_str(&labels);
            } else {
                output.push('[');
                output.push_str(&labels);
                output.push(']');
            }
            if segment.ends_with('\n') {
                output.push('\n');
            }
            replaced = true;
        } else {
            output.push_str(segment);
        }
    }
    // A generated workflow can be adopted again. In that case the runner
    // selector is already static, so preserve it instead of treating the
    // absence of a replacement as a malformed source template.
    if !replaced {
        return body.to_owned();
    }
    output
}

/// The owner blocks the rendered `package-update.yml` declares, in template
/// order. The repo-owned channel grants are validated against this set, so a
/// typo'd block name is a usage error instead of a narrowed publish lane; the
/// renderer below walks the same shape to attribute each channel matrix to its
/// block.
pub(crate) fn apt_package_update_owner_blocks(template: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut in_jobs = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        if indentation == 0 {
            in_jobs = trimmed == "jobs:";
            continue;
        }
        if in_jobs && indentation == 2 && trimmed.ends_with(':') {
            blocks.push(trimmed.strip_suffix(':').unwrap_or(trimmed));
        }
    }
    blocks
}

pub(crate) fn render_apt_package_update_template(config: &ProjectConfig, template: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut replaced = false;
    let mut block: Option<&str> = None;
    let mut in_jobs = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        // Owner blocks are the top-level jobs of this workflow; the channel
        // matrix inside each one is rewritten from the declared grants below.
        if indentation == 0 {
            in_jobs = trimmed == "jobs:";
        }
        if in_jobs && indentation == 2 && trimmed.ends_with(':') {
            block = Some(trimmed.strip_suffix(':').unwrap_or(trimmed));
        }
        let is_channel_matrix = indentation == 8 && trimmed.starts_with("channel:");
        if let Some(block) = block.filter(|_| is_channel_matrix) {
            output.push_str(&line[..indentation]);
            output.push_str(&apt_package_update_channel_matrix(
                &package_update_channel_grant(config, block),
            ));
            if segment.ends_with('\n') {
                output.push('\n');
            }
            replaced = true;
        } else if trimmed.starts_with("runs-on:")
            && line.contains("${{")
            && line.contains("inputs.providers")
        {
            let Some(hosted) = config.selectors.get(&ProviderId::GithubHosted) else {
                return template.to_owned();
            };
            let indent = &line[..indentation];
            output.push_str(indent);
            output.push_str("runs-on: ");
            output.push_str(&crate::s2::yaml_scalar(
                &hosted.runs_on.first().cloned().unwrap_or_default(),
            ));
            if segment.ends_with('\n') {
                output.push('\n');
            }
            replaced = true;
        } else {
            output.push_str(segment);
        }
    }
    // A generated workflow can be adopted again. In that case the barrier
    // selector is already static, so preserve it instead of panicking while
    // checking an otherwise valid generated file.
    if !replaced {
        return template.to_owned();
    }
    output
}

/// The update channels one owner block of the rendered `package-update.yml`
/// matrix may consult. The repo-owned config grants channels explicitly, and a
/// declared grant table is total before it reaches this renderer —
/// `config::validate` rejects a table that leaves an owner block without a row
/// or a `default`. A repository that grants no channels at all gets `stable`,
/// the one channel every consumer-side updater implements.
pub(crate) fn package_update_channel_grant(config: &ProjectConfig, block: &str) -> Vec<String> {
    if let Some(grants) = &config.package_update_channels {
        return grants
            .get(block)
            .or_else(|| grants.get("default"))
            .cloned()
            .unwrap_or_else(|| vec!["stable".to_owned()]);
    }
    vec!["stable".to_owned()]
}

fn apt_package_update_channel_matrix(channels: &[String]) -> String {
    let rendered = channels
        .iter()
        .map(|channel| crate::s2::yaml_scalar(channel))
        .collect::<Vec<_>>()
        .join(", ");
    format!("channel: [{rendered}]")
}

fn apt_local_trusted_gate(default_branch: &str) -> String {
    format!(
        "github.ref == 'refs/heads/{default_branch}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')"
    )
}
