use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// Per-session launch options applied as `claude` flags at spawn time.
/// `None` fields are omitted, preserving the CLI's defaults.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionConfig {
    /// `--model` value (alias like "opus"/"sonnet"/"fable" or a full model id).
    pub model: Option<String>,
    /// `--effort` value ("low"|"medium"|"high"|"xhigh"|"max").
    pub effort: Option<String>,
}

/// Where an Output event originated from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputSource {
    Stdout,
    Stderr,
}

/// Events emitted by background reader tasks for a session.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SessionEvent {
    Output {
        session_id: String,
        line: String,
        source: OutputSource,
    },
    /// A small streaming text chunk from content_block_delta.
    StreamDelta {
        session_id: String,
        text: String,
    },
    /// Streaming content block finished (content_block_stop / message_stop).
    StreamEnd {
        session_id: String,
    },
    Exited {
        session_id: String,
        exit_code: Option<i32>,
    },
    Error {
        session_id: String,
        error: String,
    },
    /// Claude session ID discovered from stream-json output (used for --resume).
    ClaudeSessionId {
        session_id: String,
        claude_session_id: String,
    },
    /// Slash commands advertised by the CLI's system/init event. Names have no
    /// leading '/'. The init list omits interactive-only built-ins (e.g. /model,
    /// /config) that no-op in headless `-p` mode, so it is safe to offer as-is.
    SlashCommands {
        session_id: String,
        commands: Vec<String>,
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
    /// Claude session. `config` adds `--model`/`--effort` flags when set.
    /// Returns the child process PID.
    pub fn start_session(
        &mut self,
        session_id: &str,
        workspace_dir: &str,
        resume_id: Option<&str>,
        config: &SessionConfig,
    ) -> Result<u32> {
        let mut cmd = Command::new("claude");
        cmd.args([
            "-p",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]);
        cmd.args(config_flags(config));
        cmd.current_dir(workspace_dir);

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
            let mut had_deltas = false;
            while let Ok(Some(raw_line)) = lines.next_line().await {
                let stripped = strip_ansi_escapes::strip_str(&raw_line);
                // Try to extract claude session_id from any JSON line
                if let Some(csid) = extract_claude_session_id(&stripped) {
                    let _ = tx_stdout.send(SessionEvent::ClaudeSessionId {
                        session_id: sid_stdout.clone(),
                        claude_session_id: csid,
                    });
                }
                let event = match classify_json_event(&stripped) {
                    JsonEvent::Delta(text) => {
                        had_deltas = true;
                        SessionEvent::StreamDelta {
                            session_id: sid_stdout.clone(),
                            text,
                        }
                    }
                    JsonEvent::StreamEnd => {
                        let was_streaming = had_deltas;
                        had_deltas = false;
                        if was_streaming {
                            SessionEvent::StreamEnd {
                                session_id: sid_stdout.clone(),
                            }
                        } else {
                            continue;
                        }
                    }
                    JsonEvent::Complete(text) => {
                        if text.is_empty() {
                            continue;
                        }
                        // If we already streamed deltas, skip the duplicate complete message
                        if had_deltas {
                            continue;
                        }
                        SessionEvent::Output {
                            session_id: sid_stdout.clone(),
                            line: text,
                            source: OutputSource::Stdout,
                        }
                    }
                    JsonEvent::SlashCommands(commands) => SessionEvent::SlashCommands {
                        session_id: sid_stdout.clone(),
                        commands,
                    },
                    JsonEvent::ErrorMsg(error) => SessionEvent::Error {
                        session_id: sid_stdout.clone(),
                        error,
                    },
                    JsonEvent::Empty => continue,
                };
                if tx_stdout.send(event).is_err() {
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
                        source: OutputSource::Stderr,
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
        config: &SessionConfig,
    ) -> Result<(String, u32)> {
        // Remove old session if it exists (best-effort stop via drop + kill_on_drop)
        self.sessions.remove(session_id);

        let new_session_id = uuid::Uuid::new_v4().to_string();
        let pid = self.start_session(&new_session_id, workspace_dir, claude_session_id, config)?;
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
            if let Some(session) = self.sessions.get_mut(sid)
                && session.child.id().is_some() {
                    let pgid = *pid as i32;
                    let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
                    let _ = session.child.wait().await;
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
                    // Store claude_session_id when discovered
                    if let SessionEvent::ClaudeSessionId {
                        ref session_id,
                        ref claude_session_id,
                    } = event
                        && let Some(session) = self.sessions.get_mut(session_id)
                            && session.claude_session_id.is_none() {
                                session.claude_session_id = Some(claude_session_id.clone());
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

/// Classified JSON event from Claude CLI stream-json output.
#[derive(Debug)]
enum JsonEvent {
    /// Streaming text chunk (content_block_delta).
    Delta(String),
    /// End of a streaming content block.
    StreamEnd,
    /// Complete text from assistant/message/result (may duplicate deltas).
    Complete(String),
    /// Slash commands advertised by the system/init event (names without '/').
    SlashCommands(Vec<String>),
    /// An error event surfaced from the stream (e.g. auth/API failures).
    ErrorMsg(String),
    /// Non-content event or empty.
    Empty,
}

/// Classify a stream-json output line into a JsonEvent.
fn classify_json_event(line: &str) -> JsonEvent {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return JsonEvent::Empty;
    }

    match serde_json::from_str::<Value>(trimmed) {
        Ok(val) => {
            let event_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match event_type {
                "system" => {
                    // The init event advertises the session's dispatchable slash
                    // commands in a flat array of names (no leading '/').
                    if val.get("subtype").and_then(|s| s.as_str()) == Some("init")
                        && let Some(arr) = val.get("slash_commands").and_then(|c| c.as_array())
                    {
                        let cmds: Vec<String> = arr
                            .iter()
                            .filter_map(|c| c.as_str().map(|s| s.to_string()))
                            .collect();
                        if !cmds.is_empty() {
                            return JsonEvent::SlashCommands(cmds);
                        }
                    }
                    JsonEvent::Empty
                }
                "start" | "ping" => JsonEvent::Empty,

                "error" => {
                    // A dedicated error event is not observed in current CLI output
                    // (failures arrive as a `result` with is_error=true, handled
                    // below) but accept it as a secondary catch-all. Shape is
                    // undocumented; try a few plausible layouts, else keep the raw.
                    JsonEvent::ErrorMsg(extract_error_message(&val, trimmed))
                }

                "content_block_delta" => {
                    if let Some(text) = val
                        .get("delta")
                        .and_then(|d| d.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        JsonEvent::Delta(text.to_string())
                    } else {
                        JsonEvent::Empty
                    }
                }

                "content_block_stop" | "message_stop" => JsonEvent::StreamEnd,

                "assistant" | "message" => {
                    if let Some(blocks) = val
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                        .or_else(|| val.get("content").and_then(|c| c.as_array()))
                    {
                        let texts: Vec<&str> = blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect();
                        if !texts.is_empty() {
                            return JsonEvent::Complete(texts.join(""));
                        }
                    }
                    JsonEvent::Empty
                }

                "result" => {
                    // A failed turn arrives as a result with is_error=true and the
                    // detail in the top-level `result` string (the success text is
                    // already surfaced via the preceding assistant event, so only
                    // the error case needs to be turned into output here).
                    let is_error = val.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
                    let subtype = val.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                    let api_error = val.get("api_error_status").and_then(|s| s.as_str());
                    if is_error || subtype.contains("error") || api_error.is_some() {
                        let msg = val
                            .get("result")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| api_error.map(|s| s.to_string()))
                            .unwrap_or_else(|| {
                                if subtype.is_empty() {
                                    "unknown error".to_string()
                                } else {
                                    subtype.to_string()
                                }
                            });
                        return JsonEvent::ErrorMsg(msg);
                    }
                    // result.content as string
                    if let Some(content) = val
                        .get("result")
                        .and_then(|r| r.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        return JsonEvent::Complete(content.to_string());
                    }
                    // result with content blocks array
                    if let Some(blocks) = val
                        .get("result")
                        .and_then(|r| r.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        let texts: Vec<&str> = blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect();
                        if !texts.is_empty() {
                            return JsonEvent::Complete(texts.join(""));
                        }
                    }
                    JsonEvent::Empty
                }

                _ => {
                    // content as string directly
                    if let Some(content) = val.get("content").and_then(|c| c.as_str()) {
                        return JsonEvent::Complete(content.to_string());
                    }
                    JsonEvent::Empty
                }
            }
        }
        Err(_) => {
            // Not valid JSON -- treat as raw text (pitfall 3)
            JsonEvent::Complete(trimmed.to_string())
        }
    }
}

/// Build the `--model`/`--effort` flag args for a session config (empty when
/// both are unset, preserving the CLI defaults).
fn config_flags(config: &SessionConfig) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(model) = &config.model {
        flags.push("--model".to_string());
        flags.push(model.clone());
    }
    if let Some(effort) = &config.effort {
        flags.push("--effort".to_string());
        flags.push(effort.clone());
    }
    flags
}

/// Extract a human-readable message from an error event of unknown shape,
/// falling back to a truncated copy of the raw line so detail is never lost.
fn extract_error_message(val: &Value, raw: &str) -> String {
    val.get("error")
        .and_then(|e| e.as_str())
        .or_else(|| val.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()))
        .or_else(|| val.get("message").and_then(|m| m.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // char-boundary safe truncation (byte slicing could split a codepoint)
            let snippet: String = raw.chars().take(500).collect();
            format!("unrecognized error event: {snippet}")
        })
}

/// Try to extract `session_id` from a JSON output line.
fn extract_claude_session_id(line: &str) -> Option<String> {
    let val: Value = serde_json::from_str(line.trim()).ok()?;
    val.get("session_id")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_event_extracts_slash_commands() {
        let line = r#"{"type":"system","subtype":"init","slash_commands":["compact","context","clear"]}"#;
        match classify_json_event(line) {
            JsonEvent::SlashCommands(cmds) => {
                assert_eq!(cmds, vec!["compact", "context", "clear"]);
            }
            other => panic!("expected SlashCommands, got {other:?}"),
        }
    }

    #[test]
    fn non_init_system_event_is_empty() {
        let line = r#"{"type":"system","subtype":"hook_started","hook_id":"x"}"#;
        assert!(matches!(classify_json_event(line), JsonEvent::Empty));
    }

    #[test]
    fn init_without_slash_commands_is_empty() {
        let line = r#"{"type":"system","subtype":"init","session_id":"s1"}"#;
        assert!(matches!(classify_json_event(line), JsonEvent::Empty));
    }

    #[test]
    fn error_event_extracts_string_message() {
        let line = r#"{"type":"error","error":"boom"}"#;
        match classify_json_event(line) {
            JsonEvent::ErrorMsg(m) => assert_eq!(m, "boom"),
            other => panic!("expected ErrorMsg, got {other:?}"),
        }
    }

    #[test]
    fn error_event_extracts_nested_message() {
        let line = r#"{"type":"error","error":{"message":"nested boom"}}"#;
        match classify_json_event(line) {
            JsonEvent::ErrorMsg(m) => assert_eq!(m, "nested boom"),
            other => panic!("expected ErrorMsg, got {other:?}"),
        }
    }

    #[test]
    fn error_event_unknown_shape_keeps_raw() {
        let line = r#"{"type":"error","unexpected":123}"#;
        match classify_json_event(line) {
            JsonEvent::ErrorMsg(m) => {
                assert!(m.starts_with("unrecognized error event:"), "got {m}");
                assert!(m.contains("unexpected"));
            }
            other => panic!("expected ErrorMsg, got {other:?}"),
        }
    }

    #[test]
    fn result_with_is_error_becomes_error_message() {
        // The real-world failure shape: a result event, not a type:"error" event.
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"API failure: overloaded"}"#;
        match classify_json_event(line) {
            JsonEvent::ErrorMsg(m) => assert_eq!(m, "API failure: overloaded"),
            other => panic!("expected ErrorMsg, got {other:?}"),
        }
    }

    #[test]
    fn successful_result_is_empty_not_duplicated() {
        // The assistant event already surfaced the text; a success result must
        // not re-emit it as Complete.
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"pong"}"#;
        assert!(matches!(classify_json_event(line), JsonEvent::Empty));
    }

    #[test]
    fn api_error_status_without_is_error_still_errors() {
        let line = r#"{"type":"result","subtype":"success","api_error_status":"overloaded_error","result":"partial"}"#;
        match classify_json_event(line) {
            JsonEvent::ErrorMsg(m) => assert_eq!(m, "partial"),
            other => panic!("expected ErrorMsg, got {other:?}"),
        }
    }

    #[test]
    fn config_flags_omitted_by_default() {
        assert!(config_flags(&SessionConfig::default()).is_empty());
    }

    #[test]
    fn config_flags_includes_set_values() {
        let cfg = SessionConfig {
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        assert_eq!(
            config_flags(&cfg),
            vec!["--model", "opus", "--effort", "high"]
        );
    }

    #[test]
    fn config_flags_partial() {
        let cfg = SessionConfig { model: Some("sonnet".into()), effort: None };
        assert_eq!(config_flags(&cfg), vec!["--model", "sonnet"]);
    }

    #[test]
    fn assistant_text_is_complete() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"pong"}]}}"#;
        match classify_json_event(line) {
            JsonEvent::Complete(t) => assert_eq!(t, "pong"),
            other => panic!("expected Complete, got {other:?}"),
        }
    }
}
