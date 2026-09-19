//! Pure publication decisions shared by the generated preview publisher.
//!
//! GitHub is the transport, but it is not the state machine.  The publisher
//! first makes an immutable release complete, then applies these decisions to
//! the append-only channel index.  Keeping the decisions pure gives staged
//! fixtures a way to exercise partial uploads, retries, and races without
//! talking to GitHub.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// The rolling channel's index release.  The release itself is retained; its
/// uniquely named index assets form an append-only history.
pub(crate) const PREVIEW_CHANNEL_TAG: &str = "preview";

/// The schema consumed by package updaters when they select a channel head.
pub(crate) const PREVIEW_CHANNEL_SCHEMA: &str = "velnor.preview-channel/v1";

/// Return the immutable release tag for one preview build.
pub(crate) fn preview_release_tag(version: &str) -> String {
    format!("preview-v{version}")
}

/// GitHub release assets use the dotted Debian spelling of a preview version.
pub(crate) fn preview_asset_version(version: &str) -> String {
    version.replace('~', ".")
}

/// Return the unique channel-index asset name for one preview build.
pub(crate) fn preview_channel_asset_name(version: &str) -> String {
    format!("preview-channel-{}.json", preview_asset_version(version))
}

/// The minimum immutable identity the channel index must carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreviewChannelHead {
    pub(crate) version: String,
    pub(crate) release_tag: String,
    pub(crate) release_id: String,
    pub(crate) manifest_sha256: String,
}

impl PreviewChannelHead {
    pub(crate) fn new(
        version: impl Into<String>,
        release_id: impl Into<String>,
        manifest_sha256: impl Into<String>,
    ) -> Self {
        let version = version.into();
        Self {
            release_tag: preview_release_tag(&version),
            version,
            release_id: release_id.into(),
            manifest_sha256: manifest_sha256.into(),
        }
    }
}

/// The result of comparing a candidate with the retained channel head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChannelAdvance {
    /// The candidate is newer and may be appended to the channel history.
    Advance,
    /// The candidate is already the channel head; only byte confirmation is
    /// needed.
    Confirm,
    /// A newer head exists, so this slower run must not advance the channel.
    Superseded,
}

/// Compare a candidate against one retained preview head.
pub(crate) fn channel_advance(
    current: Option<&PreviewChannelHead>,
    candidate: &PreviewChannelHead,
) -> Result<ChannelAdvance, String> {
    let Some(current) = current else {
        return Ok(ChannelAdvance::Advance);
    };
    match crate::apt::cmp_preview_versions(&candidate.version, &current.version)
        .map_err(|error| error.to_string())?
    {
        Ordering::Greater => Ok(ChannelAdvance::Advance),
        Ordering::Equal => {
            if current.release_tag == candidate.release_tag
                && current.release_id == candidate.release_id
                && current.manifest_sha256 == candidate.manifest_sha256
            {
                Ok(ChannelAdvance::Confirm)
            } else {
                Err(format!(
                    "preview version {} already names different immutable release bytes",
                    candidate.version
                ))
            }
        }
        Ordering::Less => Ok(ChannelAdvance::Superseded),
    }
}

/// One immutable subject and its verified digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImmutableAsset {
    pub(crate) name: String,
    pub(crate) sha256: String,
}

/// The safe action for one immutable asset during a retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AssetAction {
    /// The remote bytes match; do not upload or rewrite them.
    Confirm,
    /// The remote asset is absent; upload it once without clobbering.
    Upload,
}

/// Plan a retry-safe upload against the remote asset digest map.
///
/// Existing mismatched bytes are a hard error.  Missing bytes are the only
/// values eligible for upload; this is what lets a failed/partial API call be
/// retried without rewriting an immutable release.
pub(crate) fn immutable_asset_plan(
    expected: &[ImmutableAsset],
    remote: &BTreeMap<String, String>,
) -> Result<Vec<AssetAction>, String> {
    expected
        .iter()
        .map(|asset| match remote.get(&asset.name) {
            Some(actual) if actual == &asset.sha256 => Ok(AssetAction::Confirm),
            Some(actual) => Err(format!(
                "immutable asset {} digest changed: expected {}, found {}",
                asset.name, asset.sha256, actual
            )),
            None => Ok(AssetAction::Upload),
        })
        .collect()
}

