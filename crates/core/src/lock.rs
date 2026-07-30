//! Running-instance detection: flock(2) on `<base>/locks/<name>.lock`.
//!
//! The lock file lives OUTSIDE the profile dir and is NEVER deleted: a
//! stable inode is what makes mid-delete races detectable (an in-dir file
//! would be recreated as a fresh, non-conflicting inode), and deleting a
//! lock file is the classic unlock race. Leftover 0-byte files are harmless.
//! Case-insensitive filesystems: `Work.lock`/`work.lock` are one inode, so
//! the lock still conflicts even though name comparisons are case-sensitive.
//! Only NON-blocking variants exist here on purpose: a blocking flock would
//! hang the caller (or a regressed test suite) instead of failing fast.

use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

use crate::AppState;

/// A held flock on a profile's lock file. Released on drop, or on process
/// exit however it happens (the kernel unlocks when the fd closes).
#[derive(Debug)]
#[must_use = "hold in a named binding for the whole operation"]
pub struct ProfileLock {
    _lock: Flock<File>,
}

fn lock_path(base: &Path, name: &str) -> PathBuf {
    base.join("locks").join(format!("{name}.lock"))
}

/// Open (creating as needed) the lock file for `name`. Validates the name
/// FIRST: it becomes a path segment here too. The locks dir is 0o700 and the
/// file 0o600 (flock works on any fd another user can open; tight modes keep
/// a shared machine's users from wedging the lock). std opens files with
/// O_CLOEXEC by default on unix, so embedded/detached children never inherit
/// the fd (an inherited fd shares the open file description and would hold
/// the lock after this process exits).
fn open_lock_file(base: &Path, name: &str) -> anyhow::Result<File> {
    AppState::validate_profile_name(name).map_err(|e| anyhow::anyhow!(e))?;
    let dir = base.join("locks");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    let path = lock_path(base, name);
    OpenOptions::new()
        .create(true)
        .write(true)
        // Explicit: the file's content is irrelevant (the inode is the lock),
        // and truncate(false) satisfies clippy::suspicious_open_options.
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))
}

/// Take `name`'s lock SHARED (an instance announcing itself). Fails fast
/// while a delete/rename holds the lock exclusively.
pub(crate) fn acquire_shared_at(base: &Path, name: &str) -> anyhow::Result<ProfileLock> {
    let file = open_lock_file(base, name)?;
    match Flock::lock(file, FlockArg::LockSharedNonblock) {
        Ok(lock) => Ok(ProfileLock { _lock: lock }),
        Err((_, e)) if e == Errno::EWOULDBLOCK => {
            bail!("profile '{name}' is locked (a profile delete or rename is in progress)")
        }
        Err((_, e)) => {
            Err(anyhow::anyhow!(e)).with_context(|| format!("failed to lock profile '{name}'"))
        }
    }
}

/// Take `name`'s lock EXCLUSIVE (a delete/rename claiming sole ownership).
/// Fails fast while any instance holds it shared (or another exclusive op
/// holds it).
pub(crate) fn acquire_exclusive_at(base: &Path, name: &str) -> anyhow::Result<ProfileLock> {
    let file = open_lock_file(base, name)?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(ProfileLock { _lock: lock }),
        Err((_, e)) if e == Errno::EWOULDBLOCK => {
            bail!(
                "profile '{name}' is in use by a running instance or another profile operation"
            )
        }
        Err((_, e)) => {
            Err(anyhow::anyhow!(e)).with_context(|| format!("failed to lock profile '{name}'"))
        }
    }
}

/// Startup lock for a running instance: shared, to be held for the process's
/// lifetime. `Ok(None)` when `KOMMAND0_STATE_DIR` is set (non-empty): the
/// exact-dir escape hatch has no profiles tree, and skipping keeps unit/e2e
/// harnesses hermetic.
pub fn acquire_shared(name: &str) -> anyhow::Result<Option<ProfileLock>> {
    if AppState::state_dir_override().is_some() {
        return Ok(None);
    }
    Ok(Some(acquire_shared_at(&AppState::base_dir(), name)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn shared_locks_coexist_and_exclusive_conflicts() {
        let tmp = TempDir::new().unwrap();
        let s1 = acquire_shared_at(tmp.path(), "work").unwrap();
        let s2 = acquire_shared_at(tmp.path(), "work").unwrap();
        // The load-bearing half: flock is per OPEN FILE DESCRIPTION, so a
        // separate open in the SAME process still conflicts, the platform
        // assumption the whole design rests on (pinned on macOS and Linux CI).
        let err = acquire_exclusive_at(tmp.path(), "work").unwrap_err();
        assert!(err.to_string().contains("is in use by"), "got: {err}");
        drop(s1);
        let err = acquire_exclusive_at(tmp.path(), "work").unwrap_err();
        assert!(err.to_string().contains("is in use by"), "one shared holder suffices: {err}");
        drop(s2);
        let _x = acquire_exclusive_at(tmp.path(), "work")
            .expect("exclusive succeeds once every shared guard dropped");
    }

    #[test]
    fn startup_shared_refused_while_exclusive_held() {
        let tmp = TempDir::new().unwrap();
        let _x = acquire_exclusive_at(tmp.path(), "work").unwrap();
        let err = acquire_shared_at(tmp.path(), "work").unwrap_err();
        assert!(err.to_string().contains("is locked"), "got: {err}");
    }

    #[test]
    fn lock_helpers_validate_the_name() {
        let tmp = TempDir::new().unwrap();
        let err = acquire_exclusive_at(tmp.path(), "../evil").unwrap_err();
        assert!(err.to_string().contains("invalid profile name"), "got: {err}");
        // Validation ran BEFORE any fs op: no locks/ dir, and nothing escaped
        // it (locks/../evil.lock would land at base/evil.lock).
        assert!(!tmp.path().join("locks").exists(), "no locks dir for a bad name");
        assert!(!tmp.path().join("evil.lock").exists(), "no traversal escape");
    }
}
