//! Embedded hook scripts for ceremony enforcement.
//!
//! Hook scripts (stop-gate.sh, track-files.sh) are embedded in the binary via
//! `include_str!` and written to a temp directory at runtime so Claude Code's
//! `--settings` flag can reference them by absolute path.
//!
//! This mirrors the approach used for agent definitions in `agents.rs`.

use std::path::PathBuf;

const STOP_GATE_SH: &str = include_str!("../hooks/stop-gate.sh");
const TRACK_FILES_SH: &str = include_str!("../hooks/track-files.sh");

/// Write embedded hook scripts to temp dir and return the `--settings` JSON
/// that configures Claude Code to use them.
///
/// Returns `None` only if temp dir creation or file writes fail.
pub fn build_hooks_settings_json() -> Option<String> {
    let hook_dir = std::env::temp_dir().join("epic-runner-hooks");
    std::fs::create_dir_all(&hook_dir).ok()?;

    let stop_gate_path = hook_dir.join("stop-gate.sh");
    std::fs::write(&stop_gate_path, STOP_GATE_SH).ok()?;
    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&stop_gate_path, std::fs::Permissions::from_mode(0o755));
    }

    let track_files_path = hook_dir.join("track-files.sh");
    std::fs::write(&track_files_path, TRACK_FILES_SH).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&track_files_path, std::fs::Permissions::from_mode(0o755));
    }

    let stop_gate_abs = stop_gate_path.to_string_lossy();
    let track_abs = track_files_path.to_string_lossy();

    let hooks_obj = serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": stop_gate_abs,
                    "timeout": 10
                }]
            }],
            "PostToolUse": [{
                "matcher": "Edit|Write",
                "hooks": [{
                    "type": "command",
                    "command": track_abs,
                    "timeout": 5
                }]
            }]
        }
    });

    tracing::debug!(
        stop_gate = %stop_gate_path.display(),
        track_files = %track_files_path.display(),
        "Wrote embedded hooks to temp dir"
    );

    Some(hooks_obj.to_string())
}

/// Get the path to the embedded stop-gate.sh (for testing/debugging).
pub fn stop_gate_path() -> Option<PathBuf> {
    let p = std::env::temp_dir()
        .join("epic-runner-hooks")
        .join("stop-gate.sh");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_embedded_and_written() {
        let json = build_hooks_settings_json().expect("Should build hooks JSON");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("Should be valid JSON");

        // Verify structure
        assert!(parsed["hooks"]["Stop"].is_array());
        assert!(parsed["hooks"]["PostToolUse"].is_array());

        // Verify stop-gate command path exists
        let stop_cmd = parsed["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            std::path::Path::new(stop_cmd).exists(),
            "stop-gate.sh should exist at {stop_cmd}"
        );

        // Verify track-files command path exists
        let track_cmd = parsed["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            std::path::Path::new(track_cmd).exists(),
            "track-files.sh should exist at {track_cmd}"
        );

        // Verify content
        let stop_content = std::fs::read_to_string(stop_cmd).unwrap();
        assert!(stop_content.contains("EPIC_RUNNER_STORY_FILE"));

        let track_content = std::fs::read_to_string(track_cmd).unwrap();
        assert!(track_content.contains("EPIC_RUNNER_CHANGED_FILES"));
    }

    #[test]
    fn stop_gate_path_after_build() {
        build_hooks_settings_json().unwrap();
        assert!(stop_gate_path().is_some());
    }
}
