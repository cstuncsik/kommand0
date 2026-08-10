//! Codex session-store matching for the TUI's early session-id capture.
//!
//! Interactive `codex` prints nothing on SIGTERM, but it writes a rollout file
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<local-ts>-<uuid>.jsonl` at session
//! START, whose first line carries the session meta (id, cwd, UTC timestamp,
//! originator). [`latest_codex_rollout`] finds the newest interactive session
//! started in a given directory at/after a given instant, so a codex tab's id
//! can be persisted while the tab is still running (surviving a kommand0 quit
//! or crash). Format verified against codex 0.145.0; any drift, parse or IO
//! failure is a silent skip (the capture degrades to fresh-open). Panic-free
//! and meant to run off the UI thread.

use std::fs;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, Utc};

/// Codex's on-disk session store: `KOMMAND0_CODEX_SESSIONS_DIR` (set and
/// non-empty; a test seam, mirroring the CLAUDE_CONFIG_DIR resolution) wins,
/// else `~/.codex/sessions`.
pub fn codex_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("KOMMAND0_CODEX_SESSIONS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex").join("sessions")))
}

/// The bare uuid of the newest codex-tui rollout created in `cwd` at/after
/// `since`, or `None` when there is no match.
///
/// Scans yesterday's, today's and tomorrow's date dirs (the filename dates are
/// local time while the meta timestamp is UTC; the spread survives either
/// convention and the midnight boundary). A candidate must: be a
/// `rollout-*.jsonl` whose filename ends in a canonical uuid; have an mtime
/// not before `since` (a cheap prefilter: codex appends to a live rollout, so
/// mtime only advances); and carry a line-1 meta whose `originator` is
/// `"codex-tui"` (`codex exec` and subagent consults share the store, stamped
/// `codex_exec`; without this filter a consult's session could be resumed in
/// the tab's place), whose canonicalized `cwd` matches, and whose `timestamp`
/// is at/after `since` (minus a 2s clock-skew grace; an old same-dir session
/// still appending passes the mtime prefilter but fails this). Newest meta
/// timestamp wins. Two just-started, still-empty sessions in the same
/// directory are genuinely ambiguous; the caller's adopt guards bound the
/// cross-attribution to at most one tab per rollout.
pub fn latest_codex_rollout(sessions_dir: &Path, cwd: &Path, since: SystemTime) -> Option<String> {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let since = since.checked_sub(Duration::from_secs(2)).unwrap_or(since);
    let since_utc = DateTime::<Utc>::from(since);
    let today = Local::now().date_naive();
    let mut best: Option<(DateTime<Utc>, String)> = None;
    for date in [today.pred_opt(), Some(today), today.succ_opt()].into_iter().flatten() {
        let dir = sessions_dir.join(date.format("%Y/%m/%d").to_string());
        let Ok(entries) = fs::read_dir(&dir) else {
            continue; // missing/unreadable date dir: nothing started that day
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".jsonl") else {
                continue;
            };
            if !stem.starts_with("rollout-") {
                continue;
            }
            // The filename uuid (== the meta id) is the last 36 chars; `get`
            // rather than a slice so an odd name can't panic.
            let Some(uuid) = stem.len().checked_sub(36).and_then(|at| stem.get(at..)) else {
                continue;
            };
            if !crate::AppState::is_valid_session_uuid(uuid) {
                continue;
            }
            if entry.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < since) {
                continue;
            }
            let Ok(file) = fs::File::open(entry.path()) else {
                continue;
            };
            // Real codex-tui line 1 is ~15KB (it embeds base_instructions);
            // the generous cap keeps a future format growth from silently
            // killing early capture, while still bounding a pathological file.
            let mut line = String::new();
            if std::io::BufReader::new(file.take(256 * 1024)).read_line(&mut line).is_err() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let meta = v.get("payload").unwrap_or(&v); // tolerate a flat variant
            if meta.get("originator").and_then(|o| o.as_str()) != Some("codex-tui") {
                continue;
            }
            let Some(meta_cwd) = meta.get("cwd").and_then(|c| c.as_str()).map(Path::new) else {
                continue;
            };
            if fs::canonicalize(meta_cwd).unwrap_or_else(|_| meta_cwd.to_path_buf()) != cwd {
                continue;
            }
            let Some(ts) = meta
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc))
            else {
                continue;
            };
            if ts < since_utc {
                continue;
            }
            if best.as_ref().is_none_or(|(bt, _)| ts > *bt) {
                best = Some((ts, uuid.to_string()));
            }
        }
    }
    best.map(|(_, uuid)| uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    const UUID_A: &str = "019faaaa-aaaa-7aaa-aaaa-aaaaaaaaaaaa";
    const UUID_B: &str = "019fbbbb-bbbb-7bbb-bbbb-bbbbbbbbbbbb";

    /// Write a rollout file under `store/<date>/` with a codex-0.145.0-shaped
    /// line 1. Returns nothing; every knob a test discriminates on is a param.
    fn seed_rollout(
        store: &Path,
        date: NaiveDate,
        uuid: &str,
        cwd: &Path,
        originator: &str,
        ts: DateTime<Utc>,
    ) {
        let dir = store.join(date.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&dir).unwrap();
        let line = serde_json::json!({
            "timestamp": ts.to_rfc3339(),
            "type": "session_meta",
            "payload": {
                "id": uuid,
                "timestamp": ts.to_rfc3339(),
                "cwd": cwd.to_str().unwrap(),
                "originator": originator,
                "cli_version": "0.145.0",
            }
        });
        fs::write(
            dir.join(format!("rollout-2026-01-01T00-00-00-{uuid}.jsonl")),
            format!("{line}\n"),
        )
        .unwrap();
    }

    fn ago(secs: i64) -> SystemTime {
        if secs >= 0 {
            SystemTime::now() - Duration::from_secs(secs as u64)
        } else {
            SystemTime::now() + Duration::from_secs((-secs) as u64)
        }
    }

    #[test]
    fn finds_a_fresh_codex_tui_rollout_for_the_cwd() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        seed_rollout(
            store.path(),
            Local::now().date_naive(),
            UUID_A,
            ws.path(),
            "codex-tui",
            Utc::now(),
        );
        assert_eq!(
            latest_codex_rollout(store.path(), ws.path(), ago(5)),
            Some(UUID_A.to_string())
        );
    }

    #[test]
    fn rejects_a_rollout_from_another_cwd() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        seed_rollout(
            store.path(),
            Local::now().date_naive(),
            UUID_A,
            other.path(),
            "codex-tui",
            Utc::now(),
        );
        assert_eq!(latest_codex_rollout(store.path(), ws.path(), ago(5)), None);
    }

    #[test]
    fn rejects_a_codex_exec_rollout() {
        // `codex exec` / subagent consults share the store and can run in the
        // SAME workspace dir; adopting one would resume the wrong conversation.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        seed_rollout(
            store.path(),
            Local::now().date_naive(),
            UUID_A,
            ws.path(),
            "codex_exec",
            Utc::now(),
        );
        assert_eq!(latest_codex_rollout(store.path(), ws.path(), ago(5)), None);
    }

    #[test]
    fn rejects_an_old_session_despite_a_fresh_mtime() {
        // An old same-dir session still appending: the write keeps its mtime
        // fresh (passing the prefilter), but the meta timestamp predates the
        // spawn, so it must not be adopted.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        seed_rollout(
            store.path(),
            Local::now().date_naive(),
            UUID_A,
            ws.path(),
            "codex-tui",
            Utc::now() - chrono::Duration::seconds(600),
        );
        assert_eq!(latest_codex_rollout(store.path(), ws.path(), ago(5)), None);
    }

    #[test]
    fn newest_of_two_matches_wins() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let today = Local::now().date_naive();
        seed_rollout(
            store.path(),
            today,
            UUID_A,
            ws.path(),
            "codex-tui",
            Utc::now() - chrono::Duration::seconds(3),
        );
        seed_rollout(store.path(), today, UUID_B, ws.path(), "codex-tui", Utc::now());
        assert_eq!(
            latest_codex_rollout(store.path(), ws.path(), ago(10)),
            Some(UUID_B.to_string())
        );
    }

    #[test]
    fn a_malformed_line_one_skips_without_killing_the_scan() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let today = Local::now().date_naive();
        let dir = store.path().join(today.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("rollout-2026-01-01T00-00-00-{UUID_B}.jsonl")),
            "not json at all\n",
        )
        .unwrap();
        seed_rollout(store.path(), today, UUID_A, ws.path(), "codex-tui", Utc::now());
        assert_eq!(
            latest_codex_rollout(store.path(), ws.path(), ago(5)),
            Some(UUID_A.to_string()),
            "the garbage sibling is skipped, the valid rollout still matches"
        );
    }

    #[test]
    fn a_missing_store_yields_none() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert_eq!(
            latest_codex_rollout(&store.path().join("nope"), ws.path(), ago(5)),
            None
        );
    }

    #[test]
    fn a_bad_filename_uuid_is_skipped() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let today = Local::now().date_naive();
        let dir = store.path().join(today.format("%Y/%m/%d").to_string());
        fs::create_dir_all(&dir).unwrap();
        // Well-formed meta but a filename whose tail is not a canonical uuid.
        let line = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": UUID_A,
                "timestamp": Utc::now().to_rfc3339(),
                "cwd": ws.path().to_str().unwrap(),
                "originator": "codex-tui",
            }
        });
        fs::write(dir.join("rollout-NOT-A-UUID-AT-ALL-PADDING-TO-36-CHARSx.jsonl"), format!("{line}\n"))
            .unwrap();
        assert_eq!(latest_codex_rollout(store.path(), ws.path(), ago(5)), None);
    }

    #[test]
    fn a_symlinked_cwd_still_matches() {
        // macOS: /tmp is a symlink to /private/tmp, so codex's meta cwd and
        // the workspace's working_dir can disagree textually; both sides are
        // canonicalized before comparing.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let link = store.path().join("ws-link");
        std::os::unix::fs::symlink(ws.path(), &link).unwrap();
        seed_rollout(
            store.path(),
            Local::now().date_naive(),
            UUID_A,
            ws.path(),
            "codex-tui",
            Utc::now(),
        );
        assert_eq!(
            latest_codex_rollout(store.path(), &link, ago(5)),
            Some(UUID_A.to_string()),
            "the symlinked spawn dir canonicalizes onto the meta cwd"
        );
    }

    #[test]
    fn a_rollout_filed_under_yesterday_is_still_found() {
        // Midnight boundary: the file's date dir reflects when codex STAMPED
        // it (local date), which can lag the poll's "today".
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let yesterday = Local::now().date_naive().pred_opt().unwrap();
        seed_rollout(store.path(), yesterday, UUID_A, ws.path(), "codex-tui", Utc::now());
        assert_eq!(
            latest_codex_rollout(store.path(), ws.path(), ago(5)),
            Some(UUID_A.to_string())
        );
    }
}
