//! Parity oracle: `src/source.rs::safe_component` rejects exactly empty, `.`, `..`,
//! and any string containing `/` or `\`; everything else passes through unchanged.

use std::str::FromStr;

use phora::kernel::{ArtifactName, SourceName};

#[test]
fn artifact_name_accepts_normal_single_components() {
    for name in ["init.lua", "lua", "run.sh", "opts.lua", "a"] {
        let parsed =
            ArtifactName::from_str(name).expect("a normal single path component must be accepted");
        assert_eq!(
            parsed.as_str(),
            name,
            "{name} is a normal single path component and must pass through unchanged"
        );
    }
}

#[test]
fn artifact_name_rejects_empty() {
    assert!(
        ArtifactName::from_str("").is_err(),
        "an empty artifact name must be rejected (safe_component parity)"
    );
}

#[test]
fn artifact_name_rejects_dot_segments() {
    for name in [".", ".."] {
        assert!(
            ArtifactName::from_str(name).is_err(),
            "{name:?} is a traversal segment and must be rejected (safe_component parity)"
        );
    }
}

#[test]
fn artifact_name_rejects_path_separators() {
    for name in ["a/b", "..\\b", "lua\\opts", "/abs", "/etc/passwd", "a/../b"] {
        assert!(
            ArtifactName::from_str(name).is_err(),
            "{name:?} contains a path separator and escapes a single component; must be rejected"
        );
    }
}

#[test]
fn artifact_name_accepts_leading_dot_filename() {
    let parsed = ArtifactName::from_str(".keep")
        .expect("a leading-dot filename is NOT `.` or `..` and must be accepted (no new rules)");
    assert_eq!(parsed.as_str(), ".keep");
}

#[test]
fn artifact_name_display_round_trips_the_input() {
    let parsed = ArtifactName::from_str("init.lua").expect("valid artifact name");
    assert_eq!(
        parsed.to_string(),
        "init.lua",
        "Display must reproduce the exact accepted name"
    );
}

#[test]
fn source_name_accepts_normal_names() {
    for name in ["nvim-config", "dotfiles", "a", "my.source"] {
        let parsed = SourceName::from_str(name).expect("a normal source name must be accepted");
        assert_eq!(parsed.as_str(), name, "{name} must pass through unchanged");
    }
}

#[test]
fn source_name_rejects_empty() {
    assert!(
        SourceName::from_str("").is_err(),
        "an empty source name must be rejected"
    );
}

#[test]
fn source_name_rejects_dot_segments() {
    for name in [".", ".."] {
        assert!(
            SourceName::from_str(name).is_err(),
            "{name:?} must be rejected (safe_component parity)"
        );
    }
}

#[test]
fn source_name_rejects_path_separators() {
    for name in ["a/b", "lua\\opts", "/abs", "a/../b"] {
        assert!(
            SourceName::from_str(name).is_err(),
            "{name:?} contains a path separator and must be rejected"
        );
    }
}

#[test]
fn source_name_display_round_trips_the_input() {
    let parsed = SourceName::from_str("nvim-config").expect("valid source name");
    assert_eq!(
        parsed.to_string(),
        "nvim-config",
        "Display must reproduce the exact accepted name"
    );
}
