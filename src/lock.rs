//! Lock file management with stale PID detection.
//!
//! Creates `.epic-runner/{EPIC_CODE}.lock` containing the current PID.
//! On subsequent runs, reads the PID inside, checks if that process is still
//! alive via `kill(pid, 0)` (Unix) or `tasklist` (Windows), and auto-removes
//! stale locks from crashed processes.

use std::path::{Path, PathBuf};

/// Outcome of attempting to acquire an epic lock.
pub enum LockOutcome {
    /// Lock acquired successfully. Contains the lock file path.
    Acquired(PathBuf),
    /// Lock already held by a live process.
    AlreadyRunning { epic_code: String, pid: u32 },
    /// A stale lock was cleaned up and the new lock was acquired.
    StaleRecovered { lock_path: PathBuf, dead_pid: u32 },
}

/// Attempt to acquire the lock for an epic.
///
/// 1. If no lock file exists → create it with our PID, return `Acquired`.
/// 2. If lock file exists with a live PID → return `AlreadyRunning`.
/// 3. If lock file exists with a dead/invalid PID → remove stale lock,
///    create new one, return `StaleRecovered`.
pub fn acquire_epic_lock(
    lock_dir: &Path,
    epic_code: &str,
) -> Result<LockOutcome, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(lock_dir).ok();
    let lock_path = lock_dir.join(format!("{}.lock", epic_code));

    if lock_path.exists() {
        let pid_str = std::fs::read_to_string(&lock_path).unwrap_or_default();
        let pid = pid_str.trim().parse::<u32>().unwrap_or(0);

        if pid > 0 && is_process_alive(pid) {
            return Ok(LockOutcome::AlreadyRunning {
                epic_code: epic_code.to_string(),
                pid,
            });
        }

        // Stale lock — process is dead (or PID unparseable). Clean up.
        let dead_pid = pid;
        std::fs::remove_file(&lock_path).ok();

        // Write new lock
        std::fs::write(&lock_path, std::process::id().to_string())?;
        return Ok(LockOutcome::StaleRecovered {
            lock_path,
            dead_pid,
        });
    }

    // No lock exists — create it
    std::fs::write(&lock_path, std::process::id().to_string())?;
    Ok(LockOutcome::Acquired(lock_path))
}

/// Remove a lock file. Called on clean shutdown (or via scopeguard).
pub fn release_lock(lock_path: &Path) {
    std::fs::remove_file(lock_path).ok();
}

/// Check if a process with the given PID is still alive.
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a standard POSIX call that checks process existence
    // without sending any signal. Returns 0 if process exists, -1 with ESRCH if not.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Check if a process with the given PID is still alive (Windows).
