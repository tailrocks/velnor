//! Per-VM TAP + netns + nftables. Teardown targets only one isolation ID.

use super::isolation::{IsolationIdentity, IsolationResources};

/// Host argv to create TAP + netns + nftables for one isolation.
#[must_use]
pub fn setup_net_invocations(resources: &IsolationResources) -> Vec<(String, Vec<String>)> {
    let id = resources.identity.as_jailer_id();
    let tap = resources.tap.clone();
    let table = format!("velnor-{id}");
    vec![
        ("ip".into(), vec!["netns".into(), "add".into(), id.clone()]),
        (
            "ip".into(),
            vec![
                "tuntap".into(),
                "add".into(),
                "dev".into(),
                tap.clone(),
                "mode".into(),
                "tap".into(),
            ],
        ),
        (
            "ip".into(),
            vec![
                "link".into(),
                "set".into(),
                tap.clone(),
                "netns".into(),
                id.clone(),
            ],
        ),
        (
            "ip".into(),
            vec![
                "netns".into(),
                "exec".into(),
                id.clone(),
                "ip".into(),
                "link".into(),
                "set".into(),
                tap.clone(),
                "up".into(),
            ],
        ),
        (
            "nft".into(),
            vec!["add".into(), "table".into(), "inet".into(), table.clone()],
        ),
        (
            "nft".into(),
            vec![
                "add".into(),
                "chain".into(),
                "inet".into(),
                table.clone(),
                "forward".into(),
                "{ type filter hook forward priority 0; policy drop; }".into(),
            ],
        ),
        (
            "nft".into(),
            vec![
                "add".into(),
                "rule".into(),
                "inet".into(),
                table,
                "forward".into(),
                "iifname".into(),
                tap.clone(),
                "oifname".into(),
                "!=".into(),
                tap,
                "drop".into(),
            ],
        ),
    ]
}

/// Host argv that delete only this isolation's TAP, netns, and nftables.
#[must_use]
pub fn teardown_net_invocations(resources: &IsolationResources) -> Vec<(String, Vec<String>)> {
    let id = resources.identity.as_jailer_id();
    vec![
        (
            "ip".into(),
            vec!["link".into(), "delete".into(), resources.tap.clone()],
        ),
        (
            "ip".into(),
            vec!["netns".into(), "delete".into(), id.clone()],
        ),
        (
            "nft".into(),
            vec![
                "delete".into(),
                "table".into(),
                "inet".into(),
                format!("velnor-{id}"),
            ],
        ),
    ]
}

/// nftables commands for one VM. No VM-to-VM, no host-management net.
#[must_use]
pub fn nftables_commands(resources: &IsolationResources) -> Vec<String> {
    setup_net_invocations(resources)
        .into_iter()
        .filter(|(program, _)| program == "nft")
        .map(|(program, args)| format!("{program} {}", args.join(" ")))
        .collect()
}

/// Host commands that delete only this isolation's TAP and netns.
#[must_use]
pub fn teardown_net_commands(resources: &IsolationResources) -> Vec<String> {
    teardown_net_invocations(resources)
        .into_iter()
        .map(|(program, args)| format!("{program} {}", args.join(" ")))
        .collect()
}

/// True when every teardown command names this isolation and no sibling.
#[must_use]
pub fn teardown_is_exact(resources: &IsolationResources, sibling: &IsolationIdentity) -> bool {
    let own = resources.identity.as_jailer_id();
    let other = sibling.as_jailer_id();
    let sibling_tap =
        IsolationResources::for_identity(sibling.clone(), std::path::Path::new("/run")).tap;
    teardown_net_commands(resources).into_iter().all(|command| {
        (command.contains(&own) || command.contains(&resources.tap))
            && !command.contains(&other)
            && !command.contains(&sibling_tap)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn teardown_does_not_touch_sibling() {
        let a =
            IsolationResources::for_identity(IsolationIdentity::new("job-a", 1), Path::new("/run"));
        let b = IsolationIdentity::new("job-b", 2);
        assert!(teardown_is_exact(&a, &b));
        assert!(nftables_commands(&a).iter().all(|command| command
            .contains(&a.identity.as_jailer_id())
            || command.contains(&a.tap)));
        assert!(setup_net_invocations(&a).iter().any(
            |(program, args)| program == "ip" && args.windows(2).any(|w| w == ["netns", "add"])
        ));
        let b =
            IsolationResources::for_identity(IsolationIdentity::new("job-b", 2), Path::new("/run"));
        assert_ne!(a.tap, b.tap);
        assert!(setup_net_invocations(&a)
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == &a.tap)));
        assert!(setup_net_invocations(&b)
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == &b.tap)));
    }
}
