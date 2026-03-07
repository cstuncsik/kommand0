use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// Events emitted by background reader tasks for a session.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SessionEvent {
    Output {
        session_id: String,
        line: String,
    },
    Exited {
        session_id: String,
        exit_code: Option<i32>,
    },
    Error {
        session_id: String,
        error: String,
    },
}

/// Internal state for a running child process.
struct RunningSession {
    child: Child,
    stdin: ChildStdin,
    #[allow(dead_code)]
    workspace_id: String,
    claude_session_id: Option<String>,
}

/// Manages spawning, streaming, and lifecycle of Claude CLI child processes.
///
/// Each session is a `claude -p --input-format stream-json --output-format stream-json`
/// process. Output is streamed via mpsc channels and consumed by the TUI event loop
/// through `poll_events()`.
pub struct SessionManager {
    sessions: HashMap<String, RunningSession>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    event_rx: mpsc::UnboundedReceiver<SessionEvent>,
}

#[allow(dead_code)]
impl SessionManager {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            sessions: HashMap::new(),
            event_tx,
            event_rx,
        }
    }

    /// Spawn a new Claude CLI session for the given workspace directory.
    ///
    /// If `resume_id` is provided, passes `--resume <id>` to continue a previous
    /// Claude session. Returns the child process PID.
    pub fn start_session(
        &mut self,
        session_id: &str,
        workspace_dir: &str,
        resume_id: Option<&str>,
    ) -> Result<u32> {
        let mut cmd = Command::new("claude");
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--cwd",
            workspace_dir,
        ]);

        if let Some(rid) = resume_id {
            cmd.args(["--resume", rid]);
        }

        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_remove("CLAUDECODE")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;

        let pid = child
            .id()
            .ok_or_else(|| anyhow!("failed to get child PID"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to take child stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to take child stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to take child stderr"))?;

        // Spawn stdout reader task.
        // When stdout closes (process exited), this task ends and sends an Exited event.
        let tx_stdout = self.event_tx.clone();
        let sid_stdout = session_id.to_string();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(raw_line)) = lines.next_line().await {
                let stripped = strip_ansi_escapes::strip_str(&raw_line);
                let text = extract_text_from_json(&stripped);
                if tx_stdout
                    .send(SessionEvent::Output {
                        session_id: sid_stdout.clone(),
                        line: text,
                    })
                    .is_err()
                {
                    break;
                }
            }
            // Stdout closed -- process has exited
            let _ = tx_stdout.send(SessionEvent::Exited {
                session_id: sid_stdout,
                exit_code: None,
            });
        });

        // Spawn stderr reader task
        let tx_stderr = self.event_tx.clone();
        let sid_stderr = session_id.to_string();
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(raw_line)) = lines.next_line().await {
                let stripped = strip_ansi_escapes::strip_str(&raw_line);
                if tx_stderr
                    .send(SessionEvent::Output {
                        session_id: sid_stderr.clone(),
                        line: stripped.to_string(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        self.sessions.insert(
            session_id.to_string(),
            RunningSession {
                child,
                stdin,
                workspace_id: workspace_dir.to_string(),
                claude_session_id: None,
            },
        );

        Ok(pid)
    }

    /// Send a user message to the session's stdin as stream-json.
    pub async fn send_message(&mut self, session_id: &str, content: &str) -> Result<()> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;

        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content
            }
        });

        session
            .stdin
            .write_all(msg.to_string().as_bytes())
            .await?;
        session.stdin.write_all(b"\n").await?;
        session.stdin.flush().await?;

        Ok(())
    }

    /// Stop a running session by sending SIGTERM to its process group,
    /// falling back to SIGKILL after 5 seconds.
    pub async fn stop_session(&mut self, session_id: &str) -> Result<()> {
        let mut session = self
            .sessions
            .remove(session_id)
            .ok_or_else(|| anyhow!("session not found: {session_id}"))?;

        if let Some(pid) = session.child.id() {
            let pgid = pid as i32;
            let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);

            match tokio::time::timeout(Duration::from_secs(5), session.child.wait()).await {
                Ok(_) => {}
                Err(_) => {
                    let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
                    let _ = session.child.wait().await;
                }
            }
        }

        Ok(())
    }

    /// Restart a session, optionally resuming the previous Claude session.
    ///
    /// Stops the existing session if running, generates a new session ID,
    /// and starts a new process with `--resume` if `claude_session_id` is provided.
    /// Returns `(new_session_id, pid)`.
    pub fn restart_session(
        &mut self,
        session_id: &str,
        workspace_dir: &str,
        claude_session_id: Option<&str>,
    ) -> Result<(String, u32)> {
        // Remove old session if it exists (best-effort stop via drop + kill_on_drop)
        self.sessions.remove(session_id);

        let new_session_id = uuid::Uuid::new_v4().to_string();
        let pid = self.start_session(&new_session_id, workspace_dir, claude_session_id)?;
        Ok((new_session_id, pid))
    }

    /// Shut down all running sessions: SIGTERM all process groups,
    /// wait up to 5 seconds total, then SIGKILL any remaining.
    pub async fn shutdown_all(&mut self) -> Result<()> {
        let mut pids: Vec<(String, u32)> = Vec::new();

        for (sid, session) in &self.sessions {
            if let Some(pid) = session.child.id() {
                let pgid = pid as i32;
                let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);
                pids.push((sid.clone(), pid));
            }
        }

        // Wait up to 5 seconds for all to exit
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        for (sid, _pid) in &pids {
            if let Some(session) = self.sessions.get_mut(sid) {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let _ = tokio::time::timeout(remaining, session.child.wait()).await;
            }
        }

        // SIGKILL any still running
        for (sid, pid) in &pids {
            if let Some(session) = self.sessions.get_mut(sid) {
                if session.child.id().is_some() {
                    let pgid = *pid as i32;
                    let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
                    let _ = session.child.wait().await;
                }
            }
        }

        self.sessions.clear();
        Ok(())
    }

    /// Drain all pending events from the channel.
    ///
    /// Called each tick (250ms) by the TUI event loop to batch-process output.
    pub fn poll_events(&mut self) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    // Check for claude_session_id in Output events
                    if let SessionEvent::Output {
                        ref session_id,
                        ref line,
                    } = event
                    {
                        if let Some(csid) = extract_claude_session_id(line) {
                            if let Some(session) = self.sessions.get_mut(session_id) {
                                if session.claude_session_id.is_none() {
                                    session.claude_session_id = Some(csid);
                                }
                            }
                        }
                    }
                    events.push(event);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        events
    }

    /// Get the Claude session ID (for --resume) if discovered from output.
    pub fn get_claude_session_id(&self, session_id: &str) -> Option<String> {
        self.sessions
            .get(session_id)
            .and_then(|s| s.claude_session_id.clone())
    }

    /// Check if a session is currently tracked (process may still be running).
    pub fn is_running(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }
}

