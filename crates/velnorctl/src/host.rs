//! On-demand host entry point: start repository-scoped Docker-backed capacity.
//!
//! `velnorctl host start` wraps `configure`+`daemon` with defaults. It never
//! joins an organization runner group. GitHub remains the job scheduler.

use std::env;
use std::path::PathBuf;

use velnor_model::ExitClass;

use crate::commands::{HostCommand, HostStartArgs};
use crate::runtime::{self, DaemonArgs};
use crate::{CommandError, GlobalArgs};

const DEFAULT_REPO: &str = "tailrocks/velnor";

pub async fn run(globals: &GlobalArgs, command: HostCommand) -> Result<(), CommandError> {
    match command {
        HostCommand::Start(args) => start(globals, args).await,
        HostCommand::Status => status(globals).await,
        HostCommand::Drain => drain(globals).await,
        HostCommand::Stop => stop(),
    }
}

async fn start(globals: &GlobalArgs, args: HostStartArgs) -> Result<(), CommandError> {
    let url = resolve_repo_url(&args)?;
    if github_pat().is_none() {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.token_missing",
            "GITHUB_TOKEN is unset. Export a short-lived registration token \
             with repo administration rights; do not pass the token as a flag.",
        ));
    }

    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("velnor-local-{}", hostname_slug()));
    let slots = args.slots.max(1);
    let socket = velnor_client::socket_root();
    let docker = docker_endpoint_display();
    let execution = execution_platform();

    println!("host start");
    println!("  github_scope     {url} (repository)");
    println!("  runner_name      {name}");
    println!("  slots            {slots}");
    println!("  docker_endpoint  {docker}");
    println!("  execution        {execution}");
    println!(
        "  host_platform    {}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("  control_socket   unix://{}/{name}", socket.display());
    println!("  pool             none (repository-scoped; will not join velnor-trusted)");
    if let Some(pr) = args.pr {
        print_pr_scheduling(pr);
    }
    println!("  drain/stop       Ctrl-C this process, or `velnorctl host drain` then Ctrl-C");

    let mut daemon = DaemonArgs {
        state_db: None,
        config_dir: args.config_dir,
        url: Some(url),
        pat: github_pat(),
        name: Some(name),
        labels: vec!["self-hosted".into(), "velnor-target-mvp".into()],
        target_mvp_labels: true,
        target_mvp_arm_label: cfg!(target_arch = "aarch64"),
        replace: false,
        pool_id: None,
        pool_name: None,
        routing_policy_file: None,
        dry_run_registration: false,
        slots,
        max_idle_slot_age_seconds: None,
        once: false,
        idle_timeout_seconds: None,
        complete_noop: false,
        execute_scripts: false,
        dry_run_jobs: false,
        dump_job_message: None,
        docker_image: "velnor/job-ubuntu:26.04".into(),
        job_cpus: String::new(),
        job_memory: String::new(),
        trust: velnor_runner::trust_scope::TrustScopeArg {
            trust_scope: env::var("VELNOR_TRUST_SCOPE").unwrap_or_else(|_| "untrusted".into()),
        },
        emergency_reserve_bytes: 10_737_418_240,
        job_peak_bytes: 32_212_254_720,
        node_action_image: String::new(),
        work_dir: args.work_dir,
        docker_host_work_dir: args.docker_host_work_dir,
        skip_preflight: false,
        require_docker_socket: true,
    };
    if globals.instance.is_some() {
        daemon.name = globals.instance.clone();
    }

    runtime::run_daemon(daemon).await.map_err(|error| {
        CommandError::new(
            ExitClass::Operation,
            "host.start_failed",
            format!("unable to start on-demand host: {error}"),
        )
    })
}

async fn status(_globals: &GlobalArgs) -> Result<(), CommandError> {
    println!(
        "control_socket_root {}",
        velnor_client::socket_root().display()
    );
    println!(
        "package_socket_mode {}",
        velnor_client::is_package_socket_mode()
    );
    println!("docker_endpoint     {}", docker_endpoint_display());
    println!("execution           {}", execution_platform());
    println!("follow              velnorctl status");
    println!("docker_report       velnorctl docker report");
    Ok(())
}

