use std::collections::VecDeque;
use std::sync::Arc;

use dicom_viewer_core::WorkspaceDocument;

const DEFAULT_MAX_COMMANDS: usize = 500;
const DEFAULT_MAX_RETAINED_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug)]
struct HistoryEntry {
    label: String,
    before: Arc<WorkspaceDocument>,
    after: Arc<WorkspaceDocument>,
    retained_bytes: usize,
}

impl HistoryEntry {
    fn new(
        label: impl Into<String>,
        before: Arc<WorkspaceDocument>,
        after: Arc<WorkspaceDocument>,
    ) -> Self {
        let retained_bytes = before
            .estimated_retained_bytes()
            .saturating_add(after.estimated_retained_bytes());
        Self {
            label: label.into(),
            before,
            after,
            retained_bytes,
        }
    }
}

#[derive(Debug)]
pub(super) struct WorkspaceHistory {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
    max_commands: usize,
    max_retained_bytes: usize,
    truncated: bool,
}

impl Default for WorkspaceHistory {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_COMMANDS, DEFAULT_MAX_RETAINED_BYTES)
    }
}

impl WorkspaceHistory {
    pub(super) fn with_limits(max_commands: usize, max_retained_bytes: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            max_commands,
            max_retained_bytes,
            truncated: false,
        }
    }

    pub(super) fn record(
        &mut self,
        label: impl Into<String>,
        before: Arc<WorkspaceDocument>,
        after: Arc<WorkspaceDocument>,
    ) {
        self.redo.clear();
        self.undo.push_back(HistoryEntry::new(label, before, after));
        self.enforce_limits();
    }

    pub(super) fn undo(&mut self) -> Option<Arc<WorkspaceDocument>> {
        let entry = self.undo.pop_back()?;
        let document = Arc::clone(&entry.before);
        self.redo.push_back(entry);
        Some(document)
    }

    pub(super) fn redo(&mut self) -> Option<Arc<WorkspaceDocument>> {
        let entry = self.redo.pop_back()?;
        let document = Arc::clone(&entry.after);
        self.undo.push_back(entry);
        Some(document)
    }

    #[must_use]
    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub(super) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[must_use]
    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub(super) fn undo_label(&self) -> Option<&str> {
        self.undo.back().map(|entry| entry.label.as_str())
    }

    #[must_use]
    pub(super) fn redo_label(&self) -> Option<&str> {
        self.redo.back().map(|entry| entry.label.as_str())
    }

    fn enforce_limits(&mut self) {
        while self.undo.len() > self.max_commands || self.retained_bytes() > self.max_retained_bytes
        {
            if self.undo.pop_front().is_none() {
                break;
            }
            self.truncated = true;
        }
    }

    fn retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|entry| entry.retained_bytes)
            .fold(0usize, usize::saturating_add)
    }
}