/// Extract text content from a stream-json output line.
///
/// Parses the line as JSON and looks for content in common locations:
/// - `result.content` (final result)
/// - `content` as string (streaming events)
/// - `content` as array of content blocks with `text` fields
/// - `message.content` (assistant message events)
/// - Falls back to the raw line if JSON parsing fails (pitfall 3: graceful fallback).
fn extract_text_from_json(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    match serde_json::from_str::<Value>(trimmed) {
        Ok(val) => {
            // Try result.content (final response)
            if let Some(content) = val
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.as_str())
            {
                return content.to_string();
            }

            // Try content as string (streaming text events)
            if let Some(content) = val.get("content").and_then(|c| c.as_str()) {
                return content.to_string();
            }

            // Try content as array of content blocks
            if let Some(blocks) = val.get("content").and_then(|c| c.as_array()) {
                let texts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect();
                if !texts.is_empty() {
                    return texts.join("");
                }
            }

            // Try message.content for assistant message events
            if let Some(content) = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return content.to_string();
            }

            // Return the raw JSON line as fallback
            trimmed.to_string()
        }
        Err(_) => {
            // Not valid JSON -- treat as raw text (pitfall 3)
            trimmed.to_string()
        }
    }
}

/// Try to extract `session_id` from a JSON output line.
fn extract_claude_session_id(line: &str) -> Option<String> {
    let val: Value = serde_json::from_str(line.trim()).ok()?;
    val.get("session_id")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}