async fn drain(_globals: &GlobalArgs) -> Result<(), CommandError> {
    Err(CommandError::new(
        ExitClass::Usage,
        "host.drain_foreground",
        "Foreground `velnorctl host start` drains on Ctrl-C. Wait for in-flight \
         jobs to finish, then press Ctrl-C. `velnorctl drain` needs a live admin socket.",
    ))
}

fn stop() -> Result<(), CommandError> {
    Err(CommandError::new(
        ExitClass::Usage,
        "host.stop_foreground",
        "`velnorctl host start` runs in the foreground. Press Ctrl-C to disconnect. \
         Drain first with `velnorctl host drain` if jobs are running.",
    ))
}

fn resolve_repo_url(args: &HostStartArgs) -> Result<String, CommandError> {
    let raw = match (args.url.as_deref(), args.repo.as_deref()) {
        (Some(url), _) => url.trim().to_owned(),
        (None, Some(repo)) => format!("https://github.com/{}", repo.trim().trim_matches('/')),
        (None, None) => format!("https://github.com/{DEFAULT_REPO}"),
    };
    let Some(path) = github_repo_path(&raw) else {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.url_invalid",
            format!("host start needs a repository URL (owner/name), got {raw}"),
        ));
    };
    if path.matches('/').count() != 1 {
        return Err(CommandError::new(
            ExitClass::Usage,
            "host.org_scope_refused",
            format!(
                "{raw} is an organization or enterprise URL. Repository-scoped \
                 recovery will not join velnor-trusted or any other org pool. \
                 Pass --repo owner/name."
            ),
        ));
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

fn github_repo_path(url: &str) -> Option<&str> {
    url.strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .map(|path| path.trim_matches('/'))
}

fn github_pat() -> Option<String> {
    env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
}

fn hostname_slug() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| {
            value
                .chars()
                .flat_map(|ch| ch.to_lowercase())
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .take(24)
                .collect::<String>()
                .trim_matches('-')
                .to_owned()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "host".into())
}

fn docker_endpoint_display() -> String {
    for key in ["VELNOR_DOCKER_HOST", "DOCKER_HOST"] {
        if let Ok(value) = env::var(key) {
            if !value.is_empty() {
                return format!("{value} (from {key})");
            }
        }
    }
    if PathBuf::from("/var/run/docker.sock").exists() {
        return "unix:///var/run/docker.sock".into();
    }
    if let Some(home) = env::var_os("HOME") {
        let orb = PathBuf::from(home).join(".orbstack/run/docker.sock");
        if orb.exists() {
            return format!("unix://{} (orbstack)", orb.display());
        }
    }
    "unavailable (start Docker/OrbStack; host start will not switch backends)".into()
}

fn execution_platform() -> String {
    if std::env::consts::OS == "macos" {
        "linux-container (Docker VM on macOS; not a native macOS job)".into()
    } else {
        format!(
            "linux-container on {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests may panic")]
mod tests {
    use super::*;

    #[test]
    fn default_and_repo_urls_are_repository_scoped() {
        let default = resolve_repo_url(&HostStartArgs {
            repo: None,
            url: None,
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
        })
        .expect("default repo");
        assert_eq!(default, "https://github.com/tailrocks/velnor");

        let named = resolve_repo_url(&HostStartArgs {
            repo: Some("tailrocks/velnor".into()),
            url: None,
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
        })
        .expect("named repo");
        assert_eq!(named, "https://github.com/tailrocks/velnor");
    }

    #[test]
    fn org_urls_are_refused() {
        let error = resolve_repo_url(&HostStartArgs {
            repo: None,
            url: Some("https://github.com/tailrocks".into()),
            name: None,
            slots: 1,
            pr: None,
            work_dir: None,
            config_dir: None,
            docker_host_work_dir: None,
        })
        .expect_err("org URL");
        assert_eq!(error.reason, "host.org_scope_refused");
    }
}

fn print_pr_scheduling(pr: u64) {
    println!(
        "  pr_target        #{pr}: GitHub remains the scheduler. This session registers \
         repository-scoped runners only."
    );
    println!(
        "  pr_target        Jobs already queued with `group: velnor-trusted` cannot be \
         claimed by this host. Changing labels or YAML does not rewrite those jobs."
    );
    println!(
        "  pr_target        A workflow_dispatch against the PR branch is a new run and \
         does not satisfy the original PR checks unless GitHub associates it."
    );
}