#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            // tasklist prints "INFO: No tasks are running..." when PID not found
            !out.contains("No tasks") && out.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a temp dir that auto-cleans.
    fn temp_lock_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    // ── is_process_alive ──────────────────────────────────

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_process_alive(pid), "our own PID should be alive");
    }

    #[test]
    fn dead_pid_is_not_alive() {
        // Use a very high PID that is almost certainly not running.
        // PID 4,294,967 exceeds the max PID on most systems.
        assert!(
            !is_process_alive(4_294_967),
            "very high PID should not be alive"
        );
    }

    #[test]
    fn is_process_alive_returns_false_for_pid_99999() {
        // AC5: is_process_alive(99999) should return false.
        // PID 99999 *could* theoretically be alive on a loaded system, but this
        // matches the acceptance criterion directly. The dead_pid_is_not_alive
        // test above uses a safer PID as fallback.
        assert!(
            !is_process_alive(99999),
            "PID 99999 should not be alive on this machine"
        );
    }

    #[test]
    fn pid_zero_returns_false_on_guard() {
        // PID 0 on Unix means "all processes in the process group" — we guard
        // against this in acquire_epic_lock with `pid > 0`. The raw function
        // may return true (since pid 0 sends to the process group), but the
        // lock logic gates on pid > 0 first.
        // This test documents the expectation at the lock-acquisition level.
        let dir = temp_lock_dir();
        let lock_path = dir.path().join("TEST.lock");
        std::fs::write(&lock_path, "0").unwrap();

        let outcome = acquire_epic_lock(dir.path(), "TEST").unwrap();
        match outcome {
            LockOutcome::StaleRecovered { dead_pid, .. } => {
                assert_eq!(dead_pid, 0);
            }
            _ => panic!("expected StaleRecovered for PID 0"),
        }
    }

    // ── acquire_epic_lock ─────────────────────────────────

    #[test]
    fn acquires_fresh_lock() {
        let dir = temp_lock_dir();
        let outcome = acquire_epic_lock(dir.path(), "FRESH-001").unwrap();

        match outcome {
            LockOutcome::Acquired(path) => {
                assert!(path.exists(), "lock file should exist");
                let contents = std::fs::read_to_string(&path).unwrap();
                assert_eq!(
                    contents,
                    std::process::id().to_string(),
                    "lock should contain our PID"
                );
            }
            _ => panic!("expected Acquired for fresh lock"),
        }
    }

    #[test]
    fn detects_live_process() {
        let dir = temp_lock_dir();
        let our_pid = std::process::id();

        // Pre-create a lock file with our own PID (which is alive)
        let lock_path = dir.path().join("LIVE-001.lock");
        std::fs::write(&lock_path, our_pid.to_string()).unwrap();

        let outcome = acquire_epic_lock(dir.path(), "LIVE-001").unwrap();
        match outcome {
            LockOutcome::AlreadyRunning { pid, epic_code } => {
                assert_eq!(pid, our_pid);
                assert_eq!(epic_code, "LIVE-001");
            }
            _ => panic!("expected AlreadyRunning when PID is alive"),
        }
    }

    #[test]
    fn recovers_stale_lock_with_dead_pid() {
        let dir = temp_lock_dir();
        let dead_pid: u32 = 4_294_967; // almost certainly not running

        // Pre-create a lock file with a dead PID
        let lock_path = dir.path().join("STALE-001.lock");
        std::fs::write(&lock_path, dead_pid.to_string()).unwrap();

        let outcome = acquire_epic_lock(dir.path(), "STALE-001").unwrap();
        match outcome {
            LockOutcome::StaleRecovered {
                dead_pid: recovered_pid,
                lock_path: new_path,
            } => {
                assert_eq!(recovered_pid, dead_pid);
                // New lock should contain OUR pid
                let contents = std::fs::read_to_string(&new_path).unwrap();
                assert_eq!(contents, std::process::id().to_string());
            }
            _ => panic!("expected StaleRecovered for dead PID"),
        }
    }

    #[test]
    fn recovers_corrupt_lock_file() {
        let dir = temp_lock_dir();

        // Lock file with garbage content
        let lock_path = dir.path().join("CORRUPT-001.lock");
        std::fs::write(&lock_path, "not-a-pid\n").unwrap();

        let outcome = acquire_epic_lock(dir.path(), "CORRUPT-001").unwrap();
        match outcome {
            LockOutcome::StaleRecovered { dead_pid, .. } => {
                assert_eq!(dead_pid, 0, "unparseable PID should default to 0");
            }
            _ => panic!("expected StaleRecovered for corrupt lock"),
        }
    }

    #[test]
    fn recovers_empty_lock_file() {
        let dir = temp_lock_dir();

        // Empty lock file (crash during write?)
        let lock_path = dir.path().join("EMPTY-001.lock");
        std::fs::File::create(&lock_path).unwrap();

        let outcome = acquire_epic_lock(dir.path(), "EMPTY-001").unwrap();
        match outcome {
            LockOutcome::StaleRecovered { dead_pid, .. } => {
                assert_eq!(dead_pid, 0, "empty file should parse to PID 0");
            }
            _ => panic!("expected StaleRecovered for empty lock"),
        }
    }

    // ── release_lock ──────────────────────────────────────

    #[test]
    fn release_removes_lock_file() {
        let dir = temp_lock_dir();
        let lock_path = dir.path().join("RELEASE-001.lock");
        std::fs::write(&lock_path, "12345").unwrap();
        assert!(lock_path.exists());

        release_lock(&lock_path);
        assert!(!lock_path.exists(), "lock should be removed after release");
    }

    #[test]
    fn release_nonexistent_is_noop() {
        let dir = temp_lock_dir();
        let lock_path = dir.path().join("GHOST-001.lock");
        // Should not panic
        release_lock(&lock_path);
    }

    // ── lock_dir creation ─────────────────────────────────

    #[test]
    fn creates_lock_dir_if_missing() {
        let dir = temp_lock_dir();
        let nested = dir.path().join("deep").join("nested");
        assert!(!nested.exists());

        let outcome = acquire_epic_lock(&nested, "NESTED-001").unwrap();
        match outcome {
            LockOutcome::Acquired(path) => {
                assert!(path.exists());
                assert!(nested.exists(), "lock dir should have been created");
            }
            _ => panic!("expected Acquired"),
        }
    }
}
