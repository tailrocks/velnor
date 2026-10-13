//! Packaged-invoker coverage: every `velnorctl <verb>` invocation shipped in
//! the package (systemd units, Debian maintainer scripts) and in the release
//! workflow must resolve against this binary's parse surface. Ported from the
//! service-surface guard of the former `velnor-runner` CLI: after the command
//! center cutover, `/usr/bin/velnorctl` is the only product binary, so a verb
//! the packaged files still invoke must exist HERE or installs and releases
//! break. This test fails at test time instead.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("repo root")
        .to_path_buf()
}

fn packaged_invoker_verbs() -> Vec<String> {
    let root = repo_root();
    // The deb payload: systemd units and maintainer scripts are the shipped
    // invokers of /usr/bin/velnorctl. Workflow usage is exercised by CI itself.
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates/velnor-runner/debian")).expect("debian dir") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    files
        .iter()
        .map(|path| {
            (
                path.clone(),
                std::fs::read_to_string(path).unwrap_or_default(),
            )
        })
        .flat_map(|(path, content)| {
            content
                .lines()
                .filter_map(move |line| {
                    let idx = line.find("/usr/bin/velnorctl")?;
                    let rest = line[idx + "/usr/bin/velnorctl".len()..].trim();
                    let verb = rest.split_whitespace().next()?;
                    Some(format!("{}: {verb}", path.display()))
                })
                .collect::<Vec<_>>()
                .into_iter()
        })
        .collect()
}

#[test]
fn velnorctl_parses_every_packaged_invoker_verb() {
    use clap::CommandFactory;

    let invokers = packaged_invoker_verbs();
    assert!(
        !invokers.is_empty(),
        "expected at least one /usr/bin/velnorctl invocation in packaged files"
    );
    let cli = velnorctl::Cli::command();
    for invoker in &invokers {
        let verb = invoker.rsplit(": ").next().expect("verb");
        assert!(
            cli.find_subcommand(verb).is_some(),
            "packaged invocation {invoker} has no matching velnorctl subcommand"
        );
    }
}
