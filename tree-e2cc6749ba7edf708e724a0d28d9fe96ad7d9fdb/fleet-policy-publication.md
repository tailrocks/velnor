# Fleet-policy publication

`velnor-tools fleet-policy generate` is an offline publisher for the reviewed
release-ref ledger. It writes the generated `<org>-desired-policy.json` files
only on Unix and only through a dedicated root publisher boundary.

## Security contract

The publisher requires all of the following:

- effective uid 0;
- an absolute output directory;
- every output-directory ancestor, including the output directory, is a real
  directory owned by uid 0 and has no group or other write bit;
- every policy entry is a single-link regular file owned by uid 0 and has no
  group or other write bit;
- no symlink is accepted in the directory walk or at a policy pathname;
- no hardlink, FIFO, socket, device, or other non-regular policy entry is
  accepted.

The publisher opens each directory relative to a descriptor with `O_NOFOLLOW`,
stages bytes in an exclusive temporary file, calls `sync_all`, reads the
staged bytes back, validates canonical JSON and its SHA-256 digest, and then
uses descriptor-relative publication. Failed temporary cleanup removes a
pathname only when it still names the publisher's captured temporary inode;
an attacker replacement is left untouched.

The directory `flock` and identity/content revalidation are cooperative
serialization and change detection. They are not compare-and-swap. Unix does
not provide a portable fd-bound atomic compare-and-rename or
compare-and-unlink for an arbitrary existing pathname. The old structure in
[#528](https://github.com/tailrocks/velnor/pull/528) therefore admitted a
whole bug class: a same-uid non-cooperating writer could replace a target or
temporary pathname after validation but before `renameat`/`unlinkat`, and the
mutation would resolve the attacker's inode.

The structural remedy is the ownership boundary. The protection claim is
limited to unprivileged/non-root writers subject to the host's Unix DAC rules;
the dedicated root publisher, any root or equivalent-capability process, ACL
exceptions, and the filesystem are trusted. A same-uid writer is protected
against only when it is unprivileged and cannot bypass that boundary. Root
process compromise is outside this contract. Revalidation remains useful for
detecting unexpected trusted-operator or filesystem churn and causes a
fail-closed refusal; it is not relied on as the security proof.

## Platform and workflow contract

Non-Unix builds refuse generation because the required no-follow descriptor
primitives are not claimed there. Unix filesystems must preserve the ownership
and mode semantics above. Repository CI uses `audit-ci` to compare committed
policy bytes with deterministic generation and deliberately does not invoke
the privileged mutator.

## External publisher acceptance

CI cannot run the positive root-publisher path acceptance. The provisioned
publisher host must run the enforceable acceptance task against an existing
output directory:

```sh
export VELNOR_FLEET_POLICY_OUT_DIR=/absolute/path/to/publisher/fleet/policies
rtk mise run fleet-publisher-acceptance
```

The task fails closed when the variable is absent, relative, `/`, missing, or
the effective uid is not 0. It invokes the production binary and records the
success of the effective-uid, descriptor-relative no-follow namespace,
root-owner, mode, regular-file, and single-link checks. The command publishes
the reviewed ledger output into the supplied directory; use the provisioned
publisher directory, then copy reviewed output through the approved workflow.
The ordinary `rtk mise run fleet-generate` task retains the same required
environment and production checks for routine publication.
