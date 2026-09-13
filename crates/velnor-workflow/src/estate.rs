//! Generic package-update rendering for adopted workflow templates.
//!
//! Everything here is config-driven: the group, the labels, the hosted image,
//! and the channel grants all come from the scanned repository's own
//! generation config. The crate carries no repository catalog.

use crate::ProjectConfig;

/// The runner selector the generator's earliest generated surfaces embedded
/// before the group and the labels moved into each repository's generation
/// config. Static-template adoption replaces it with the declaring
/// repository's own selector.
pub(crate) const LEGACY_VELNOR_RUNNER_SELECTOR: &str =
    "fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]')";

pub(crate) fn render_apt_package_updater_template(
    template: &str,
    default_branch: &str,
    github_runner: &str,
    velnor_labels: &[String],
    velnor_group: Option<&str>,
) -> String {
    let Some((prefix, jobs)) = template.split_once("\n  verify:\n") else {
        return normalize_apt_package_updater_static_template(template, github_runner);
    };
    let Some((verify_body, mutate_body)) = jobs.split_once("\n  mutate:\n") else {
        return template.to_owned();
    };
    let trusted_gate = apt_velnor_trusted_gate(default_branch);
    let runners = AptLaneRunners {
        github_runner,
        velnor_labels,
        velnor_group,
    };
    let mut lanes = String::new();
    for (id, body, job, lane) in [
        (
            "verify-velnor",
            verify_body,
            AptPackageUpdaterJob::Verify,
            AptPackageUpdaterLane::Velnor,
        ),
        (
            "verify-github",
            verify_body,
            AptPackageUpdaterJob::Verify,
            AptPackageUpdaterLane::Github,
        ),
        (
            "mutate-velnor",
            mutate_body,
            AptPackageUpdaterJob::Mutate,
            AptPackageUpdaterLane::Velnor,
        ),
        (
            "mutate-github",
            mutate_body,
            AptPackageUpdaterJob::Mutate,
            AptPackageUpdaterLane::Github,
        ),
    ] {
        lanes.push_str(&render_apt_package_updater_job(
            id,
            body,
            job,
            lane,
            &trusted_gate,
            &runners,
        ));
        lanes.push('\n');
    }
    format!("{prefix}\n{}", lanes.trim_end_matches('\n'))
}

fn normalize_apt_package_updater_static_template(template: &str, github_runner: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut github_job = false;
    for segment in template.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        if indentation == 2 && trimmed.ends_with(':') {
            github_job = matches!(trimmed, "verify-github:" | "mutate-github:");
        }
        if github_job
            && indentation == 4
            && trimmed.starts_with("runs-on:")
            && trimmed != "runs-on:"
        {
            output.push_str(&line[..indentation]);
            output.push_str("runs-on: ");
            output.push_str(&crate::yaml_scalar(github_runner));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AptPackageUpdaterLane {
    Github,
    Velnor,
}

/// The runner placement every lane of a rendered `package-update.yml` selects
/// from: the hosted image the GitHub lane pins, and the self-hosted labels and
/// group the Velnor lane selects by.
struct AptLaneRunners<'a> {
    github_runner: &'a str,
    velnor_labels: &'a [String],
    velnor_group: Option<&'a str>,
}

fn render_apt_package_updater_job(
    id: &str,
    body: &str,
    job: AptPackageUpdaterJob,
    lane: AptPackageUpdaterLane,
    trusted_gate: &str,
    runners: &AptLaneRunners<'_>,
) -> String {
    let lane_name = match lane {
        AptPackageUpdaterLane::Github => "github",
        AptPackageUpdaterLane::Velnor => "velnor",
    };
    let trusted_condition = if lane == AptPackageUpdaterLane::Velnor {
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
                    "    if: ${{{{ inputs.consumer-repository != '' && inputs.lane == '{lane_name}'{trusted_condition} }}}}"
                ),
            );
        }
        AptPackageUpdaterJob::Mutate => {
            body = body.replace(
                "    needs: verify",
                &format!("    needs: verify-{lane_name}"),
            );
            body = body.replace(
                "    if: ${{ needs.verify.outputs.available == 'true' && inputs.writer }}",
                &format!(
                    "    if: ${{{{ needs.verify-{lane_name}.outputs.available == 'true' && inputs.writer{trusted_condition} }}}}"
                ),
            );
            // The mutate body can consume more than the availability output.
            // Rewrite every remaining dependency reference after renaming the
            // verify job so split lanes cannot retain a dangling needs.verify.
            body = body.replace("needs.verify.", &format!("needs.verify-{lane_name}."));
        }
    }
    format!(
        "  {id}:\n{}",
        replace_apt_package_updater_runner(&body, lane, runners)
    )
}

fn replace_apt_package_updater_runner(
    body: &str,
    lane: AptPackageUpdaterLane,
    runners: &AptLaneRunners<'_>,
) -> String {
    let mut output = String::with_capacity(body.len() + 96);
    let mut replaced = false;
    for segment in body.split_inclusive('\n') {
        let runner_line = segment.strip_suffix('\n').unwrap_or(segment);
        if runner_line.trim_start().starts_with("runs-on:")
            && runner_line.contains("${{")
            && runner_line.contains("inputs.lane")
        {
            match lane {
                AptPackageUpdaterLane::Github => {
                    output.push_str("    runs-on: ");
                    output.push_str(&crate::yaml_scalar(runners.github_runner));
                }
                AptPackageUpdaterLane::Velnor => {
                    output.push_str("    runs-on:");
                    output.push_str(&velnor_lane_selector(
                        runners.velnor_labels,
                        runners.velnor_group,
                    ));
                }
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

/// The static `runs-on:` value the updater's Velnor lane renders: the group
/// and the labels the repository declares for its self-hosted lane.
fn velnor_lane_selector(labels: &[String], group: Option<&str>) -> String {
    let labels = labels
        .iter()
        .map(|label| crate::yaml_scalar(label))
        .collect::<Vec<_>>()
        .join(", ");
    match group {
        Some(group) => format!(
            "\n      group: {}\n      labels: [{labels}]",
            crate::yaml_scalar(group)
        ),
        None => format!("[{labels}]"),
    }
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
            && line.contains("inputs.lanes")
            && line.contains("velnor")
        {
            let indent = &line[..indentation];
            output.push_str(indent);
            output.push_str("runs-on: ");
            output.push_str(&crate::yaml_scalar(&config.github_runner));
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
        .map(|channel| crate::yaml_scalar(channel))
        .collect::<Vec<_>>()
        .join(", ");
    format!("channel: [{rendered}]")
}

fn apt_velnor_trusted_gate(default_branch: &str) -> String {
    format!(
        "github.ref == 'refs/heads/{default_branch}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')"
    )
}
