use anyhow::Result;

use crate::{
    cli::{ConfigureArgs, RemoveArgs, RunArgs, StatusArgs},
    config::{self, RunnerSettings, StoredRunnerConfig},
};

pub async fn configure(args: ConfigureArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let agent_name = args.name.unwrap_or_else(default_agent_name);
    let labels = normalize_labels(args.labels);

    let stored = StoredRunnerConfig {
        settings: RunnerSettings {
            github_url: args.url,
            server_url: None,
            server_url_v2: None,
            pool_id: None,
            pool_name: None,
            agent_id: None,
            agent_name,
            labels,
            use_v2_flow: false,
            ephemeral: false,
            disable_update: true,
        },
        credentials: None,
    };

    config::save(&dir, &stored)?;
    println!("Wrote local runner config to {}", dir.display());
    println!("GitHub registration API calls are the next Milestone 0 step.");

    if args.replace {
        println!("Recorded --replace intent; remote replacement is not implemented yet.");
    }

    Ok(())
}

pub async fn run(args: RunArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let stored = config::load(&dir)?;
    println!(
        "Runner '{}' ready with labels: {}",
        stored.settings.agent_name,
        stored.settings.labels.join(",")
    );
    println!("Protocol polling is not implemented yet.");

    if args.once {
        println!("Run-once mode requested.");
    }

    Ok(())
}

pub async fn remove(args: RemoveArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    if config::remove(&dir)? {
        println!("Removed local runner config from {}", dir.display());
    } else {
        println!("No local runner config at {}", dir.display());
    }
    println!("Remote unregister is not implemented yet.");
    Ok(())
}

pub async fn status(args: StatusArgs) -> Result<()> {
    let dir = config::config_dir(args.config_dir)?;
    let stored = config::load(&dir)?;
    println!("Config dir: {}", dir.display());
    println!("GitHub URL: {}", stored.settings.github_url);
    println!("Runner name: {}", stored.settings.agent_name);
    println!("Labels: {}", stored.settings.labels.join(","));
    println!("Use V2 flow: {}", stored.settings.use_v2_flow);
    println!(
        "Credentials stored: {}",
        if stored.credentials.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    Ok(())
}

fn normalize_labels(mut labels: Vec<String>) -> Vec<String> {
    if labels.is_empty() {
        labels.push("velnor".to_string());
    }
    labels.sort();
    labels.dedup();
    labels
}

fn default_agent_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "velnor-runner".to_string())
}
