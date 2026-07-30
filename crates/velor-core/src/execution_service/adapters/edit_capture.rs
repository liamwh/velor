//! Shared file-edit capture, used by every adapter whose agent executes its
//! own file-edit tools out-of-process (Claude, omp).
//!
//! Velor only sees the wire-protocol event stream, not the edit itself. To
//! show the *real* resulting change (not the agent's claimed patch) each
//! adapter snapshots a target file when its edit tool_call is observed —
//! before the edit lands — and this module diffs it against the post-edit
//! contents once the matching tool_result arrives (by which point the edit is
//! on disk).

use std::io::ErrorKind;
use std::path::Path;

use crate::agent::AgentEvent;
use crate::execution_service::adapter::AgentEventSink;
use crate::execution_service::error::AgentExecutionError;
use crate::file_edit::{
    DEFAULT_MAX_DIFF_LINES, FileEdit, FileEditKind, compute_file_edit, infer_syntax,
};

/// On-disk state of a file at a capture point.
pub(super) enum ReadState {
    /// The file did not exist.
    Missing,
    /// The file existed with these bytes.
    Bytes(Vec<u8>),
    /// The file existed but could not be read (e.g. a permission error).
    Unreadable(String),
}

/// Records a pre-edit snapshot, preserving insertion order and ignoring
/// duplicate paths within a single in-flight batch (one diff per file).
pub(super) fn note_pending(pending: &mut Vec<(String, ReadState)>, path: String, state: ReadState) {
    if !pending.iter().any(|(p, _)| p == &path) {
        pending.push((path, state));
    }
}

/// Emits an [`AgentEvent::FileEdit`] for each pending edit, reading the
/// post-edit file state and computing the diff. No-op (and cheap) when
/// nothing is pending.
pub(super) async fn drain_pending_edits(
    cwd: &Path,
    pending: &mut Vec<(String, ReadState)>,
    sink: &mut dyn AgentEventSink,
) -> Result<(), AgentExecutionError> {
    if pending.is_empty() {
        return Ok(());
    }
    let drained = std::mem::take(pending);
    for (path, pre) in drained {
        if let Some(edit) = build_edit(cwd, &path, pre).await {
            sink.emit(AgentEvent::FileEdit { edit })
                .await
                .map_err(|_| AgentExecutionError::Cancelled)?;
        }
    }
    Ok(())
}

/// Builds the [`FileEdit`] for one path from its pre-edit state and the
/// current post-edit state. Returns `None` when the edit made no effective
/// change.
pub(super) async fn build_edit(cwd: &Path, path: &str, pre: ReadState) -> Option<FileEdit> {
    let pre_bytes: Option<Vec<u8>> = match pre {
        ReadState::Bytes(bytes) => Some(bytes),
        ReadState::Missing => None,
        ReadState::Unreadable(ref reason) => {
            return Some(capture_failed(path, reason.clone()));
        }
    };
    match read_file_state(cwd, path).await {
        ReadState::Unreadable(reason) => Some(capture_failed(path, reason)),
        ReadState::Missing => {
            compute_file_edit(path, pre_bytes.as_deref(), None, DEFAULT_MAX_DIFF_LINES)
        }
        ReadState::Bytes(post) => compute_file_edit(
            path,
            pre_bytes.as_deref(),
            Some(&post),
            DEFAULT_MAX_DIFF_LINES,
        ),
    }
}

/// Reads the on-disk state of `path` resolved against `cwd`. Absolute paths
/// override `cwd` (as `PathBuf::join` does).
pub(super) async fn read_file_state(cwd: &Path, path: &str) -> ReadState {
    let resolved = cwd.join(path);
    match tokio::fs::read(&resolved).await {
        Ok(bytes) => ReadState::Bytes(bytes),
        Err(err) if err.kind() == ErrorKind::NotFound => ReadState::Missing,
        Err(err) => ReadState::Unreadable(err.to_string()),
    }
}

/// A [`FileEdit`] recording that the real before/after state could not be
/// captured, so the transcript surfaces the failure rather than silently
/// presenting the agent's claimed patch as the result.
fn capture_failed(path: &str, reason: String) -> FileEdit {
    FileEdit {
        path: path.to_string(),
        syntax: infer_syntax(path),
        kind: FileEditKind::CaptureFailed { reason },
        hunks: Vec::new(),
        omitted_lines: 0,
        // No source to highlight for a capture failure.
        full_new_source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_edit_reports_real_modified_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "src/lib.rs";
        std::fs::create_dir_all(cwd.join("src")).expect("mkdir");
        std::fs::write(cwd.join(path), "before\n").expect("write");
        let pre = read_file_state(cwd, path).await;
        std::fs::write(cwd.join(path), "after\n").expect("write");

        let edit = build_edit(cwd, path, pre).await.expect("an edit");
        assert!(matches!(edit.kind, FileEditKind::Modified));
    }

    #[tokio::test]
    async fn build_edit_reports_creation_and_deletion() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "new.rs";
        let pre = ReadState::Missing;
        std::fs::write(cwd.join(path), "hello\n").expect("write");
        let created = build_edit(cwd, path, pre).await.expect("created edit");
        assert!(matches!(created.kind, FileEditKind::Created));

        std::fs::remove_file(cwd.join(path)).expect("remove");
        let deleted = build_edit(cwd, path, ReadState::Bytes(b"hello\n".to_vec()))
            .await
            .expect("deleted edit");
        assert!(matches!(deleted.kind, FileEditKind::Deleted));
    }

    #[tokio::test]
    async fn build_edit_emits_nothing_for_no_effective_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "unchanged.rs";
        std::fs::write(cwd.join(path), "unchanged\n").expect("write");
        let pre = ReadState::Bytes(b"unchanged\n".to_vec());
        assert!(build_edit(cwd, path, pre).await.is_none());
    }

    #[tokio::test]
    async fn build_edit_reports_capture_failure_when_post_read_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let path = "does-not-exist-and-unreadable.rs";
        let edit = build_edit(cwd, path, ReadState::Unreadable("denied".to_string()))
            .await
            .expect("a capture-failed edit");
        assert!(matches!(edit.kind, FileEditKind::CaptureFailed { .. }));
    }
}
