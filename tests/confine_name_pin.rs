//! RED pins for TDEP-CONFINE-001 (S6): `safe_component` hardening, exercised through
//! the public `ArtifactName` boundary which calls `safe_component`.

use std::str::FromStr;

use phora::error::Error;
use phora::kernel::{ArtifactName, KernelError};
use phora::source::SourceError;

fn is_unsafe_component_rejection(err: &Error) -> bool {
    matches!(
        err,
        Error::SourceCtx(SourceError::Kernel(KernelError::UnsafeComponent(_)))
    )
}

#[test]
fn artifact_name_rejects_ntfs_ads_colon() {
    for name in ["foo:bar", "init.lua:$DATA", ":hidden", "a:b:c"] {
        let err = ArtifactName::from_str(name)
            .expect_err("an NTFS ADS `:` component must be rejected by safe_component (S6)");
        assert!(
            is_unsafe_component_rejection(&err),
            "{name:?} carries an NTFS ADS `:` and must be rejected as an UnsafeComponent (S6); \
             got a different error variant instead: {err:?}"
        );
    }
}

/// Reserved DOS device names resolve to a device, not a file, even with an extension.
#[test]
fn artifact_name_rejects_reserved_device_names() {
    for name in [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9", "con", "Com3", "nul.txt",
        "LPT5.log",
    ] {
        let err = ArtifactName::from_str(name)
            .expect_err("a reserved DOS device name must be rejected by safe_component (S6)");
        assert!(
            is_unsafe_component_rejection(&err),
            "{name:?} is a reserved DOS device name (incl. an extension variant) and must be \
             rejected as an UnsafeComponent (S6); got a different error variant instead: {err:?}"
        );
    }
}

/// Hardening must not regress the existing accept-set: ordinary names with digits or a
/// `com`/`lpt` substring that are NOT the reserved tokens still pass.
#[test]
fn artifact_name_still_accepts_ordinary_names_after_hardening() {
    for name in [
        "init.lua",
        "COM0",
        "COM10",
        "LPT0",
        "console",
        "comrade",
        "lptools.sh",
    ] {
        assert!(
            ArtifactName::from_str(name).is_ok(),
            "{name:?} is an ordinary single component (not a reserved device name) and must \
             keep passing after the S6 hardening"
        );
    }
}
