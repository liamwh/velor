//! Structured JSONL run logging — one file per `vel auto` invocation.
//!
//! Events (agent output, tool calls, retries, lifecycle) are written as
//! newline-delimited JSON to `.velor/logs/<timestamp>-<prompt>.jsonl`.
//! Files older than 7 days or exceeding a 10 GB total are rotated on startup.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use serde::Serialize;
use serde_json::json;

const MAX_AGE_DAYS: u64 = 7;
const MAX_TOTAL_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GB
const LOG_DIR_NAME: &str = "logs";

/// A structured run logger that writes JSONL events to a file.
pub struct RunLogger {
    file: Mutex<Option<std::fs::File>>,
    run_id: String,
    path: std::path::PathBuf,
}

#[derive(Serialize)]
struct LogEntry {
    ts: chrono::DateTime<Utc>,
    run_id: String,
    #[serde(flatten)]
    data: serde_json::Value,
}

impl RunLogger {
    /// Creates a new logger, writing to `<git_root>/.velor/logs/<ts>-<prompt>.jsonl`.
    /// Rotates old log files on creation.
    #[must_use]
    pub fn new(git_root: &std::path::Path, prompt_name: &str) -> Self {
        let log_dir = git_root.join(".velor").join(LOG_DIR_NAME);
        let _ = fs::create_dir_all(&log_dir);
        rotate_logs(&log_dir);

        let ts = Utc::now().format("%Y%m%dT%H%M%S");
        let safe_name =
            prompt_name.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        let filename = format!("{ts}-{safe_name}.jsonl");
        let path = log_dir.join(&filename);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();

        let run_id = format!("{ts}-{safe_name}");

        let logger = Self {
            file: Mutex::new(file),
            run_id: run_id.clone(),
            path: path.clone(),
        };

        logger.log(
            "lifecycle",
            json!({
                "event": "run_started",
                "log_file": path.to_string_lossy(),
            }),
        );

        logger
    }

    /// Logs an event with a category and arbitrary JSON data.
    pub fn log(&self, category: &str, data: serde_json::Value) {
        let mut merged = data;
        if let Some(obj) = merged.as_object_mut() {
            obj.insert("category".to_string(), json!(category));
        } else {
            merged = json!({ "category": category, "data": merged });
        }
        let entry = LogEntry {
            ts: Utc::now(),
            run_id: self.run_id.clone(),
            data: merged,
        };

        if let Ok(line) = serde_json::to_string(&entry) {
            if let Ok(mut guard) = self.file.lock() {
                if let Some(file) = guard.as_mut() {
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }
            }
        }
    }

    /// Logs an [`AgentEvent`](velor_core::agent::AgentEvent).
    pub fn log_agent_event(&self, event: &velor_core::agent::AgentEvent) {
        use velor_core::agent::AgentEvent;
        match event {
            AgentEvent::TextDelta { text } if text.is_empty() => {}
            AgentEvent::TextDelta { text } if text.starts_with("💭 ") => {
                self.log("thinking", json!({ "text": truncate(&text[3..], 2000) }));
            }
            AgentEvent::TextDelta { text } => {
                self.log("text", json!({ "text": truncate(text, 2000) }));
            }
            AgentEvent::ToolCall {
                tool,
                detail,
                input,
            } => {
                self.log(
                    "tool_call",
                    json!({
                        "tool": tool,
                        "detail": detail,
                        "input": input,
                    }),
                );
            }
            AgentEvent::ToolResult {
                tool,
                detail,
                success,
            } => {
                self.log(
                    "tool_result",
                    json!({
                        "tool": tool,
                        "detail": truncate(detail, 4000),
                        "success": success,
                    }),
                );
            }
            // Suppress internal session/thread metadata — it's noisy and not
            // useful in the log (Claude Code emits dozens of system events per
            // turn, all with the same session ID).
            AgentEvent::Status { message } if message.starts_with("session: ") => {}
            AgentEvent::Status { message } if message.starts_with("thread started: ") => {}
            AgentEvent::Status { message } => {
                self.log("status", json!({ "message": message }));
            }
            AgentEvent::Error { message } => {
                self.log("error", json!({ "message": truncate(message, 2000) }));
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
            } => {
                self.log(
                    "usage",
                    json!({
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "cached_input_tokens": cached_input_tokens,
                    }),
                );
            }
        }
    }

    /// Logs a retry decision.
    pub fn log_retry(
        &self,
        attempt: u32,
        max_retries: u32,
        delay_secs: f64,
        error: &str,
        classification: &str,
    ) {
        self.log(
            "retry",
            json!({
                "attempt": attempt,
                "max_retries": max_retries,
                "delay_secs": (delay_secs * 10.0).round() / 10.0,
                "error": truncate(error, 1000),
                "classification": classification,
            }),
        );
    }

    /// Logs a permanent failure (no more retries).
    pub fn log_permanent_failure(&self, attempt: u32, error: &str) {
        self.log(
            "permanent_failure",
            json!({
                "attempt": attempt,
                "error": truncate(error, 2000),
            }),
        );
    }

    /// Returns the path to the log file.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Logs the final outcome.
    pub fn log_outcome(&self, status: &str, iterations: u32, duration_secs: u64) {
        self.log(
            "lifecycle",
            json!({
                "event": "run_finished",
                "status": status,
                "iterations_completed": iterations,
                "duration_secs": duration_secs,
            }),
        );
    }
}

/// Truncates a string for logging, keeping the head.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

/// Rotates log files: deletes files older than MAX_AGE_DAYS, and if the total
/// directory size exceeds MAX_TOTAL_BYTES, deletes the oldest files until under.
fn rotate_logs(log_dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };

    let mut files: Vec<(PathBuf, SystemTime, u64)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = metadata.modified().unwrap_or(SystemTime::now());
        let size = metadata.len();
        files.push((path, modified, size));
    }

    // Sort by modified time (oldest first).
    files.sort_by_key(|(_, modified, _)| *modified);

    let cutoff = SystemTime::now() - Duration::from_secs(MAX_AGE_DAYS * 24 * 3600);
    let mut total_size: u64 = files.iter().map(|(_, _, sz)| sz).sum();

    for (path, modified, size) in &files {
        let should_delete = *modified < cutoff || total_size > MAX_TOTAL_BYTES;
        if should_delete {
            let _ = fs::remove_file(path);
            total_size = total_size.saturating_sub(*size);
        }
    }
}