/// Advance only one channel in a channel map, retaining every other channel.
pub(crate) fn advance_channel(
    channels: &mut BTreeMap<String, PreviewChannelHead>,
    channel: &str,
    candidate: PreviewChannelHead,
) -> Result<ChannelAdvance, String> {
    let decision = channel_advance(channels.get(channel), &candidate)?;
    if decision == ChannelAdvance::Advance {
        channels.insert(channel.to_owned(), candidate);
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, sha256: &str) -> ImmutableAsset {
        ImmutableAsset {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
        }
    }

    fn head(version: &str, id: &str, digest: &str) -> PreviewChannelHead {
        PreviewChannelHead::new(version, id, digest)
    }

    #[test]
    fn partial_upload_plans_only_missing_immutable_subjects() {
        let expected = [asset("amd64.deb", "a"), asset("arm64.deb", "b")];
        let remote = BTreeMap::from([(String::from("amd64.deb"), String::from("a"))]);

        assert_eq!(
            immutable_asset_plan(&expected, &remote),
            Ok(vec![AssetAction::Confirm, AssetAction::Upload])
        );
    }

    #[test]
    fn api_error_retry_confirms_bytes_that_arrived_before_retry() {
        let expected = [asset("manifest.json", "m")];
        let empty = BTreeMap::new();
        assert_eq!(
            immutable_asset_plan(&expected, &empty),
            Ok(vec![AssetAction::Upload])
        );

        // The first upload returned an API error after the provider stored the
        // bytes.  The retry observes and confirms them; it never clobbers.
        let remote_after_error =
            BTreeMap::from([(String::from("manifest.json"), String::from("m"))]);
        assert_eq!(
            immutable_asset_plan(&expected, &remote_after_error),
            Ok(vec![AssetAction::Confirm])
        );
    }

    #[test]
    fn equal_version_rerun_confirms_identity_but_rejects_different_bytes() {
        let candidate = head("1.2.3~preview.7+abcdef0", "42", "deadbeef");
        assert_eq!(
            channel_advance(Some(&candidate), &candidate),
            Ok(ChannelAdvance::Confirm)
        );

        let changed = head("1.2.3~preview.7+abcdef0", "42", "different");
        assert!(channel_advance(Some(&candidate), &changed).is_err());
    }

    #[test]
    fn slower_old_run_cannot_advance_a_newer_channel_head() {
        let newer = head("1.2.3~preview.8+abcdef1", "8", "new");
        let older = head("1.2.3~preview.7+abcdef0", "7", "old");
        let mut channels = BTreeMap::from([
            (String::from("stable"), head("1.2.2", "stable", "stable")),
            (String::from(PREVIEW_CHANNEL_TAG), newer.clone()),
        ]);

        assert_eq!(
            advance_channel(&mut channels, PREVIEW_CHANNEL_TAG, older),
            Ok(ChannelAdvance::Superseded)
        );
        assert_eq!(channels.get(PREVIEW_CHANNEL_TAG), Some(&newer));
        assert_eq!(
            channels.get("stable").map(|value| &value.version),
            Some(&String::from("1.2.2"))
        );
    }

    #[test]
    fn channel_update_retains_the_other_channel() {
        let stable = head("1.2.2", "stable", "stable");
        let preview = head("1.2.3~preview.1+abcdef0", "1", "preview");
        let mut channels = BTreeMap::from([(String::from("stable"), stable.clone())]);

        assert_eq!(
            advance_channel(&mut channels, PREVIEW_CHANNEL_TAG, preview.clone()),
            Ok(ChannelAdvance::Advance)
        );
        assert_eq!(channels.get("stable"), Some(&stable));
        assert_eq!(channels.get(PREVIEW_CHANNEL_TAG), Some(&preview));
    }

    #[test]
    fn naming_is_orderable_and_immutable() {
        assert_eq!(
            preview_release_tag("1.2.3~preview.7+abcdef0"),
            "preview-v1.2.3~preview.7+abcdef0"
        );
        assert_eq!(
            preview_channel_asset_name("1.2.3~preview.7+abcdef0"),
            "preview-channel-1.2.3.preview.7+abcdef0.json"
        );
    }
}
