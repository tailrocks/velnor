//! Per-VM TAP + netns + nftables. Teardown targets only one isolation ID.

use super::isolation::{IsolationIdentity, IsolationResources};

/// nftables commands for one VM. No VM-to-VM, no host-management net.
#[must_use]
pub fn nftables_commands(resources: &IsolationResources) -> Vec<String> {
    let id = resources.identity.as_jailer_id();
    vec![
        format!("nft add table inet velnor-{id}"),
        format!("nft add chain inet velnor-{id} forward '{{ type filter hook forward priority 0; policy drop; }}'"),
        format!(
            "nft add rule inet velnor-{id} forward iifname {} oifname != {} drop",
            resources.tap, resources.tap
        ),
    ]
}

/// Host commands that delete only this isolation's TAP and netns.
#[must_use]
pub fn teardown_net_commands(resources: &IsolationResources) -> Vec<String> {
    let id = resources.identity.as_jailer_id();
    vec![
        format!("ip link delete {}", resources.tap),
        format!("ip netns delete {id}"),
        format!("nft delete table inet velnor-{id}"),
    ]
}

/// True when every teardown command names this isolation and no sibling.
#[must_use]
pub fn teardown_is_exact(resources: &IsolationResources, sibling: &IsolationIdentity) -> bool {
    let own = resources.identity.as_jailer_id();
    let other = sibling.as_jailer_id();
    teardown_net_commands(resources)
        .into_iter()
        .all(|command| command.contains(&own) && !command.contains(&other))
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
        assert!(nftables_commands(&a)
            .iter()
            .all(|command| command.contains(&a.identity.as_jailer_id())));
    }
}
